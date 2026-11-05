//! Background connection to the laser.
//!
//! A worker thread owns the serial port: it polls status and, when asked,
//! sends a job and pause / resume / stop commands. The UI reads a shared
//! snapshot and talks to the worker through a command queue, so the port
//! is only ever used from one place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ruida::link::{StatusLink, Transport};
use ruida::memory::MachineStatus;
use ruida::scramble::MAGIC_644X;
use ruida::sender::JobSender;
use ruida::serial::SerialTransport;

use crate::{logln, t};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_FAILURES: u32 = 3;
/// Idle polls after being busy before a job counts as finished.
const IDLE_TO_FINISH: u32 = 4;
/// How long to wait for the machine to get busy after sending.
const START_TIMEOUT: Duration = Duration::from_secs(6);
/// After the port vanishes (USB adapters near a laser drop out now and
/// then), how long to keep trying to open it again, and how often.
const RECONNECT_FOR: Duration = Duration::from_secs(60);
const RECONNECT_EVERY: Duration = Duration::from_millis(500);
/// When Fokus quits, how long to wait for the worker to get a Stop out.
const QUIT_PATIENCE: Duration = Duration::from_secs(3);
/// How often the log looks for changes in the connection and the job.
const LOG_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobPhase {
    Sending,
    Running,
    Paused,
    Finished,
    Stopped,
    Failed(String),
}

impl JobPhase {
    pub fn active(&self) -> bool {
        matches!(self, JobPhase::Sending | JobPhase::Running | JobPhase::Paused)
    }
}

/// What a sent job is for; the laser only fires for `Job`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Job,
    /// The whole job with the laser off, to check the path.
    DryRun,
    /// The box around the design with the laser off, to check the position.
    Frame,
}

#[derive(Debug, Clone)]
pub struct JobProgress {
    pub phase: JobPhase,
    pub sent: usize,
    pub total: usize,
    pub started: Instant,
    pub ended: Option<Instant>,
    /// Expected laser time, for the progress bar.
    pub estimate_s: f64,
    pub kind: JobKind,
    /// Time spent paused so far, and since when it is paused now.
    pub paused_total: Duration,
    pub paused_since: Option<Instant>,
    /// Something the user should know about this run (e.g. the connection
    /// dropped while it ran, so the end is a guess).
    pub note: Option<String>,
}

impl JobProgress {
    fn new(total: usize, estimate_s: f64, kind: JobKind) -> Self {
        JobProgress { phase: JobPhase::Sending, sent: 0, total, started: Instant::now(), ended: None, estimate_s, kind, paused_total: Duration::ZERO, paused_since: None, note: None }
    }

    /// Time the job has been working (pauses don't count).
    pub fn elapsed(&self) -> Duration {
        let end = self.ended.unwrap_or_else(Instant::now);
        let paused_now = self.paused_since.map(|p| end.saturating_duration_since(p)).unwrap_or_default();
        end.saturating_duration_since(self.started).saturating_sub(self.paused_total + paused_now)
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub conn: Conn,
    pub port: Option<String>,
    pub status: Option<MachineStatus>,
    pub updated: Option<Instant>,
    pub job: Option<JobProgress>,
}

impl Snapshot {
    pub fn job_active(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.phase.active())
    }
}

pub enum Cmd {
    Send { plain: Vec<u8>, estimate_s: f64, kind: JobKind },
    Pause,
    Resume,
    Stop,
}

pub struct Machine {
    shared: Arc<Mutex<Snapshot>>,
    stop: Option<Arc<AtomicBool>>,
    tx: Option<Sender<Cmd>>,
    worker: Option<JoinHandle<()>>,
}

impl Machine {
    pub fn new() -> Self {
        let snapshot = Snapshot { conn: Conn::Disconnected, port: None, status: None, updated: None, job: None };
        let shared = Arc::new(Mutex::new(snapshot));
        log_changes(Arc::downgrade(&shared));
        Machine { shared, stop: None, tx: None, worker: None }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().unwrap().clone()
    }

    pub fn connect(&mut self, port: String, ctx: eframe::egui::Context) {
        let open = {
            let port = port.clone();
            move || SerialTransport::open(&port, true).map_err(|e| t!("err-cannot-open-port", port = port.clone(), error = e.to_string()))
        };
        self.start_worker(Some(port), open, move || ctx.request_repaint());
    }

