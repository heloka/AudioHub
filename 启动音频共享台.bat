@echo off
setlocal
cd /d "%~dp0"
if not exist "audiohub.exe" call build.cmd
if not exist "audiohub.exe" (
    pause
    exit /b 1
)
start "" "%~dp0audiohub.exe"
