@echo off
rem Run cargo with the whisper.cpp CUDA backend on Windows.
rem
rem Why: Visual Studio 18 (2026) has no CUDA MSBuild integration, so CMake's VS generator fails
rem with "No CUDA toolset found". We build whisper.cpp with Ninja inside a VS developer env
rem instead. Also: no libclang on this PC, so use whisper-rs-sys' pregenerated bindings.
rem
rem Usage (from the repo root):
rem   crates\almond-stt\scripts\cargo-cuda.cmd build -p almond-stt --release --features cuda,sherpa
rem
rem Env overrides: VSDEVCMD (path to vcvars64.bat), CMAKE_CUDA_ARCHITECTURES (default 120 = RTX 50xx).

rem CUDA 13.1's cudafe++ crashes (0xC0000005) with MSVC 14.51 (VS 18), so prefer the VS 2022
rem Build Tools (MSVC 14.44) as the host compiler when installed:
rem   winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100"
if "%VSDEVCMD%"=="" if exist "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" set "VSDEVCMD=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if "%VSDEVCMD%"=="" set "VSDEVCMD=C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat"
call "%VSDEVCMD%" >nul || exit /b 1

set "CMAKE_GENERATOR=Ninja"
where ninja >nul 2>nul || if exist "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe" set "PATH=%PATH%;C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja"
if "%CMAKE_CUDA_ARCHITECTURES%"=="" set "CMAKE_CUDA_ARCHITECTURES=120"
rem nvcc may not yet list the newest MSVC as supported.
set "NVCC_APPEND_FLAGS=-allow-unsupported-compiler"
set "WHISPER_DONT_GENERATE_BINDINGS=1"
rem nvcc fails on paths > 260 chars ("Could not open output file ...cu.obj.d"); the ggml-cuda
rem object paths are ~170 chars below the target dir, so use a short target dir.
if "%CARGO_TARGET_DIR%"=="" set "CARGO_TARGET_DIR=%USERPROFILE%\.almond\t"
rem The newest SDK's mt.exe (10.0.28000) crashes with 0xc0000135 on this PC; pin a working one.
if "%CMAKE_MT%"=="" if exist "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\mt.exe" set "CMAKE_MT=C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/mt.exe"

cargo %*
