use crate::config::Device;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const PORT: &str = "5038";
const POLL: Duration = Duration::from_millis(60);
const ADB_TIMEOUT: Duration = Duration::from_secs(7);

#[derive(Clone)]
pub struct Tools {
    adb: PathBuf,
    scrcpy: PathBuf,
    fixed_port: Arc<AtomicBool>,
    usb_stop: Arc<AtomicBool>,
    usb_switches: Arc<std::sync::Mutex<std::collections::HashMap<String, Instant>>>,
}

#[derive(Debug, Clone)]
pub struct Found {
    pub name: String,
    pub addr: String,
    pub pairing: bool,
    pub identity: Option<String>,
}

#[derive(Debug)]
pub enum Event {
    Status(String),
    Identified { addr: String, identity: String },
}

pub struct Session {
    stop: Arc<AtomicBool>,
    audio_generation: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Session {
    pub fn restart_audio(&self) {
        self.audio_generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Tools {
    pub fn discover(root: &Path) -> Self {
        let mut bundles = root.parent().map_or_else(Vec::new, |parent| {
            parent
                .read_dir()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("scrcpy-win64-")
                })
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        });
        bundles.sort();
        bundles.reverse();
        bundles.insert(0, root.to_path_buf());

        let find = |name: &str| {
            bundles
                .iter()
                .map(|dir| dir.join(name))
                .find(|path| path.is_file())
                .or_else(|| {
                    std::env::var_os("PATH").and_then(|paths| {
                        std::env::split_paths(&paths)
                            .map(|dir| dir.join(name))
                            .find(|path| path.is_file())
                    })
                })
                .unwrap_or_else(|| PathBuf::from(name))
        };
        Self {
            fixed_port: Default::default(),
            usb_stop: Arc::new(AtomicBool::new(true)),
            usb_switches: Default::default(),
            adb: find(if cfg!(windows) { "adb.exe" } else { "adb" }),
            scrcpy: find(if cfg!(windows) {
                "scrcpy.exe"
            } else {
                "scrcpy"
            }),
        }
    }

    pub fn scan(&self) -> Result<Vec<Found>, String> {
        let stop = AtomicBool::new(false);
        let advertised = self.discover_services(&stop);
        let connected = self.adb(&["devices"], &stop);
        let mut found: Vec<(Found, bool)> = advertised
            .as_ref()
            .ok()
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        if let Ok(list) = &connected {
            for line in list.lines() {
                let mut fields = line.split_whitespace();
                if let (Some(addr), Some("device")) = (fields.next(), fields.next())
                    && endpoint(addr).is_some()
                    && !found
                        .iter()
                        .any(|(item, _)| !item.pairing && item.addr == addr)
                {
                    found.push((
                        Found {
                            name: addr.into(),
                            addr: addr.into(),
                            pairing: false,
                            identity: None,
                        },
                        false,
                    ));
                }
            }
        }
        if found.is_empty() {
            advertised?;
            connected.as_ref().map_err(Clone::clone)?;
        }
        let online = connected.unwrap_or_default();
        found.sort_by_key(|(_, tls)| !*tls);
        let mut results = Vec::new();
        let mut identities = std::collections::HashSet::new();
        for (mut service, tls) in found {
            if !service.pairing {
                let alias = format!("{}._adb-tls-connect._tcp", service.name);
                let selector = if tls && device_state(&online, &alias) == Some("device") {
                    Some(alias.as_str())
                } else if device_state(&online, &service.addr) == Some("device") {
                    Some(service.addr.as_str())
                } else {
                    None
                };
                // A background discovery pass reads metadata only over an authorized transport.
                if let Some(selector) = selector {
                    service.identity = self
                        .property(selector, "ro.serialno", &stop)
                        .and_then(|serial| stable_serial(&serial));
                    if let Some(serial) = &service.identity
                        && !identities.insert(serial.clone())
                    {
                        continue;
                    }
                    if let Some(model) = self.property(selector, "ro.product.model", &stop) {
                        service.name = model;
                    }
                }
            }
            results.push(service);
        }
        Ok(results)
    }

    fn property(&self, selector: &str, key: &str, stop: &AtomicBool) -> Option<String> {
        self.adb(&["-s", selector, "shell", "getprop", key], stop)
            .ok()
            .filter(|value| !value.is_empty())
    }

    pub fn set_usb_recovery(&self, enabled: bool) {
        self.usb_stop.store(!enabled, Ordering::Relaxed);
    }

    /// Restore a known tablet's fixed wireless endpoint from its authorized USB transport.
    pub fn recover_usb(&self, devices: &[Device]) -> Result<Vec<(u64, String, String)>, String> {
        let stop = self.usb_stop.as_ref();
        let list = self.adb(&["devices"], stop)?;
        let mut recovered = Vec::new();
        let mut errors = Vec::new();
        self.usb_switches.lock().unwrap().retain(|serial, when| {
            device_state(&list, serial).is_some() && when.elapsed() < Duration::from_secs(60)
        });
        for line in list.lines() {
            let mut fields = line.split_whitespace();
            let (Some(usb), Some("device")) = (fields.next(), fields.next()) else {
                continue;
            };
            // Require the exact saved hardware serial. Wireless aliases, emulators,
            // unknown USB devices and distrusted serial placeholders cannot match.
            let Some(device) = devices.iter().find(|d| d.identity.as_deref() == Some(usb)) else {
                continue;
            };
            if stable_serial(usb).is_none()
                || usb.starts_with("emulator-")
                || usb.contains("._adb")
                || self.property(usb, "ro.serialno", stop).as_deref() != Some(usb)
            {
                continue;
            }
            let info = self.adb(
                &["-s", usb, "shell", "ip", "-4", "addr", "show", "wlan0"],
                stop,
            )?;
            let Some(ip) = wifi_ipv4(&info) else { continue };
            let addr = format!("{ip}:5556");
            if device_state(&list, &addr) == Some("device") {
                if device.addr != addr
                    && self.property(&addr, "ro.serialno", stop).as_deref() == Some(usb)
                {
                    recovered.push((device.id, addr, usb.to_owned()));
                }
                continue;
            }
            // Plugging in a charging cable during an existing wireless session must
            // not restart adbd on that device (including its native TLS transport).
            let native_prefix = format!("adb-{usb}-");
            if list.lines().any(|line| {
                let mut f = line.split_whitespace();
                f.next().is_some_and(|s| {
                    s.starts_with(&native_prefix) && s.ends_with("._adb-tls-connect._tcp")
                }) && f.next() == Some("device")
            }) || (device.addr != usb && device_state(&list, &device.addr) == Some("device"))
            {
                continue;
            }
            if TcpStream::connect_timeout(
                &SocketAddr::new(ip.into(), 5556),
                Duration::from_millis(400),
            )
            .is_err()
            {
                // Avoid repeatedly restarting adbd while the tablet changes transport.
                {
                    use std::collections::hash_map::Entry;
                    match self.usb_switches.lock().unwrap().entry(usb.to_owned()) {
                        Entry::Occupied(_) => continue,
                        Entry::Vacant(entry) => {
                            entry.insert(Instant::now());
                        }
                    }
                }
                if let Err(reason) = self.adb(&["-s", usb, "tcpip", "5556"], stop) {
                    errors.push(format!("{}：{reason}", device.name));
                    continue;
                }
            }
            let mut last_error = String::new();
            for _ in 0..4 {
                thread::sleep(Duration::from_millis(600));
                match self.identify(&addr, Some(usb), stop) {
                    Ok(_) => {
                        recovered.push((device.id, addr.clone(), usb.to_owned()));
                        last_error.clear();
                        break;
                    }
                    Err(reason) => last_error = reason,
                }
            }
            if !last_error.is_empty() {
                errors.push(format!("{} USB 恢复未完成：{last_error}", device.name));
            }
        }
        if recovered.is_empty() && !errors.is_empty() {
            return Err(errors.join("；"));
        }
        Ok(recovered)
    }

    fn discover_services(&self, stop: &AtomicBool) -> Result<Vec<(Found, bool)>, String> {
        self.adb(&["mdns", "services"], stop)
            .map(|output| parse_mdns(&output))
    }

    pub fn pair(&self, addr: &str, code: &str) -> Result<(), String> {
        if endpoint(addr).is_none()
            || code.len() != 6
            || !code.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("请输入有效的配对地址和六位配对码".into());
        }
        let stop = AtomicBool::new(false);
        // Pairing services live only as long as the Android pairing dialog.
        // Query the current advertisements; the UI may still show an earlier scan.
        if let Ok(services) = self.discover_services(&stop) {
            check_pair_endpoint(addr, &services)?;
        }
        let output = capture(
            command(&self.adb).args(["pair", addr]),
            Some(&format!("{code}\n")),
            Duration::from_secs(15),
            &stop,
        );
        pairing_result(output, code)
    }

