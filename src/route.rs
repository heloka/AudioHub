use std::{sync::mpsc, thread, time::Duration};
use windows::{
    Win32::{
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATEMASK_ALL, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
            eConsole, eMultimedia, eRender,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
    },
    core::{Interface, Result},
};

fn endpoint_id(device: &IMMDevice) -> Result<String> {
    // MMDevice allocates this UTF-16 identifier with the COM task allocator.
    unsafe {
        let id = device.GetId()?;
        let value = String::from_utf16_lossy(id.as_wide());
        CoTaskMemFree(Some(id.0.cast()));
        Ok(value)
    }
}

#[derive(PartialEq)]
struct Outputs {
    defaults: [Option<String>; 2],
    states: Vec<(String, u32)>,
}

fn snapshot(enumerator: &IMMDeviceEnumerator) -> Result<Outputs> {
    unsafe {
        let default = |role| {
            enumerator
                .GetDefaultAudioEndpoint(eRender, role)
                .ok()
                .map(|device| endpoint_id(&device))
                .transpose()
        };
        let defaults = [default(eConsole)?, default(eMultimedia)?];
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
        let mut states = Vec::new();
        for i in 0..devices.GetCount()? {
            let device = devices.Item(i)?;
            states.push((endpoint_id(&device)?, device.GetState()?.0));
        }
        states.sort_unstable();
        Ok(Outputs { defaults, states })
    }
}

pub fn watch() -> mpsc::Receiver<()> {
    let (changed, receiver) = mpsc::channel();
    thread::spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(
            &MMDeviceEnumerator,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            let mut previous = None;
            loop {
                if let Ok(current) = snapshot(&enumerator) {
                    if previous.as_ref().is_some_and(|old| old != &current)
                        && changed.send(()).is_err()
                    {
                        break;
                    }
                    previous = Some(current);
                }
                thread::sleep(Duration::from_secs(2));
            }
        }
        CoUninitialize();
    });
    receiver
}

/// An active render session proves that the player opened a Windows audio stream.
/// A silent video or paused app is valid, so do not gate on measured volume.
pub fn player_active(pid: u32) -> Result<bool> {
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    };
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let result = (|| {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)?;
            let outputs = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for i in 0..outputs.GetCount()? {
                let device = outputs.Item(i)?;
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_INPROC_SERVER, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                for j in 0..sessions.GetCount()? {
                    let session = sessions.GetSession(j)?;
                    if session.GetState()? == AudioSessionStateActive
                        && session.cast::<IAudioSessionControl2>()?.GetProcessId()? == pid
                    {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        })();
        CoUninitialize();
        result
    }
}