    /// Start the worker thread on whatever `open` connects to (a serial
    /// port, or a simulated controller in tests).
    fn start_worker<T: Transport + 'static>(&mut self, port: Option<String>, open: impl Fn() -> Result<T, String> + Send + 'static, repaint: impl Fn() + Send + 'static) {
        self.disconnect();
        {
            let mut s = self.shared.lock().unwrap();
            s.conn = Conn::Connecting;
            s.port = port;
            s.status = None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        self.stop = Some(stop.clone());
        self.tx = Some(tx);
        let shared = self.shared.clone();
        self.worker = Some(thread::spawn(move || {
            let result = run(open, &shared, &stop, &rx, &repaint);
            let mut s = shared.lock().unwrap();
            s.conn = match result {
                Ok(()) => Conn::Disconnected,
                Err(e) => Conn::Failed(e),
            };
            if let Some(job) = s.job.as_mut().filter(|j| j.phase.active()) {
                // The controller has the whole job and carries on without us.
                job.phase = JobPhase::Failed(t!("err-connection-lost-job"));
                job.ended = Some(Instant::now());
            }
            repaint();
        }));
    }

    /// Close the connection. A job that is still being sent or running is
    /// stopped first: without the connection there would be no Stop button.
    pub fn disconnect(&mut self) {
        if let Some(tx) = self.tx.take() {
            if self.shared.lock().unwrap().job_active() {
                // The worker handles queued commands before it notices the
                // closed queue, so this Stop still goes out.
                let _ = tx.send(Cmd::Stop);
            }
        }
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.shared.lock().unwrap().conn = Conn::Disconnected;
    }

    /// Send a job; it starts at the head as soon as it arrives.
    pub fn start_job(&self, plain: Vec<u8>, estimate_s: f64, kind: JobKind) -> Result<(), String> {
        let snap = self.snapshot();
        if snap.conn != Conn::Connected {
            return Err(t!("err-not-connected"));
        }
        if snap.job_active() {
            return Err(t!("err-job-running"));
        }
        if snap.status.as_ref().is_some_and(|s| s.state() != "idle") {
            return Err(t!("err-laser-busy"));
        }
        let tx = self.tx.as_ref().ok_or_else(|| t!("err-not-connected"))?;
        logln!("Job: sending {kind:?}, {} bytes, about {estimate_s:.0} s", plain.len());
        set_job(&self.shared, Some(JobProgress::new(plain.len(), estimate_s, kind)));
        tx.send(Cmd::Send { plain, estimate_s, kind }).map_err(|_| t!("err-connection-lost"))
    }

    pub fn pause_job(&self) {
        self.command(Cmd::Pause);
    }

    pub fn resume_job(&self) {
        self.command(Cmd::Resume);
    }

    pub fn stop_job(&self) {
        self.command(Cmd::Stop);
    }

    /// Forget a finished / stopped job (the progress card goes away).
    pub fn clear_job(&self) {
        let mut s = self.shared.lock().unwrap();
        if !s.job_active() {
            s.job = None;
        }
    }

