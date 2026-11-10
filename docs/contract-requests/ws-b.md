# WS-B (STT) contract / workspace requests

None of these block WS-B; almond-stt works against `m0-contracts` as is.

## 1. CI must not use `--all-features` for `almond-stt` (root/CI change)

`almond-stt` has GPU features `cuda` and `vulkan` that need the CUDA toolkit / Vulkan SDK, so
`cargo clippy --workspace --all-features` fails on any runner without them. Proposed CI:

```sh
cargo clippy --workspace --all-targets --exclude almond-stt -- -D warnings   # plus --all-features as today
cargo clippy -p almond-stt --all-targets --features whisper,sherpa,download -- -D warnings
```

## 2. whisper-rs needs libclang on Windows (root `.cargo/config.toml` or CI image)

`whisper-rs-sys` generates bindings with bindgen. Its bundled fallback `bindings.rs` was generated
on x86_64 Linux (`int64_t` -> `c_long`, which is 32-bit on Windows), so on Windows it is ABI-wrong
and fails const layout asserts. Therefore:

- Linux (CI, Fedora): set `WHISPER_DONT_GENERATE_BINDINGS=1` (no clang needed) — see
  `crates/almond-stt/scripts/wsl-cargo.sh`. Suggest adding it to the Linux CI job env.
- Windows: LLVM (libclang) must be installed (`winget install LLVM.LLVM`, `LIBCLANG_PATH`) to
  build with `--features whisper`. On the dev PC this is **not installed** (install was declined),
  so the whisper engine is built/tested in WSL only for now.
- Windows CUDA additionally needs: MSVC v143 (VS 2022 Build Tools; CUDA 13.1's cudafe++ crashes
  with MSVC 14.51), Ninja generator, and a short target dir (nvcc MAX_PATH). All handled by
  `crates/almond-stt/scripts/cargo-cuda.cmd`.

## 3. Document the `SttUpdate` partial/final protocol in `almond-types::stt`

almond-stt implements (and the UI should rely on): updates carry `segment.idx`; each partial
(`is_final=false`) for idx N **replaces** the previous partial for N; exactly one final per idx;
after the final for N no further updates for N arrive. Proposed doc comment on `SttUpdate`.

## 4. (Nice to have) model-management API types for the Settings → Models screen (WS-F)

`SttModelInfo` covers listing. For downloads the UI will want a progress event, e.g.

```rust
pub struct ModelDownloadProgress { pub id: String, pub done_bytes: u64, pub total_bytes: Option<u64> }
```

and a `role` on `SttModelInfo` (`stt | segmentation | embedding`) so diarization models can be
shown separately. almond-stt's `ModelManager::ensure(id, progress_cb)` already provides the data.
