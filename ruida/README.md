# ruida

Rust implementation of the Ruida laser controller protocol (RDC644x): reading
and writing `.rd` job files (cutting and scanline engraving), and talking to
the controller over USB (status, sending jobs, pause / resume / stop).
Tested on a Ruida RDC6445G.

Based on community reverse engineering, mainly MeerK40t's Ruida driver.

```bash
cargo test -p ruida
cargo run -p ruida --bin rd-dump -- job.rd --svg preview.svg
```

`rd-dump` auto-detects the scramble magic (0x88 for the 644XG), lists every
command with its raw bytes, and prints a per-layer summary (speed, power,
colour, cut length) plus the cut area. `--svg` writes a toolpath preview;
our machine has its origin on the right, so add `--mirror-x` to see the job
the way LightBurn shows it.

Verified against a real LightBurn 1.7 job for our 644XG
(`tests/data/lightburn_square_text.*`): every command recognised, layer speed
and power match the project, geometry is correct.

## Live status (read-only)

```bash
cargo run -p ruida --bin rd-status -- --list          # find the port (FTDI 0403:6001)
cargo run -p ruida --bin rd-status -- --raw --watch   # card id, bed size, state, position
```

`rd-status` only ever sends memory *read* requests (`DA 00 <addr>`); the
`StatusLink` type has no way to send anything else, and a test checks this.
USB settings follow MeerK40t: 115200 baud, RTS/CTS, no checksum, no ACK.
Close LightBurn first — only one program can hold the port.

## Sending jobs

```bash
cargo run -p ruida --bin rd-send -- job.rd    # shows what will happen, asks before sending
cargo run -p ruida --bin rd-send -- --stop    # stop the running job
```

`JobSender` (in `sender`) streams a job over USB in ~1000-byte chunks split
between commands, and sends pause / resume / stop. Unlike `StatusLink`, this
moves the machine and fires the laser.

Modules: `scramble` (byte obfuscation), `codec` (7-bit number encodings),
`command` (split + decode), `job` (toolpath), `svg` (preview), `fill`
(scanline engraving), `writer` (job files), `memory` + `link` (status),
`sender` (jobs), `serial` (USB).
