# LagScope

Lag / hiccup diagnostics for THE FINALS on Windows. Two modes, picked from a menu when you double-click the exe:

- **Wi-Fi check & fix** (desktop, game closed): offers Wi-Fi/Bluetooth driver updates from Windows Update, measures the router ping, then switches suspect settings off one at a time (roaming, power saving, band, Bluetooth, Location, Wi-Fi Direct, Windows background scanning). Each one is restored right after its test, and the check offers to keep the one that removed the freezes. Every change is journaled in `%ProgramData%\LagScope`, so an interrupted test is undone at the next start.
- **Record while playing** (passive): records game packet timing, pings, frame times, Wi-Fi, NDIS driver requests and CPU/GPU data. When you stop it, it builds an offline HTML report showing where each marked lag spike came from.

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

## Develop

- `cargo test`: analyzer tests (they run on Linux).
- `cargo run --release -- analyze <session folder>`: rebuilds `report.html` from a recorded session. This lets you tweak the analysis without replaying a game.
- `lagscope.exe --help`: capture options (hotkey, extra ping targets, duration, ...).
- `lagscope.exe wifi-inventory [--wu]`: read-only. Shows what the Wi-Fi check would test on this PC, and with `--wu` the driver updates Windows Update offers.
- `lagscope.exe --wifi-check --unattended --force-tests --check-secs 15`: runs every test with no questions and keeps nothing. Use this to try the check end to end.

Layout:

- `src/win/`: Windows collectors (ETW, pings, Wi-Fi, CPU/GPU, hotkey, system info).
- `src/win/wificheck.rs`, `src/win/fixes.rs`: the guided Wi-Fi check, plus the reversible settings changes it makes.
- `src/analyze/`: platform-independent analysis and report generation.
- `src/report/`: the HTML template and the bundled uPlot charting library.