    pub fn set_fixed_port(&self, enabled: bool) {
        self.fixed_port.store(enabled, Ordering::Relaxed);
    }

    pub fn start(&self, device: Device, args: Vec<String>, events: mpsc::Sender<Event>) -> Session {
        let stop = Arc::new(AtomicBool::new(false));
        let audio_generation = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_generation = Arc::clone(&audio_generation);
        let tools = self.clone();
        let worker = thread::spawn(move || match audio_args(args) {
            Ok(args) => tools.run(device, &args, &events, &worker_stop, &worker_generation),
            Err(reason) => {
                let _ = events.send(Event::Status(reason));
            }
        });
        Session {
            stop,
            audio_generation,
            worker: Some(worker),
        }
    }

    fn adb(&self, args: &[&str], stop: &AtomicBool) -> Result<String, String> {
        capture(command(&self.adb).args(args), None, ADB_TIMEOUT, stop)
    }

    fn run(
        &self,
        mut device: Device,
        args: &[String],
        events: &mpsc::Sender<Event>,
        stop: &AtomicBool,
        audio_generation: &AtomicU64,
    ) {
        let mut reported = String::new();
        let mut status = |value: String| {
            if value != reported {
                reported.clone_from(&value);
                let _ = events.send(Event::Status(value));
            }
        };
        let mut backoff = Duration::from_secs(2);
        let mut last_switch = None::<Instant>;
        let mut saved_endpoint = (device.addr.clone(), device.identity.clone());
        while !stopped(stop) {
            let attempt = Instant::now();
            let result =
                self.locate(&device, stop)
                    .and_then(|(mut addr, mut transport, identity)| {
                        if self.fixed_port.load(Ordering::Relaxed)
                            && let (Some(mut socket), Some(serial)) =
                                (endpoint(&addr), identity.as_ref())
                            && !matches!(socket.port(), 5555 | 5556)
                            && last_switch
                                .is_none_or(|when| when.elapsed() >= Duration::from_secs(600))
                        {
                            last_switch = Some(Instant::now());
                            socket.set_port(5556);
                            let fixed = socket.to_string();
                            status("正在尝试固定端口 5556…".into());
                            if self
                                .adb(&["-s", &transport, "tcpip", "5556"], stop)
                                .is_ok_and(|out| out.contains("restarting"))
                            {
                                // The native transport has closed. Keep the candidate locally
                                // until the hardware serial is verified over the new transport.
                                device.identity = Some(serial.clone());
                                device.addr.clone_from(&fixed);
                                pause(Duration::from_secs(1), stop);
                                self.identify(&fixed, Some(serial), stop)?;
                                addr.clone_from(&fixed);
                                transport = fixed;
                            }
                        }
                        if let Some(identity) = identity {
                            device.addr.clone_from(&addr);
                            device.identity = Some(identity.clone());
                            if saved_endpoint.0 != addr
                                || saved_endpoint.1.as_ref() != Some(&identity)
                            {
                                saved_endpoint = (addr.clone(), Some(identity.clone()));
                                let _ = events.send(Event::Identified {
                                    addr: addr.clone(),
                                    identity,
                                });
                            }
                        }
                        status("正在启动音频…".into());
                        self.stream(&transport, args, stop, audio_generation, &mut status)
                    });
            if stopped(stop) {
                break;
            }
            match result {
                Err(reason) => {
                    status(reason);
                    if attempt.elapsed() >= Duration::from_secs(30) {
                        backoff = Duration::from_secs(2);
                    }
                    if !pause(backoff, stop) {
                        break;
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(()) => {
                    backoff = Duration::from_secs(2);
                }
            }
        }
    }

    fn locate(
        &self,
        device: &Device,
        stop: &AtomicBool,
    ) -> Result<(String, String, Option<String>), String> {
        let addr = &device.addr;
        let current = self.identify(addr, device.identity.as_deref(), stop);
        let current_socket = endpoint(addr);
        let legacy = current_socket.is_some_and(|socket| matches!(socket.port(), 5555 | 5556));
        if current.is_ok() && (!legacy || self.fixed_port.load(Ordering::Relaxed)) {
            return current.map(|serial| (addr.clone(), addr.clone(), serial));
        }

        let expected = device
            .identity
            .as_deref()
            .or_else(|| current.as_ref().ok().and_then(|serial| serial.as_deref()));
        if let (Some(expected), Some(socket)) = (expected, current_socket) {
            if let Some((addr, transport)) =
                self.rediscover(addr, socket.ip(), expected, current.is_ok(), stop)
            {
                return Ok((addr, transport, Some(expected.to_owned())));
            }
        }

        let has_identity = expected.is_some();
        current
            .map(|serial| (addr.clone(), addr.clone(), serial))
            .map_err(|reason| {
                if !has_identity && current_socket.is_some() {
                    format!("{reason}。首次识别前若地址已变，请扫描并选择设备")
                } else {
                    reason
                }
            })
    }

    fn rediscover(
        &self,
        previous: &str,
        previous_ip: IpAddr,
        expected: &str,
        native_only: bool,
        stop: &AtomicBool,
    ) -> Option<(String, String)> {
        let mut services = self.discover_services(stop).ok()?;
        services.sort_by_key(|(_, tls)| !*tls);
        let online = self.adb(&["devices"], stop).ok()?;
        for (found, tls) in services {
            if stopped(stop) {
                return None;
            }
            if found.pairing || (native_only && !tls) {
                continue;
            }
            let alias = format!("{}._adb-tls-connect._tcp", found.name);
            let alias_is_online = tls && device_state(&online, &alias) == Some("device");
            if found.addr == previous && !alias_is_online {
                continue;
            }
            let transport = if alias_is_online {
                alias
            } else {
                let serial_hint = found.name.strip_prefix("adb-").is_some_and(|name| {
                    name.strip_prefix(expected)
                        .is_some_and(|suffix| suffix.starts_with('-'))
                });
                let same_host =
                    endpoint(&found.addr).is_some_and(|socket| socket.ip() == previous_ip);
                let already_online = device_state(&online, &found.addr) == Some("device");
                if !(already_online || (tls && (same_host || serial_hint))) {
                    continue;
                }
                found.addr.clone()
            };
            if self.identify(&transport, Some(expected), stop).is_ok() {
                return Some((found.addr, transport));
            }
        }
        None
    }

    fn identify(
        &self,
        addr: &str,
        expected: Option<&str>,
        stop: &AtomicBool,
    ) -> Result<Option<String>, String> {
        if addr.is_empty() || addr.starts_with('-') || addr.chars().any(char::is_whitespace) {
            return Err("设备地址无效".into());
        }
        let mut state = self.state(addr, stop)?;
        if let Some(socket) = endpoint(addr) {
            if state.as_deref() != Some("device") && state.as_deref() != Some("unauthorized") {
                if state.as_deref() == Some("offline") {
                    self.adb(&["disconnect", addr], stop)?;
                }
                if stopped(stop)
                    || TcpStream::connect_timeout(&socket, Duration::from_millis(700)).is_err()
                {
                    return Err(format!(
                        "正在等待 {addr}。请检查平板已接入同一 Wi-Fi 并开启无线调试"
                    ));
                }
                self.adb(&["connect", addr], stop)?;
                state = self.state(addr, stop)?;
            }
        }
        match state.as_deref() {
            Some("device") => {}
            Some("unauthorized") => return Err(format!("等待设备授权或重新配对（{addr}）")),
            _ => {
                return Err(format!(
                    "设备离线，正在重试。请在平板上检查无线调试（{addr}）"
                ));
            }
        }

        let serial = self
            .adb(&["-s", addr, "shell", "getprop", "ro.serialno"], stop)
            .ok()
            .and_then(|output| stable_serial(&output));
        if expected.is_some() && serial.as_deref() != expected {
            return Err(format!("无法验证设备身份（{addr}）"));
        }
        Ok(serial)
    }

    fn state(&self, addr: &str, stop: &AtomicBool) -> Result<Option<String>, String> {
        self.adb(&["devices"], stop)
            .map(|output| device_state(&output, addr).map(str::to_owned))
    }

    fn stream(
        &self,
        addr: &str,
        args: &[String],
        stop: &AtomicBool,
        audio_generation: &AtomicU64,
        status: &mut impl FnMut(String),
    ) -> Result<(), String> {
        let local_generation = audio_generation.load(Ordering::Relaxed);
        if stopped(stop) {
            return Ok(());
        }
        // Keep each device on its own PC-side tunnel, even during simultaneous starts.
        let explicit_port = args
            .iter()
            .any(|a| a == "--port" || a.starts_with("--port=") || a.starts_with("-p"));
        let port = if explicit_port {
            None
        } else {
            Some(TunnelPort::allocate()?)
        };
        let mut child = command(&self.scrcpy)
            .env("ADB", &self.adb)
            .env_remove("ANDROID_SERIAL")
            .args(["-s", addr])
            .args(args)
            .args(port.as_ref().map(|p| format!("--port={}", p.0)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("无法启动 scrcpy：{error}"))?;
        let (sender, lines) = mpsc::sync_channel(256);
        read_lines(child.stdout.take().unwrap(), sender.clone());
        read_lines(child.stderr.take().unwrap(), sender);
        let mut last_ready = Instant::now();
        let mut next_check = Instant::now();
        #[cfg(not(windows))]
        let mut stream_seen = false;
        loop {
            if stopped(stop) || audio_generation.load(Ordering::Relaxed) != local_generation {
                terminate(&mut child);
                return Ok(());
            }
            for line in lines.try_iter().take(256) {
                #[cfg(not(windows))]
                if line.contains("Demuxer 'audio': starting thread") {
                    stream_seen = true;
                }
                if let Some(error) = scrcpy_error(&line) {
                    terminate(&mut child);
                    return Err(format!("scrcpy 音频错误：{error}"));
                }
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    let deadline = Instant::now() + Duration::from_millis(150);
                    while let Ok(line) =
                        lines.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        if let Some(error) = scrcpy_error(&line) {
                            return Err(format!("scrcpy 音频错误：{error}"));
                        }
                    }
                    return Err(format!("scrcpy 已退出，正在重试（{exit}）"));
                }
                Ok(None) => {}
                Err(error) => {
                    terminate(&mut child);
                    return Err(format!("无法读取 scrcpy 状态：{error}"));
                }
            }
            if Instant::now() >= next_check {
                next_check = Instant::now() + Duration::from_secs(2);
                #[cfg(windows)]
                let ready = crate::route::player_active(child.id()).map_err(|e| e.to_string());
                #[cfg(not(windows))]
                let ready = Ok::<bool, String>(stream_seen);
                match ready {
                    Ok(true) => {
                        last_ready = Instant::now();
                        status("共享会话运行中".into());
                    }
                    Ok(false) if last_ready.elapsed() >= Duration::from_secs(20) => {
                        terminate(&mut child);
                        return Err("Windows 未建立播放会话，正在重新连接此设备".into());
                    }
                    Ok(false) => status("正在等待 Windows 音频输出…".into()),
                    Err(_) => status("无法读取 Windows 播放状态".into()),
                }
            }
            thread::sleep(POLL);
        }
    }
}

// The OS selects a free port. Keep a process-local lease after releasing the
// socket so two workers cannot select the same port before adb installs it.
struct TunnelPort(u16);

fn leased_ports() -> &'static std::sync::Mutex<std::collections::HashSet<u16>> {
    static PORTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u16>>> =
        std::sync::OnceLock::new();
    PORTS.get_or_init(Default::default)
}

