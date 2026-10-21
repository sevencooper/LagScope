# LagScope

Passive lag / hiccup recorder for THE FINALS on Windows. It records game packet timing, pings, frame times, Wi-Fi and CPU/GPU data while you play. When you stop it, it builds an offline HTML report showing where each marked lag spike came from.

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

Layout:

- `src/win/`: Windows collectors (ETW, pings, Wi-Fi, CPU/GPU, hotkey, system info).
- `src/analyze/`: platform-independent analysis and report generation.
- `src/report/`: the HTML template and the bundled uPlot charting library.
