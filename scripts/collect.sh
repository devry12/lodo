#!/usr/bin/env bash
# Kumpulkan hasil `cargo packager --release` ke satu folder dengan nama baku
# yang dikenali lodo-release (lihat crates/lodo-release/src/main.rs).
#
#   scripts/collect.sh [folder-keluaran]     (default: release)
set -euo pipefail
out=${1:-release}
mkdir -p "$out"
arch=$(uname -m)
case "$arch" in arm64|aarch64) arch=aarch64 ;; x86_64|amd64) arch=x86_64 ;; esac

case "$(uname -s)" in
  Darwin)
    plat=darwin-$arch
    tar -czf "$out/lodo-$plat.app.tar.gz" -C dist lodo.app
    cp dist/*.dmg "$out/lodo-$plat.dmg"
    cp target/release/lodo-helper "$out/lodo-helper-$plat"
    ;;
  Linux)
    plat=linux-$arch
    cp dist/*.AppImage "$out/lodo-$plat.AppImage"
    cp dist/*.deb "$out/lodo-$plat.deb"
    cp target/release/lodo-helper "$out/lodo-helper-$plat"
    ;;
  MINGW* | MSYS* | CYGWIN*)
    plat=windows-$arch
    cp dist/*-setup.exe "$out/lodo-$plat-setup.exe"
    cp target/release/lodo-helper.exe "$out/lodo-helper-$plat.exe"
    ;;
  *) echo "Sistem tidak dikenal: $(uname -s)" >&2; exit 1 ;;
esac
ls -l "$out"