impl TunnelPort {
    fn allocate() -> Result<Self, String> {
        for _ in 0..32 {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("无法分配音频端口：{e}"))?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            if leased_ports().lock().unwrap().insert(port) {
                return Ok(Self(port));
            }
        }
        Err("暂时没有可用的独立音频端口".into())
    }
}

impl Drop for TunnelPort {
    fn drop(&mut self) {
        leased_ports().lock().unwrap().remove(&self.0);
    }
}

fn audio_args(mut args: Vec<String>) -> Result<Vec<String>, String> {
    if let Some(arg) = args.iter().find(|arg| {
        let option = arg.split('=').next().unwrap_or("");
        matches!(
            option,
            "--tcpip"
                | "--serial"
                | "--select-usb"
                | "--select-tcpip"
                | "--no-audio"
                | "--no-audio-playback"
                | "--no-playback"
                | "--otg"
                | "--kill-adb-on-close"
                | "--pause-on-exit"
        ) || arg.starts_with("-s")
            || matches!(arg.as_str(), "-d" | "-e")
    }) {
        return Err(format!("scrcpy 参数与指定设备或音频播放冲突（{arg}）"));
    }
    for flag in [
        "--no-video",
        "--no-window",
        "--require-audio",
        "--no-control",
        "--force-adb-forward",
    ] {
        if !args.iter().any(|arg| arg == flag) {
            args.push(flag.to_owned());
        }
    }
    Ok(args)
}

