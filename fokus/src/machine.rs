//! Background connection to the laser.
//!
//! A worker thread owns the serial port: it polls status and, when asked,
//! sends a job and pause / resume / stop commands. The UI reads a shared
//! snapshot and talks to the worker through a command queue, so the port
//! is only ever used from one place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ruida::link::{StatusLink, Transport};
use ruida::memory::MachineStatus;
use ruida::scramble::MAGIC_644X;
use ruida::sender::JobSender;
use ruida::serial::SerialTransport;

use crate::t;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_FAILURES: u32 = 3;
/// Idle polls after being busy before a job counts as finished.
const IDLE_TO_FINISH: u32 = 4;
/// How long to wait for the machine to get busy after sending.
const START_TIMEOUT: Duration = Duration::from_secs(6);

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

#[derive(Debug, Clone)]
pub struct JobProgress {
    pub phase: JobPhase,
    pub sent: usize,
    pub total: usize,
    pub started: Instant,
    pub ended: Option<Instant>,
    /// Expected laser time, for the progress bar.
    pub estimate_s: f64,
    /// Sent with the laser off, to check the path.
    pub dry_run: bool,
    /// Time spent paused so far, and since when it is paused now.
    pub paused_total: Duration,
    pub paused_since: Option<Instant>,
}

impl JobProgress {
    fn new(total: usize, estimate_s: f64, dry_run: bool) -> Self {
        JobProgress { phase: JobPhase::Sending, sent: 0, total, started: Instant::now(), ended: None, estimate_s, dry_run, paused_total: Duration::ZERO, paused_since: None }
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
    Send { plain: Vec<u8>, estimate_s: f64, dry_run: bool },
    Pause,
    Resume,
    Stop,
}

pub struct Machine {
    shared: Arc<Mutex<Snapshot>>,
    stop: Option<Arc<AtomicBool>>,
    tx: Option<Sender<Cmd>>,
}

impl Machine {
    pub fn new() -> Self {
        let snapshot = Snapshot { conn: Conn::Disconnected, port: None, status: None, updated: None, job: None };
        Machine { shared: Arc::new(Mutex::new(snapshot)), stop: None, tx: None }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().unwrap().clone()
    }

