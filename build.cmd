@echo off
setlocal
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>&1 || (
    echo Install Rust stable for Windows MSVC, then run build.cmd again.
    exit /b 1
)
if not defined VSCMD_VER (
    for /f "usebackq delims=" %%D in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do call "%%D\Common7\Tools\VsDevCmd.bat" -arch=x64 -no_logo
)
if not defined GPUI_FXC_PATH (
    for /f "delims=" %%F in ('where fxc.exe 2^>nul') do if not defined GPUI_FXC_PATH set "GPUI_FXC_PATH=%%F"
)
set "CARGO_TARGET_DIR=%~dp0target"
cargo build --release --locked || exit /b 1
copy /y "target\release\audiohub.exe" "audiohub.exe" >nul || exit /b 1
echo Built %~dp0audiohub.exe