fn check_pair_endpoint(addr: &str, services: &[(Found, bool)]) -> Result<(), String> {
    let Some(target) = endpoint(addr) else {
        return Err("配对地址无效".into());
    };
    let mut pairing = services
        .iter()
        .filter(|(service, _)| service.pairing)
        .filter_map(|(service, _)| endpoint(&service.addr))
        .filter(|socket| socket.ip() == target.ip());
    // A manually entered address can be valid even when multicast is unavailable.
    if pairing.clone().any(|socket| socket == target) {
        return Ok(());
    }
    let connection = services
        .iter()
        .any(|(service, _)| !service.pairing && endpoint(&service.addr) == Some(target));
    if connection {
        let hint = pairing
            .next()
            .map(|s| format!("当前发现的配对地址为 {s}。"))
            .unwrap_or_default();
        return Err(format!(
            "填写的是无线调试的连接端口。{hint}请使用平板「使用配对码配对设备」弹窗里的地址和六位码。"
        ));
    }
    // Android may change the pairing port when the dialog is reopened. Other
    // advertisements can be stale too, so an unrecognized address is still tried.
    Ok(())
}

fn pairing_result(result: Result<String, String>, code: &str) -> Result<(), String> {
    let success = result.is_ok();
    let raw = result.unwrap_or_else(|reason| reason);
    let detail = raw
        .replace("Enter pairing code:", "")
        .replace(code, "******");
    let detail = detail.trim();
    if success
        && detail
            .lines()
            .any(|line| line.trim().starts_with("Successfully paired to "))
    {
        return Ok(());
    }
    let lower = detail.to_ascii_lowercase();
    if lower.contains("protocol fault")
        || lower.contains("handshake")
        || lower.contains("failed to pair")
    {
        return Err("配对握手失败。请重新打开平板的「使用配对码配对设备」，保持弹窗打开，用同一弹窗里的端口和新六位码重试。".into());
    }
    if lower.contains("failed to connect")
        || lower.contains("cannot connect")
        || detail.contains("超时")
    {
        return Err("配对服务未响应。请确认电脑和平板在同一 Wi-Fi，保持配对弹窗打开，核对当前配对端口后重试。".into());
    }
    Err(format!("配对失败：{detail}"))
}