    fn command(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.disconnect();
        // Fokus is quitting: give the worker a moment to get the Stop to the
        // laser before the process (and the thread with it) ends.
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + QUIT_PATIENCE;
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Write what changes in the connection and the job to the log, on a thread
/// of its own that only reads: it can't get in the way of a running job. Ends
/// with the machine.
fn log_changes(shared: std::sync::Weak<Mutex<Snapshot>>) {
    thread::spawn(move || {
        let (mut conn, mut port, mut phase, mut note) = (Conn::Disconnected, None, None, None);
        let mut known_card = None;
        while let Some(shared) = shared.upgrade() {
            let s = shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
            drop(shared);
            if s.conn != conn || s.port != port {
                logln!("Connection: {:?} on {}", s.conn, s.port.as_deref().unwrap_or("no port"));
                (conn, port) = (s.conn, s.port);
            }
            if let Some(st) = s.status.as_ref().filter(|st| known_card != Some(st.card_id)) {
                logln!("Laser: card {:#x}, bed {:.0} × {:.0} mm, {}", st.card_id, st.bed.0, st.bed.1, st.state());
                known_card = Some(st.card_id);
            }
            let (now, now_note) = (s.job.as_ref().map(|j| j.phase.clone()), s.job.as_ref().and_then(|j| j.note.clone()));
            if now != phase {
                if let (Some(p), Some(j)) = (&now, &s.job) {
                    logln!("Job: {p:?} after {:.0} s, {} of {} bytes sent", j.elapsed().as_secs_f64(), j.sent, j.total);
                }
                phase = now;
            }
            if now_note != note {
                if let Some(n) = &now_note {
                    logln!("Job: {n}");
                }
                note = now_note;
            }
            thread::sleep(LOG_INTERVAL);
        }
    });
}

fn set_job(shared: &Mutex<Snapshot>, job: Option<JobProgress>) {
    shared.lock().unwrap().job = job;
}

fn update_job(shared: &Mutex<Snapshot>, f: impl FnOnce(&mut JobProgress)) {
    if let Some(job) = shared.lock().unwrap().job.as_mut() {
        f(job);
    }
}

fn end_job(shared: &Mutex<Snapshot>, phase: JobPhase) {
    update_job(shared, |j| {
        j.phase = phase;
        j.ended = Some(Instant::now());
    });
}

/// Tracks whether a sent job has started and finished.
#[derive(Default)]
struct Watch {
    seen_busy: bool,
    idle_polls: u32,
    /// The connection dropped while the job ran, so we may have missed
    /// its start or end.
    lost: bool,
    /// A Stop could not be written; send it again as soon as we can.
    stop_pending: bool,
}

/// Open the port and serve it; if it is lost, keep reopening it for
/// [`RECONNECT_FOR`] and carry on following a running job. Generic over
/// the transport so it can be tested against a simulated controller.
pub(crate) fn run<T: Transport>(open: impl Fn() -> Result<T, String>, shared: &Mutex<Snapshot>, stop: &AtomicBool, rx: &Receiver<Cmd>, repaint: &dyn Fn()) -> Result<(), String> {
    let mut watch = Watch::default();
    let mut transport = open()?;
    loop {
        let error = match serve(StatusLink::new(transport, MAGIC_644X), shared, stop, rx, repaint, &mut watch) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let lost_at = Instant::now();
        {
            let mut s = shared.lock().unwrap();
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            s.conn = Conn::Connecting;
            if s.job_active() {
                watch.lost = true;
            }
        }
        repaint();
        transport = loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if lost_at.elapsed() > RECONNECT_FOR {
                return Err(error);
            }
            thread::sleep(RECONNECT_EVERY);
            if let Ok(t) = open() {
                break t;
            }
        };
    }
}

/// Poll status and run commands until the link fails or we are stopped.
fn serve<T: Transport>(mut link: StatusLink<T>, shared: &Mutex<Snapshot>, stop: &AtomicBool, rx: &Receiver<Cmd>, repaint: &dyn Fn(), watch: &mut Watch) -> Result<(), String> {
    link.timeout = Duration::from_millis(500);
    let mut status = link.status().map_err(|e| t!("err-no-answer", error = e.to_string()))?;
    track_job(shared, &status, watch);
    publish(shared, repaint, &status, stop);

    let mut failures = 0;
    loop {
        if watch.stop_pending {
            link = handle(Cmd::Stop, link, shared, rx, repaint, watch);
        }
        // Commands first, so a Stop queued by a disconnect still goes out.
        loop {
            match rx.try_recv() {
                Ok(cmd) => link = handle(cmd, link, shared, rx, repaint, watch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
        match link.poll(&mut status) {
            Ok(()) => {
                failures = 0;
                track_job(shared, &status, watch);
                publish(shared, repaint, &status, stop);
            }
            Err(e) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return Err(t!("err-lost-connection", error = e.to_string()));
                }
            }
        }
    }
}

fn handle<T: Transport>(cmd: Cmd, link: StatusLink<T>, shared: &Mutex<Snapshot>, rx: &Receiver<Cmd>, repaint: &dyn Fn(), watch: &mut Watch) -> StatusLink<T> {
    let mut sender = JobSender::new(link.into_transport(), MAGIC_644X);
    match cmd {
        Cmd::Send { plain, estimate_s, kind } => {
            set_job(shared, Some(JobProgress::new(plain.len(), estimate_s, kind)));
            let mut stop_requested = false;
            let result = sender.send_job(&plain, |sent, _| {
                update_job(shared, |j| j.sent = sent);
                repaint();
                // Only Stop matters while sending. A closed queue means
                // Fokus disconnected or quit: stop too, rather than leave
                // the laser running a job nobody can stop from here.
                loop {
                    match rx.try_recv() {
                        Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => {
                            stop_requested = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(TryRecvError::Empty) => break,
                    }
                }
                !stop_requested
            });
            match result {
                Ok(true) => {
                    *watch = Watch::default();
                    update_job(shared, |j| j.phase = JobPhase::Running);
                }
                Ok(false) => send_stop(&mut sender, shared, watch),
                Err(e) => {
                    // What was sent may be running; try to stop it, now or
                    // after reconnecting.
                    watch.stop_pending = sender.stop().is_err();
                    end_job(shared, JobPhase::Failed(t!("err-sending-failed", error = e.to_string())));
                }
            }
        }
        Cmd::Pause => {
            if sender.pause().is_ok() {
                update_job(shared, |j| {
                    if j.phase == JobPhase::Running {
                        j.phase = JobPhase::Paused;
                        j.paused_since = Some(Instant::now());
                    }
                });
            }
        }
        Cmd::Resume => {
            if sender.resume().is_ok() {
                update_job(shared, |j| {
                    if j.phase == JobPhase::Paused {
                        j.phase = JobPhase::Running;
                        if let Some(p) = j.paused_since.take() {
                            j.paused_total += p.elapsed();
                        }
                    }
                });
            }
        }
        Cmd::Stop => send_stop(&mut sender, shared, watch),
    }
    repaint();
    let mut link = StatusLink::new(sender.into_transport(), MAGIC_644X);
    link.timeout = Duration::from_millis(500);
    link
}

/// Send Stop. Only a Stop that was written ends the job; if it can't be
/// written, the job stays active (so does the Stop button), the user is told
/// to stop the machine itself, and [`serve`] keeps trying.
fn send_stop<T: Transport>(sender: &mut JobSender<T>, shared: &Mutex<Snapshot>, watch: &mut Watch) {
    match sender.stop() {
        Ok(()) => {
            watch.stop_pending = false;
            update_job(shared, |j| {
                if j.phase.active() {
                    j.phase = JobPhase::Stopped;
                    j.ended = Some(Instant::now());
                    j.note = None;
                }
            });
        }
        Err(_) => {
            update_job(shared, |j| {
                if j.phase.active() {
                    if j.phase == JobPhase::Sending {
                        // Part of the job is on the machine and may run:
                        // follow it like a running job.
                        j.phase = JobPhase::Running;
                        *watch = Watch::default();
                    }
                    j.note = Some(t!("err-stop-not-sent"));
                }
            });
            watch.stop_pending = true;
        }
    }
}

/// Move a running job to Finished once the machine was busy and is idle
/// again, or to Failed if it never got busy.
fn track_job(shared: &Mutex<Snapshot>, status: &MachineStatus, watch: &mut Watch) {
    let mut s = shared.lock().unwrap();
    let Some(job) = s.job.as_mut().filter(|j| j.phase == JobPhase::Running) else { return };
    if status.state() == "idle" {
        watch.idle_polls += 1;
        if (watch.seen_busy || watch.lost) && watch.idle_polls >= IDLE_TO_FINISH {
            // After a dropout the machine may have finished (or never
            // started) while we couldn't see it.
            if watch.lost {
                job.note = Some(t!("job-connection-gap"));
            }
            job.phase = JobPhase::Finished;
            job.ended = Some(Instant::now());
        } else if !watch.seen_busy && job.started.elapsed() > START_TIMEOUT {
            job.phase = JobPhase::Failed(t!("err-job-not-started"));
            job.ended = Some(Instant::now());
        }
    } else {
        watch.seen_busy = true;
        watch.idle_polls = 0;
    }
}

fn publish(shared: &Mutex<Snapshot>, repaint: &dyn Fn(), status: &MachineStatus, stop: &AtomicBool) {
    let mut s = shared.lock().unwrap();
    // A disconnect may have happened while we were reading; don't undo it.
    if stop.load(Ordering::Relaxed) {
        return;
    }
    s.conn = Conn::Connected;
    s.status = Some(status.clone());
    s.updated = Some(Instant::now());
    repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruida::codec::encode_u35;
    use ruida::memory;
    use ruida::scramble::{scramble, unscramble};
    use std::collections::VecDeque;
    use std::io;

    /// A controller that answers memory reads and "runs" a job for a few
    /// status polls after it receives one.
    #[derive(Default)]
    struct Sim {
        out: VecDeque<u8>,
        received: Vec<u8>,
        busy_polls: u32,
        stopped: bool,
        /// The USB adapter has dropped off: every read and write fails.
        gone: bool,
        /// Time each job chunk takes to go out (flow control).
        chunk_time: Duration,
    }

    #[derive(Clone, Default)]
    struct SimHandle(Arc<Mutex<Sim>>);

    impl Transport for SimHandle {
        fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
            let plain = unscramble(data, MAGIC_644X);
            let mut sim = self.0.lock().unwrap();
            if sim.gone {
                return Err(io::Error::from_raw_os_error(6)); // ENXIO, as seen on macOS
            }
            if let [0xDA, 0x00, hi, lo] = plain[..] {
                let addr = u16::from_be_bytes([hi, lo]);
                let value = match addr {
                    memory::MACHINE_STATUS if sim.busy_polls > 0 => {
                        sim.busy_polls -= 1;
                        memory::STATUS_JOB_RUNNING as u64
                    }
                    memory::BED_SIZE_X => 1_000_000,
                    memory::BED_SIZE_Y => 600_000,
                    _ => 0,
                };
                let mut reply = vec![0xDA, 0x01, hi, lo];
                reply.extend(encode_u35(value));
                sim.out.extend(scramble(&reply, MAGIC_644X));
                return Ok(());
            }
            if !sim.chunk_time.is_zero() {
                let wait = sim.chunk_time;
                drop(sim);
                thread::sleep(wait);
                sim = self.0.lock().unwrap();
            }
            if plain.windows(2).any(|w| w == [0xD8, 0x00]) {
                sim.busy_polls = 5;
            }
            if plain == [0xD8, 0x01] {
                sim.busy_polls = 0;
                sim.stopped = true;
            }
            sim.received.extend(plain);
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut sim = self.0.lock().unwrap();
            if sim.gone {
                return Err(io::Error::from_raw_os_error(6));
            }
            let n = buf.len().min(sim.out.len());
            if n == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            for b in buf.iter_mut().take(n) {
                *b = sim.out.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    fn start() -> (SimHandle, Arc<Mutex<Snapshot>>, Arc<AtomicBool>, Sender<Cmd>) {
        let sim = SimHandle::default();
        let shared = Arc::new(Mutex::new(Snapshot { conn: Conn::Connecting, port: None, status: None, updated: None, job: None }));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (s, sh, st) = (sim.clone(), shared.clone(), stop.clone());
        let open = move || if s.0.lock().unwrap().gone { Err("gone".to_string()) } else { Ok(s.clone()) };
        thread::spawn(move || run(open, &sh, &st, &rx, &|| {}));
        (sim, shared, stop, tx)
    }

    fn wait_for(shared: &Mutex<Snapshot>, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = shared.lock().unwrap().clone();
            if pred(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out; last snapshot {s:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn job() -> Vec<u8> {
        // Start process, a few moves, end of file.
        let mut plain = vec![0xD8, 0x12, 0xD8, 0x00];
        for _ in 0..500 {
            plain.extend([0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        plain.push(0xD7);
        plain
    }

    #[test]
    fn job_is_sent_runs_and_finishes() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        let j = s.job.unwrap();
        assert_eq!(j.phase, JobPhase::Finished);
        assert_eq!(j.sent, j.total);
        assert!(sim.0.lock().unwrap().received.ends_with(&[0xD7]), "the whole job arrived");
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn stop_ends_a_running_job() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        tx.send(Cmd::Stop).unwrap();
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert_eq!(s.job.unwrap().phase, JobPhase::Stopped);
        assert!(sim.0.lock().unwrap().stopped, "D8 01 reached the controller");
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_job_the_machine_never_starts_fails() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        // No start-process command: the simulated machine stays idle.
        tx.send(Cmd::Send { plain: vec![0xD8, 0x12, 0xD7], estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert!(matches!(s.job.unwrap().phase, JobPhase::Failed(_)));
        assert!(!sim.0.lock().unwrap().stopped);
        stop.store(true, Ordering::Relaxed);
    }

    /// Unplug the simulated adapter, wait until we notice, then plug it back in.
    fn drop_out(sim: &SimHandle, shared: &Mutex<Snapshot>, while_gone: impl FnOnce(&mut Sim)) {
        sim.0.lock().unwrap().gone = true;
        wait_for(shared, |s| s.conn == Conn::Connecting);
        let mut s = sim.0.lock().unwrap();
        while_gone(&mut s);
        s.out.clear();
        s.gone = false;
    }

    #[test]
    fn a_job_keeps_being_followed_through_a_dropout() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        drop_out(&sim, &shared, |s| s.busy_polls = 3);
        let s = wait_for(&shared, |s| s.conn == Conn::Connected);
        assert_eq!(s.job.as_ref().unwrap().phase, JobPhase::Running, "still running after reconnecting");
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        let j = s.job.unwrap();
        assert_eq!(j.phase, JobPhase::Finished);
        assert!(j.note.is_some(), "the user is told the connection dropped");
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_job_that_ends_during_a_dropout_counts_as_finished() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        drop_out(&sim, &shared, |s| s.busy_polls = 0);
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        let j = s.job.unwrap();
        assert_eq!(j.phase, JobPhase::Finished);
        assert!(j.note.is_some());
        assert_eq!(s.conn, Conn::Connected);
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_stop_that_does_not_get_through_is_not_reported_as_stopped() {
        let (sim, shared, stop, tx) = start();
        wait_for(&shared, |s| s.conn == Conn::Connected);
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, kind: JobKind::Job }).unwrap();
        wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        {
            let mut s = sim.0.lock().unwrap();
            s.busy_polls = 1000;
            s.gone = true;
        }
        tx.send(Cmd::Stop).unwrap();
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| j.note.is_some()));
        let j = s.job.unwrap();
        assert!(j.phase.active(), "still active, with the Stop button: {:?}", j.phase);
        assert_eq!(j.note, Some(t!("err-stop-not-sent")));
        // Plug back in: the Stop is sent again and only then counts.
        drop_out(&sim, &shared, |_| {});
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert_eq!(s.job.unwrap().phase, JobPhase::Stopped);
        assert!(sim.0.lock().unwrap().stopped, "D8 01 reached the controller");
        stop.store(true, Ordering::Relaxed);
    }

    /// A Machine whose worker talks to a simulated controller.
    fn machine(sim: &SimHandle) -> Machine {
        let mut m = Machine::new();
        let s = sim.clone();
        m.start_worker(None, move || if s.0.lock().unwrap().gone { Err("gone".to_string()) } else { Ok(s.clone()) }, || {});
        wait_for(&m.shared, |s| s.conn == Conn::Connected);
        m
    }

    fn big_job() -> Vec<u8> {
        let mut plain = vec![0xD8, 0x12, 0xD8, 0x00];
        for _ in 0..5000 {
            plain.extend([0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        plain.push(0xD7);
        plain
    }

    #[test]
    fn the_log_tells_what_happened_to_the_connection_and_the_job() {
        let dir = std::env::temp_dir().join(format!("fokus-machine-log-{}", std::process::id()));
        let path = dir.join("fokus.log");
        crate::logfile::log_to(path.clone());
        let sim = SimHandle::default();
        let m = machine(&sim);
        m.start_job(job(), 1.0, JobKind::Job).unwrap();
        wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Finished));
        // The log looks for changes a few times a second, from its own thread.
        thread::sleep(LOG_INTERVAL * 3);
        let text = std::fs::read_to_string(&path).unwrap();
        for expected in ["Connection: Connected", "Laser: card", "Job: sending Job", "Job: Finished after"] {
            assert!(text.contains(expected), "{expected} missing in:\n{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disconnecting_while_sending_stops_the_job() {
        let sim = SimHandle::default();
        sim.0.lock().unwrap().chunk_time = Duration::from_millis(20);
        let mut m = machine(&sim);
        m.start_job(big_job(), 1.0, JobKind::Job).unwrap();
        wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| j.sent > 0));
        m.disconnect();
        let s = wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert_eq!(s.job.unwrap().phase, JobPhase::Stopped);
        let sim = sim.0.lock().unwrap();
        assert!(sim.stopped, "D8 01 reached the controller");
        assert!(!sim.received.contains(&0xD7), "the rest of the job was not sent");
    }

    #[test]
    fn disconnecting_while_running_stops_the_job() {
        let sim = SimHandle::default();
        let mut m = machine(&sim);
        m.start_job(job(), 1.0, JobKind::Job).unwrap();
        wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        sim.0.lock().unwrap().busy_polls = 1000;
        m.disconnect();
        let s = wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert_eq!(s.job.unwrap().phase, JobPhase::Stopped);
        assert!(sim.0.lock().unwrap().stopped);
    }

    #[test]
    fn quitting_stops_a_running_job_before_returning() {
        let sim = SimHandle::default();
        let m = machine(&sim);
        m.start_job(job(), 1.0, JobKind::Job).unwrap();
        wait_for(&m.shared, |s| s.job.as_ref().is_some_and(|j| j.phase == JobPhase::Running));
        sim.0.lock().unwrap().busy_polls = 1000;
        drop(m);
        assert!(sim.0.lock().unwrap().stopped, "Stop was sent before Fokus went away");
    }
}
