@echo off
rem Run cargo with the whisper.cpp CUDA backend on Windows (see with-cuda-env.cmd for the why).
rem
rem Usage (from the repo root):
rem   crates\almond-stt\scripts\cargo-cuda.cmd build -p almond-stt --release --features cuda,sherpa
call "%~dp0with-cuda-env.cmd" cargo %*