fn wifi_ipv4(output: &str) -> Option<std::net::Ipv4Addr> {
    let mut fields = output.split_whitespace();
    while let Some(field) = fields.next() {
        if field == "inet" {
            let ip: std::net::Ipv4Addr = fields.next()?.split('/').next()?.parse().ok()?;
            if !ip.is_unspecified() && !ip.is_loopback() {
                return Some(ip);
            }
        }
    }
    None
}

fn endpoint(addr: &str) -> Option<SocketAddr> {
    let socket: SocketAddr = addr.parse().ok()?;
    (socket.port() != 0).then_some(socket)
}

fn parse_mdns(output: &str) -> Vec<(Found, bool)> {
    let mut found: Vec<(Found, bool)> = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(index) = fields.iter().position(|field| {
            matches!(
                field.trim_end_matches('.'),
                "_adb._tcp" | "_adb-tls-connect._tcp" | "_adb-tls-pairing._tcp"
            )
        }) else {
            continue;
        };
        let Some(addr) = fields.iter().rev().find(|field| endpoint(field).is_some()) else {
            continue;
        };
        let pairing = fields[index].starts_with("_adb-tls-pairing");
        let tls = fields[index].starts_with("_adb-tls-connect");
        let name = index
            .checked_sub(1)
            .map(|before| fields[before])
            .or_else(|| {
                fields
                    .get(index + 1)
                    .copied()
                    .filter(|field| endpoint(field).is_none())
            })
            .unwrap_or(addr);
        if !found
            .iter()
            .any(|(item, native)| item.addr == *addr && item.pairing == pairing && *native == tls)
        {
            found.push((
                Found {
                    name: name.into(),
                    addr: (*addr).into(),
                    identity: None,
                    pairing,
                },
                tls,
            ));
        }
    }
    found
}

