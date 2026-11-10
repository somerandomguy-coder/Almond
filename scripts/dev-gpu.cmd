@echo off
rem Start the desktop app with real speech engines on the GPU (Windows + NVIDIA).
rem Needs the CUDA toolkit, VS 2022 Build Tools and LLVM; see crates\almond-stt\scripts\with-cuda-env.cmd.
call "%~dp0..\crates\almond-stt\scripts\with-cuda-env.cmd" pnpm -C "%~dp0..\apps\desktop" tauri dev --features cuda,sherpa %*
