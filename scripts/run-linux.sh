#!/usr/bin/env bash
# Build the release app (UI embedded, CPU speech engines) and start it. For everyday use on Linux;
# scripts/dev-linux.sh is for editing the app (debug build + Vite dev server).
#   scripts/run-linux.sh            build if needed, then run
#   scripts/run-linux.sh --no-build run the last build
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${1:-}" != "--no-build" ]]; then
  pnpm -C "$here/apps/desktop" install --frozen-lockfile >/dev/null
  pnpm -C "$here/apps/desktop" tauri build --no-bundle --features whisper,sherpa
fi
# WebKitGTK's DMA-BUF renderer shows a blank window on some Intel/NVIDIA + Wayland setups.
export WEBKIT_DISABLE_DMABUF_RENDERER="${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"
exec "$here/target/release/almond-desktop"
