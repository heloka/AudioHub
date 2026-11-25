use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{ErrorKind, Write},
    path::Path,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    pub addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

impl Device {
    pub fn new(id: u64, name: String, addr: String) -> Self {
        Self {
            id,
            name,
            addr,
            identity: None,
        }
    }
}

fn default_args() -> Vec<String> {
    [
        "--no-video",
        "--no-window",
        "--audio-codec=aac",
        "--audio-bit-rate=64k",
        "--audio-buffer=100",
    ]
    .map(str::to_owned)
    .into()
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub auto_scan: bool,
    pub auto_add: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded_identities: Vec<String>,
    pub usb_recovery: bool,
    pub fixed_port: bool,
    pub restore_sessions: bool,
    pub resume_ids: Vec<u64>,
    pub devices: Vec<Device>,
    #[serde(default = "default_args")]
    pub scrcpy_args: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_scan: true,
            auto_add: true,
            excluded_identities: Vec::new(),
            usb_recovery: false,
            fixed_port: false,
            restore_sessions: false,
            resume_ids: Vec::new(),
            devices: Vec::new(),
            scrcpy_args: default_args(),
        }
    }
}

impl Config {
    pub fn load(root: &Path) -> Result<Self, String> {
        for filename in ["audiohub.json", "devices.json"] {
            let path = root.join(filename);
            let data = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("无法读取 {filename}：{e}")),
            };
            let mut config: Self = serde_json::from_str(data.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("{filename} 格式无效：{e}"))?;
            let mut used = HashSet::new();
            let mut next = 1;
            let existing: HashSet<_> = config.devices.iter().map(|d| d.id).collect();
            for device in &mut config.devices {
                if device.addr.trim().is_empty() {
                    return Err(format!("{filename} 中有设备缺少连接地址"));
                }
                if device.id == 0 || !used.insert(device.id) {
                    while existing.contains(&next) || used.contains(&next) {
                        next += 1;
                    }
                    device.id = next;
                    used.insert(next);
                }
            }
            config.resume_ids.retain(|id| used.contains(id));
            config.resume_ids.sort_unstable();
            config.resume_ids.dedup();
            return Ok(config);
        }
        Ok(Self::default())
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let save = || -> Result<(), Box<dyn std::error::Error>> {
            let mut tmp = tempfile::NamedTempFile::new_in(root)?;
            serde_json::to_writer_pretty(&mut tmp, self)?;
            tmp.write_all(b"\n")?;
            tmp.as_file().sync_all()?;
            tmp.persist(root.join("audiohub.json"))?;
            Ok(())
        };
        save().map_err(|e| format!("保存 audiohub.json 失败：{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_legacy_without_overwriting_or_resuming() {
        let dir = tempfile::tempdir().unwrap();
        let old = r#"{"devices":[{"id":1,"name":"Pad","addr":"10.0.0.8:5556","enabled":true}],"scrcpy_args":["--audio-buffer","300"]}"#;
        fs::write(dir.path().join("devices.json"), old).unwrap();
        let mut cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.scrcpy_args, ["--audio-buffer", "300"]);
        assert!(cfg.auto_scan && cfg.auto_add);
        assert!(!cfg.restore_sessions && !cfg.usb_recovery && cfg.resume_ids.is_empty());
        cfg.auto_scan = false;
        cfg.restore_sessions = true;
        cfg.resume_ids = vec![1, 1, 42];
        cfg.devices[0].identity = Some("pad-serial".into());
        cfg.save(dir.path()).unwrap();
        cfg.save(dir.path()).unwrap();
        let loaded = Config::load(dir.path()).unwrap();
        assert!(loaded.restore_sessions && !loaded.auto_scan);
        assert_eq!(loaded.resume_ids, [1]);
        assert_eq!(
            Config::load(dir.path()).unwrap().devices[0]
                .identity
                .as_deref(),
            Some("pad-serial")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("devices.json")).unwrap(),
            old
        );
        assert!(
            !fs::read_to_string(dir.path().join("audiohub.json"))
                .unwrap()
                .contains("enabled")
        );
    }

    #[test]
    fn corrupt_new_file_is_reported_instead_of_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("audiohub.json"), "{").unwrap();
        assert!(Config::load(dir.path()).is_err());
    }
}