fn device_state<'a>(output: &'a str, addr: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next()? == addr).then(|| fields.next()).flatten()
    })
}

fn stable_serial(output: &str) -> Option<String> {
    // Keep only a single getprop value, excluding generic firmware placeholders.
    let serial = output.trim();
    (!serial.is_empty()
        && serial.len() <= 128
        && !["unknown", "null", "none", "0123456789abcdef", "1234567890"]
            .iter()
            .any(|value| serial.eq_ignore_ascii_case(value))
        && !serial.bytes().all(|byte| byte == b'0')
        && serial.parse::<IpAddr>().is_err()
        && serial
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then(|| serial.to_owned())
}

fn scrcpy_error(line: &str) -> Option<&str> {
    line.trim()
        .split_once("ERROR:")
        .map(|(_, message)| message.trim())
}

fn command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    cmd.env("ANDROID_ADB_SERVER_PORT", PORT)
        .env("ADB_SERVER_SOCKET", format!("tcp:localhost:{PORT}"))
        .env_remove("ANDROID_ADB_SERVER_ADDRESS");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
}

fn pause(duration: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    while !stopped(stop) && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    !stopped(stop)
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_lines(stream: impl Read + Send + 'static, sender: mpsc::SyncSender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
            if let Err(mpsc::TrySendError::Disconnected(_)) =
                sender.try_send(String::from_utf8_lossy(&line).into_owned())
            {
                break;
            }
        }
    });
}

fn read_output(pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut pipe = BufReader::new(pipe);
        let mut output = Vec::new();
        let _ = (&mut pipe).take(256 * 1024).read_to_end(&mut output);
        let _ = sender.send(String::from_utf8_lossy(&output).into_owned());
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
    });
    receiver
}

