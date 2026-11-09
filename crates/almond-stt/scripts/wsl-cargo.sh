#!/usr/bin/env bash
# Run cargo for almond-stt inside WSL / Linux using whisper-rs-sys' pregenerated bindings.
#
# Why: on Windows, whisper-rs-sys needs libclang (bindgen); its bundled bindings.rs was generated
# on x86_64 Linux (c_long = 64 bit) and is ABI-wrong on Windows (int64_t -> c_long = 32 bit).
# On Linux the bundled bindings are correct, so no clang is needed there.
#
# Usage (from the repo root, in WSL):
#   crates/almond-stt/scripts/wsl-cargo.sh test -p almond-stt --release --features whisper,sherpa
set -euo pipefail
export WHISPER_DONT_GENERATE_BINDINGS=1
# Keep Linux build artifacts off the Windows filesystem (faster, avoids clashing with target/).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.almond-target}"
# Models are shared with Windows when ALMOND_MODELS_DIR points at the Windows profile.
exec cargo "$@"
