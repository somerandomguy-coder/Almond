@echo off
rem Build a normal, clickable Almond for this PC (Windows + NVIDIA GPU) and its installer.
rem
rem   scripts\build-app.cmd            build, then run the installer
rem   scripts\build-app.cmd --no-install
rem
rem Result: %USERPROFILE%\.almond\t\release\bundle\nsis\Almond_<version>_x64-setup.exe
rem It installs for the current user (no admin) with a Start menu entry. The installed app needs
rem what this PC already has: the CUDA toolkit on PATH (GPU speech) and Ollama (AI notes), which
rem starts with Windows. Speech models stay in %USERPROFILE%\.almond\models; meetings stay in
rem %APPDATA%\dev.almond.app, shared with `scripts\dev-gpu.cmd` (close one before opening the other).
rem
rem Build needs the same tools as dev-gpu.cmd (VS 2022 Build Tools, CUDA, LLVM). The first
rem build takes a while; CARGO_BUILD_JOBS (default 6) keeps the CPU from running flat out.
setlocal
set "ROOT=%~dp0.."
if "%CARGO_TARGET_DIR%"=="" set "CARGO_TARGET_DIR=%USERPROFILE%\.almond\t"
if "%CARGO_BUILD_JOBS%"=="" set "CARGO_BUILD_JOBS=6"
set "CUDA_ENV=%ROOT%\crates\almond-stt\scripts\with-cuda-env.cmd"
set "SRC_TAURI=%ROOT%\apps\desktop\src-tauri"

rem Step 1 only produces the speech engine DLLs (sherpa-rs copies them into target\release); the
rem packaging step compiles the app itself with Tauri's own flags, so skip it once they exist.
if exist "%CARGO_TARGET_DIR%\release\sherpa-onnx-c-api.dll" goto dlls
echo [1/3] Compiling the speech engines...
call "%CUDA_ENV%" cargo build --release -p almond-desktop --features cuda,sherpa || exit /b 1

:dlls

echo [2/3] Collecting speech engine DLLs...
if not exist "%SRC_TAURI%\gpu-dlls" mkdir "%SRC_TAURI%\gpu-dlls"
del /q "%SRC_TAURI%\gpu-dlls\*.dll" 2>nul
copy /y "%CARGO_TARGET_DIR%\release\onnxruntime*.dll" "%SRC_TAURI%\gpu-dlls\" >nul || exit /b 1
copy /y "%CARGO_TARGET_DIR%\release\sherpa-onnx*.dll" "%SRC_TAURI%\gpu-dlls\" >nul || exit /b 1

echo [3/3] Packaging the installer...
call "%CUDA_ENV%" pnpm -C "%ROOT%\apps\desktop" tauri build --features cuda,sherpa --config "%SRC_TAURI%\tauri.gpu.conf.json" || exit /b 1

set "SETUP="
for %%f in ("%CARGO_TARGET_DIR%\release\bundle\nsis\Almond_*_x64-setup.exe") do set "SETUP=%%f"
echo.
echo Installer: %SETUP%
if /i "%~1"=="--no-install" exit /b 0
if defined SETUP start "" "%SETUP%"
