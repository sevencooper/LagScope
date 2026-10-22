# LagScope

Lag / hiccup diagnostics for online games on Windows. It started with THE FINALS and works with any game through per-game profiles. Double-clicking the exe opens a small native window (egui, OpenGL) with three tabs: Record, Wi-Fi check and Sessions. The window redraws at most once a second while recording and not at all when idle. Run with arguments, the same exe works as a console tool (`--console` for the text menu). The two modes:

- **Wi-Fi check & fix** (desktop, game closed): offers Wi-Fi/Bluetooth driver updates from Windows Update, measures the router ping, then switches suspect settings off one at a time (roaming, power saving, band, Bluetooth, Location, Wi-Fi Direct, Windows background scanning). Each one is restored right after its test, and the check offers to keep the one that removed the freezes. Every change is journaled in `%ProgramData%\LagScope`, so an interrupted test is undone at the next start.
- **Record while playing** (passive): records game packet timing, pings, frame times, Wi-Fi, NDIS driver requests, CPU/GPU data and **driver latency**: every driver's DPC and interrupt-handler run times, LatencyMon-style, from a private kernel ETW session. When you stop it, it builds an offline HTML report showing where each marked lag spike came from.

End-user instructions are in [README.txt](README.txt), which ships inside the release zip.

## Build

The exe is cross-compiled from Linux or WSL. Windows itself only needs the finished exe.

One-time setup:

```sh
curl https://sh.rustup.rs -sSf | sh         # Rust toolchain
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin                     # downloads the MSVC SDK on first build
```

Build:

```sh
./build.sh                    # tests + release build -> dist/LagScope.zip
./build.sh /mnt/c/some/dir    # same, and also copies the exe + README there
```

The result is one static `lagscope.exe` with no runtime dependencies. It asks for admin rights when it starts, because ETW tracing needs them.

## Game profiles

Built in: `the_finals`, `cs2`, and `auto`, which picks the process presenting the most frames. `lagscope.exe games` lists them. To add a game, drop a JSON file into a `profiles` folder next to the exe, or into `%ProgramData%\LagScope\profiles`:

```json
{
  "id": "apex",
  "name": "Apex Legends",
  "processes": ["r5apex.exe"],
  "ignore_ports": [],
  "match_detection": "send_rate",
  "min_gap_ms": 80,
  "tick_merge_ms": 5,
  "hint": "What to look for in this game at each hiccup."
}
```

Only `id`, `name` and `processes` are required. What the other fields mean:

- `ignore_ports`: remote UDP ports that are never the game server, e.g. latency beacons.
- `match_detection`: how match play is told apart from menus.
  - `send_rate`: seconds in which the client sends at least half its peak rate count as play.
  - `none`: the whole session counts as play.
- `min_gap_ms`: the smallest silence in the server stream that counts as a stall.
- `tick_merge_ms`: packets that arrive within this many ms count as one server tick.

A file with the same `id` as a built-in replaces it. The profile is saved in each session's `meta.json`, so `analyze` re-runs with the same settings.

## Develop

- `cargo test`: analyzer tests (they run on Linux).
- `cargo run --release -- analyze <session folder>`: rebuilds `report.html` from a recorded session. This lets you tweak the analysis without replaying a game.
- `lagscope.exe --help`: capture options (hotkey, extra ping targets, duration, ...).
- `lagscope.exe --record --game <id>`: record with a given profile (the menu remembers the last one).
- `lagscope.exe wifi-inventory [--wu]`: read-only. Shows what the Wi-Fi check would test on this PC, and with `--wu` the driver updates Windows Update offers.
- `lagscope.exe --gui-selftest <idle|record|close|prompt|wifi> <out dir>`: scripted GUI run. It saves screenshots (BMP) and `selftest.txt` with CPU and memory figures, then closes. `wifi` runs a short Wi-Fi check, so settings change temporarily.
- `lagscope.exe --wifi-check --unattended --force-tests --check-secs 15`: runs every test with no questions and keeps nothing. Use this to try the check end to end.

Layout:

- `src/win/`: Windows collectors (ETW, pings, Wi-Fi, CPU/GPU, hotkey, system info).
- `src/win/wificheck.rs`, `src/win/fixes.rs`: the guided Wi-Fi check, plus the reversible settings changes it makes.
- `src/win/gui.rs`: the window. It is a thin front-end over `capture::start` / `Capture::finish`, the same engine the console uses.
- `src/win/drivers.rs`: driver DPC/ISR latency from a private kernel ETW session.
- `src/profile.rs`: game profiles.
- `src/analyze/`: platform-independent analysis and report generation.
- `src/report/`: the HTML template and the bundled uPlot charting library.