fn capture(
    command: &mut Command,
    input: Option<&str>,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<String, String> {
    if stopped(stop) {
        return Err("操作已取消".into());
    }
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 ADB：{error}"))?;
    let stdout = read_output(child.stdout.take().unwrap());
    let stderr = read_output(child.stderr.take().unwrap());
    if let Some(input) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        if stopped(stop) || Instant::now() >= deadline {
            terminate(&mut child);
            return Err(if stopped(stop) {
                "操作已取消"
            } else {
                "ADB 命令超时"
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                let collect = |pipe: mpsc::Receiver<String>| {
                    while !stopped(stop) && Instant::now() < deadline {
                        match pipe.recv_timeout(
                            POLL.min(deadline.saturating_duration_since(Instant::now())),
                        ) {
                            Ok(output) => return Ok(output),
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                    Err("无法读取 ADB 输出".to_owned())
                };
                let stdout = collect(stdout)?;
                let stderr = collect(stderr)?;
                if exit.success() {
                    return Ok(stdout.trim().to_owned());
                }
                let details = format!("{} {}", stderr.trim(), stdout.trim());
                return Err(format!("ADB 命令失败（{exit}）{}", details.trim()));
            }
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                terminate(&mut child);
                return Err(format!("无法读取 ADB 状态：{error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_and_legacy_advertisements() {
        let found = parse_mdns(concat!(
            "List of discovered mdns services\r\n",
            "adb-serial-link\t_adb-tls-connect._tcp.\t192.168.0.2:9876\n",
            "adb-serial-pair\t_adb-tls-pairing._tcp\t[fe80::2]:1234\n",
            "_adb._tcp legacy 192.168.0.3:5556\n",
            "repeat _adb._tcp 192.168.0.3:5556\n",
            "ignored _other._tcp 192.168.0.5:4321\n",
        ));
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].0.name, "adb-serial-link");
        assert!(!found[0].0.pairing);
        assert!(found[1].0.pairing);
        assert_eq!(found[2].0.name, "legacy");
    }

    #[test]
    fn ignores_serial_placeholders_and_adb_errors() {
        assert_eq!(stable_serial("  ABC123\r\n").as_deref(), Some("ABC123"));
        for invalid in [
            "",
            "unknown",
            "000000",
            "0123456789ABCDEF",
            "error: device offline",
            "ABC\nBAD",
        ] {
            assert_eq!(stable_serial(invalid), None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scan_prefers_verified_tls_address_and_usb_recovery_ignores_other_tablets() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        fs::write(
            &adb,
            r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
echo "$*" >> commands
case "$1" in
mdns)
 echo 'adb-ABC-link _adb-tls-connect._tcp 192.0.2.2:38000'
 echo 'adb-pair _adb-tls-pairing._tcp 192.0.2.3:39000'
 echo 'unknown _adb-tls-connect._tcp 192.0.2.4:30001'
 ;;
devices)
 echo 'ABC device'
 echo 'UNRELATED device'
 echo '192.0.2.2:5556 device'
 echo 'adb-ABC-link._adb-tls-connect._tcp device'
 ;;
-s)
 case "$*" in
  *ro.product.model) echo 'Tablet Model';;
  *ro.serialno) echo ABC;;
  *wlan0) echo 'inet 192.0.2.2/24 brd 192.0.2.255';;
 esac;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&adb, fs::Permissions::from_mode(0o700)).unwrap();
        let tools = Tools::discover(dir.path());
        let services = tools.scan().unwrap();
        assert_eq!(services.len(), 3);
        let known = services
            .iter()
            .find(|s| s.identity.as_deref() == Some("ABC"))
            .unwrap();
        assert_eq!(
            (&known.name[..], &known.addr[..]),
            ("Tablet Model", "192.0.2.2:38000")
        );
        assert!(
            services
                .iter()
                .filter(|s| s.addr != known.addr)
                .all(|s| s.identity.is_none())
        );
        tools.set_usb_recovery(true);
        let mut device = Device::new(5, "Pad".into(), "192.0.2.1:5556".into());
        device.identity = Some("ABC".into());
        assert_eq!(
            tools.recover_usb(&[device]).unwrap(),
            vec![(5, "192.0.2.2:5556".into(), "ABC".into())]
        );
        let commands = fs::read_to_string(dir.path().join("commands")).unwrap();
        assert!(!commands.contains("-s UNRELATED"));
        assert!(!commands.contains("tcpip") && !commands.contains("connect "));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn optional_fixed_port_is_verified_before_it_is_saved() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        let player = dir.path().join("scrcpy");
        fs::write(&adb, r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
case "$1" in
devices) if [ -e switched ]; then echo '127.0.0.1:5556 device'; else echo '127.0.0.1:39000 device'; fi;;
-s) if [ "$3" = tcpip ]; then touch switched; echo 'restarting in TCP mode port: 5556'; else echo TESTSERIAL; fi;;
esac
"#).unwrap();
        fs::write(&player, "#!/bin/sh\nexec sleep 20\n").unwrap();
        for path in [&adb, &player] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let tools = Tools::discover(dir.path());
        tools.set_fixed_port(true);
        let (send, receive) = mpsc::channel();
        let session = tools.start(
            Device::new(1, "Pad".into(), "127.0.0.1:39000".into()),
            vec![],
            send,
        );
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if let Event::Identified { addr, identity } = receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                assert_eq!(
                    (addr.as_str(), identity.as_str()),
                    ("127.0.0.1:5556", "TESTSERIAL")
                );
                break;
            }
        }
        drop(session);
    }

    #[test]
    fn distinguishes_pairing_port_from_connect_port_without_requiring_mdns() {
        let services = parse_mdns(
            "one _adb-tls-connect._tcp 192.0.2.1:32000\none _adb-tls-pairing._tcp 192.0.2.1:32100",
        );
        assert!(
            check_pair_endpoint("192.0.2.1:32000", &services)
                .unwrap_err()
                .contains("192.0.2.1:32100")
        );
        assert!(check_pair_endpoint("192.0.2.1:32100", &services).is_ok());
        assert!(check_pair_endpoint("192.0.2.1:32200", &[]).is_ok());
    }

    #[test]
    fn handles_pairing_prompt_protocol_error_and_sensitive_output() {
        assert!(
            pairing_result(
                Ok("Enter pairing code: Successfully paired to example".into()),
                "012345"
            )
            .is_ok()
        );
        let error = pairing_result(
            Err(
                "protocol fault (couldn't read status message): No error Enter pairing code:"
                    .into(),
            ),
            "012345",
        )
        .unwrap_err();
        assert!(error.contains("握手失败") && !error.contains("Enter pairing code"));
        assert!(
            !pairing_result(Ok("unexpected response 012345".into()), "012345")
                .unwrap_err()
                .contains("012345")
        );
    }

    #[test]
    fn concurrent_players_use_distinct_local_ports() {
        let a = TunnelPort::allocate().unwrap();
        let b = TunnelPort::allocate().unwrap();
        assert_ne!(a.0, b.0);
        assert!(
            audio_args(vec![])
                .unwrap()
                .iter()
                .any(|a| a == "--force-adb-forward")
        );
    }

    #[test]
    fn looks_up_the_exact_transport() {
        let output = "List of devices attached\n192.168.0.2:98760\tdevice\n192.168.0.2:9876\toffline\nUSB123\tunauthorized\n";
        assert_eq!(device_state(output, "192.168.0.2:9876"), Some("offline"));
        assert_eq!(device_state(output, "USB123"), Some("unauthorized"));
        assert_eq!(device_state(output, "USB12"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn session_verifies_dhcp_change_and_reaps_its_player() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        let scrcpy = dir.path().join("scrcpy");
        let state = dir.path().join("rebooted");
        let players = dir.path().join("players");
        let commands = dir.path().join("adb-commands");
        fs::write(
            &adb,
            r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
[ "$ANDROID_ADB_SERVER_PORT" = 5038 ] || exit 1
printf '%s\n' "$*" >> adb-commands
case "$1" in
    devices)
        printf 'List of devices attached\n'
        if [ -f rebooted ]; then
            printf '127.0.0.1:5556\tunauthorized\n'
            printf 'adb-other-link._adb-tls-connect._tcp\tdevice\n'
            printf 'adb-target-link._adb-tls-connect._tcp\tdevice\n'
        else
            printf '127.0.0.1:5556\tdevice\n'
        fi
        ;;
    mdns)
        if [ -f rebooted ]; then
            printf 'adb-unknown-link\t_adb-tls-connect._tcp\t192.0.2.5:45678\n'
            printf 'adb-other-link\t_adb-tls-connect._tcp\t192.0.2.6:45678\n'
            printf 'adb-target-link\t_adb-tls-connect._tcp\t192.0.2.7:45678\n'
        fi
        ;;
    pair)
        read -r code
        [ "$code" = 123456 ] || exit 1
        printf 'Enter pairing code: Successfully paired to %s\n' "$2"
        ;;
    -s)
        case "$2" in
            adb-other-*) printf 'OTHER\n' ;;
            *) printf 'TARGET\n' ;;
        esac
        ;;
