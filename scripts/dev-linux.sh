#!/usr/bin/env bash
# Start the desktop app on Linux with the CPU speech engines (whisper.cpp + sherpa-onnx).
# Fedora deps: sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel
#   javascriptcoregtk4.1-devel opus-devel libappindicator-gtk3-devel librsvg2-devel clang-devel
# Heavy work can go to a stronger machine: Settings → "Home PC (offload)" (docs/LINUX.md).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# WebKitGTK's DMA-BUF renderer shows a blank window on some Intel/NVIDIA + Wayland setups.
export WEBKIT_DISABLE_DMABUF_RENDERER="${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"
exec pnpm -C "$here/apps/desktop" tauri dev --features whisper,sherpa "$@"
