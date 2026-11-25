#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod config;
#[cfg(any(windows, test))]
mod engine;
#[cfg(windows)]
mod route;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError},
            System::Threading::CreateMutexW,
            UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW},
        },
        core::w,
    };
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\AudioHub.Rust")) };
    let Ok(mutex) = mutex else { return };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = MessageBoxW(
                None,
                w!("AudioHub 已在运行。请切换到已打开的窗口。"),
                w!("AudioHub"),
                MB_OK | MB_ICONINFORMATION,
            );
            let _ = CloseHandle(mutex);
        }
        return;
    }
    let exe = std::env::current_exe().expect("无法获取 AudioHub 路径");
    let root = exe
        .ancestors()
        .skip(1)
        .take(3)
        .find(|p| p.join("Cargo.toml").is_file() && p.join("src/main.rs").is_file())
        .unwrap_or(exe.parent().expect("无法获取 AudioHub 目录"));
    ui::run(root.to_owned());
    unsafe {
        let _ = CloseHandle(mutex);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("请在 Windows 上运行 AudioHub。");
}