esac
"#,
        )
        .unwrap();
        fs::write(
            &scrcpy,
            r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
printf '%s %s\n' "$$" "$*" >> players
echo "DEBUG: Demuxer 'audio': starting thread"
exec sleep 30
"#,
        )
        .unwrap();
        for path in [&adb, &scrcpy] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let tools = Tools::discover(dir.path());
        assert_eq!(tools.scan().unwrap()[0].addr, "127.0.0.1:5556");
        tools.pair("192.0.2.7:12345", "123456").unwrap();
        let (events, output) = mpsc::channel();
        let device = Device::new(1, "device".into(), "127.0.0.1:5556".into());
        let session = tools.start(device, Vec::new(), events);
        let wait_for = |addr: &str| {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut identified = false;
            loop {
                let event = output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                match event {
                    Event::Identified {
                        addr: actual,
                        identity,
                    } => {
                        assert_eq!(actual, addr);
                        assert_eq!(identity, "TARGET");
                        identified = true;
                    }
                    Event::Status(status) if status == "共享会话运行中" => break,
                    Event::Status(_) => {}
                }
            }
            assert!(identified);
        };
        wait_for("127.0.0.1:5556");
        fs::write(state, "").unwrap();
        session.restart_audio();
        wait_for("192.0.2.7:45678");
        drop(session);

        let log = fs::read_to_string(players).unwrap();
        let players: Vec<_> = log.lines().collect();
        assert_eq!(players.len(), 2);
        assert!(players[0].contains("-s 127.0.0.1:5556 --no-video --no-window --require-audio"));
        assert!(players[1].contains("-s adb-target-link._adb-tls-connect._tcp"));
        for player in players {
            let pid = player.split_whitespace().next().unwrap();
            assert!(
                !Path::new("/proc").join(pid).exists(),
                "player {pid} survived session shutdown"
            );
        }
        assert!(
            !fs::read_to_string(commands)
                .unwrap()
                .lines()
                .any(|line| line.starts_with("connect "))
        );
    }
}