    pub fn connect(&mut self, port: String, ctx: eframe::egui::Context) {
        self.disconnect();
        {
            let mut s = self.shared.lock().unwrap();
            s.conn = Conn::Connecting;
            s.port = Some(port.clone());
            s.status = None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        self.stop = Some(stop.clone());
        self.tx = Some(tx);
        let shared = self.shared.clone();
        thread::spawn(move || {
            let repaint = || ctx.request_repaint();
            let result = SerialTransport::open(&port, true)
                .map_err(|e| t!("err-cannot-open-port", port = port.clone(), error = e.to_string()))
                .and_then(|t| serve(StatusLink::new(t, MAGIC_644X), &shared, &stop, &rx, &repaint));
            let mut s = shared.lock().unwrap();
            s.conn = match result {
                Ok(()) => Conn::Disconnected,
                Err(e) => Conn::Failed(e),
            };
            if let Some(job) = s.job.as_mut().filter(|j| j.phase.active()) {
                job.phase = JobPhase::Failed(t!("err-connection-lost"));
                job.ended = Some(Instant::now());
            }
            repaint();
        });
    }

    pub fn disconnect(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.tx = None;
        self.shared.lock().unwrap().conn = Conn::Disconnected;
    }

    /// Send a job; it starts at the head as soon as it arrives.
    pub fn start_job(&self, plain: Vec<u8>, estimate_s: f64, dry_run: bool) -> Result<(), String> {
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
        set_job(&self.shared, Some(JobProgress::new(plain.len(), estimate_s, dry_run)));
        tx.send(Cmd::Send { plain, estimate_s, dry_run }).map_err(|_| t!("err-connection-lost"))
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
    }
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
}

/// The worker loop: poll status, run commands. Generic over the transport
/// so it can be tested against a simulated controller.
pub(crate) fn serve<T: Transport>(mut link: StatusLink<T>, shared: &Mutex<Snapshot>, stop: &AtomicBool, rx: &Receiver<Cmd>, repaint: &dyn Fn()) -> Result<(), String> {
    link.timeout = Duration::from_millis(500);
    let mut status = link.status().map_err(|e| t!("err-no-answer", error = e.to_string()))?;
    publish(shared, repaint, &status, stop);

    let mut watch = Watch::default();
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) {
        loop {
            match rx.try_recv() {
                Ok(cmd) => link = handle(cmd, link, shared, rx, repaint, &mut watch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        thread::sleep(POLL_INTERVAL);
        match link.poll(&mut status) {
            Ok(()) => {
                failures = 0;
                track_job(shared, &status, &mut watch);
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
    Ok(())
}

fn handle<T: Transport>(cmd: Cmd, link: StatusLink<T>, shared: &Mutex<Snapshot>, rx: &Receiver<Cmd>, repaint: &dyn Fn(), watch: &mut Watch) -> StatusLink<T> {
    let mut sender = JobSender::new(link.into_transport(), MAGIC_644X);
    match cmd {
        Cmd::Send { plain, estimate_s, dry_run } => {
            set_job(shared, Some(JobProgress::new(plain.len(), estimate_s, dry_run)));
            let mut stop_requested = false;
            let result = sender.send_job(&plain, |sent, _| {
                update_job(shared, |j| j.sent = sent);
                repaint();
                // Only Stop matters while sending.
                while let Ok(cmd) = rx.try_recv() {
                    stop_requested |= matches!(cmd, Cmd::Stop);
                }
                !stop_requested
            });
            match result {
                Ok(true) => {
                    *watch = Watch::default();
                    update_job(shared, |j| j.phase = JobPhase::Running);
                }
                Ok(false) => {
                    let _ = sender.stop();
                    end_job(shared, JobPhase::Stopped);
                }
                Err(e) => {
                    let _ = sender.stop();
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
        Cmd::Stop => {
            let _ = sender.stop();
            if shared.lock().unwrap().job_active() {
                end_job(shared, JobPhase::Stopped);
            }
        }
    }
    repaint();
    let mut link = StatusLink::new(sender.into_transport(), MAGIC_644X);
    link.timeout = Duration::from_millis(500);
    link
}

/// Move a running job to Finished once the machine was busy and is idle
/// again, or to Failed if it never got busy.
fn track_job(shared: &Mutex<Snapshot>, status: &MachineStatus, watch: &mut Watch) {
    let mut s = shared.lock().unwrap();
    let Some(job) = s.job.as_mut().filter(|j| j.phase == JobPhase::Running) else { return };
    if status.state() == "idle" {
        watch.idle_polls += 1;
        if watch.seen_busy && watch.idle_polls >= IDLE_TO_FINISH {
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
    }

    #[derive(Clone, Default)]
    struct SimHandle(Arc<Mutex<Sim>>);

    impl Transport for SimHandle {
        fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
            let plain = unscramble(data, MAGIC_644X);
            let mut sim = self.0.lock().unwrap();
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
        thread::spawn(move || serve(StatusLink::new(s, MAGIC_644X), &sh, &st, &rx, &|| {}));
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
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, dry_run: false }).unwrap();
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
        tx.send(Cmd::Send { plain: job(), estimate_s: 1.0, dry_run: false }).unwrap();
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
        tx.send(Cmd::Send { plain: vec![0xD8, 0x12, 0xD7], estimate_s: 1.0, dry_run: false }).unwrap();
        let s = wait_for(&shared, |s| s.job.as_ref().is_some_and(|j| !j.phase.active()));
        assert!(matches!(s.job.unwrap().phase, JobPhase::Failed(_)));
        assert!(!sim.0.lock().unwrap().stopped);
        stop.store(true, Ordering::Relaxed);
    }
}
