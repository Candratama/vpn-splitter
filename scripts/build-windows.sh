#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
target="x86_64-pc-windows-gnu"
echo "Building Windows release ($target)..."
cargo build --release --target "$target" -p vpn-splitter-desktop -p vpn-splitter-helper

out_dir="target/release/windows"
mkdir -p "$out_dir"
cp "target/$target/release/vpn-splitter-desktop.exe" "$out_dir/"
cp "target/$target/release/vpn-splitter-helper.exe" "$out_dir/"

echo "Windows binaries ready in $out_dir:"
ls -lh "$out_dir"
