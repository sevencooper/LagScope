#!/usr/bin/env bash
# Builds lagscope.exe (static MSVC CRT, cross-compiled from Linux/WSL with cargo-xwin)
# and packages it with the README into dist/LagScope.zip.
set -euo pipefail
cd "$(dirname "$0")"
source "$HOME/.cargo/env" 2>/dev/null || true
cargo test --quiet
cargo xwin build --release --target x86_64-pc-windows-msvc
rm -rf dist && mkdir -p dist/LagScope
cp target/x86_64-pc-windows-msvc/release/lagscope.exe dist/LagScope/
cp README.txt PRIVACY.md LICENSE dist/LagScope/
cp src/fonts/OFL.txt dist/LagScope/FONTS-OFL.txt
(cd dist && python3 -c "import shutil; shutil.make_archive('LagScope', 'zip', '.', 'LagScope')")
echo "built: dist/LagScope/lagscope.exe ($(du -h dist/LagScope/lagscope.exe | cut -f1)), dist/LagScope.zip"
# Optional: copy to a Windows folder for testing, e.g. ./build.sh /mnt/c/Users/me/Desktop/LagScope
if [[ $# -ge 1 ]]; then mkdir -p "$1" && cp dist/LagScope/* "$1/" && echo "copied to $1"; fi
