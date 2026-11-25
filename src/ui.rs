mod automation;
mod icons;
mod input;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use gpui::{
    App, Application, Bounds, BoxShadow, ClickEvent, Context, Div, ElementId, Entity, Focusable,
    FontWeight, Rgba, Stateful, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, point, prelude::*, px, rgb, rgba, size,
};

use crate::{
    config::{Config, Device},
    engine::{Event, Session, Tools},
    route,
};
use automation::Automation;
use icons::{
    ICON_ALERT, ICON_AUDIO, ICON_CHECK, ICON_CLOSE, ICON_EDIT, ICON_PLAY, ICON_PLUS, ICON_SCAN,
    ICON_SLIDERS, ICON_STOP, ICON_TRASH, UiAssets, icon,
};
use input::{InputEvent, TextInput};

const UI_FONT: &str = "Microsoft YaHei UI";
const MONO_FONT: &str = "Consolas";

// Warm architectural light console palette
const BG_CANVAS: u32 = 0xf3f2ee;
const BG_BAR: u32 = 0xf9f8f5;
const BG_SURFACE: u32 = 0xffffff;
const BG_SUBTLE: u32 = 0xf7f6f2;
const BG_HOVER: u32 = 0xeeece6;
const BG_PRESSED: u32 = 0xe5e2da;

const BG_LIVE: u32 = 0xf2faf8;
const BG_CONNECTING: u32 = 0xfffcf2;
const BG_ERROR: u32 = 0xfff6f6;

const BORDER_HAIRLINE: u32 = 0xe3e0d8;
const BORDER_CONTROL: u32 = 0xd4d0c7;
const BORDER_LIVE: u32 = 0xbadfd8;
const BORDER_CONNECTING: u32 = 0xefd79d;
const BORDER_ERROR: u32 = 0xf2c6c6;

const INK_PRIMARY: u32 = 0x181a1b;
const INK_SECONDARY: u32 = 0x565c62;
const INK_MUTED: u32 = 0x82888f;

const ACCENT: u32 = 0x0f766e;
const ACCENT_HOVER: u32 = 0x0c635c;
const ACCENT_SOFT: u32 = 0xe5f4f1;
const ACCENT_TEXT: u32 = 0x0d615a;

const LIVE_DOT: u32 = 0x10b981;
const AMBER_DOT: u32 = 0xd97706;
const AMBER_SOFT: u32 = 0xfef3c7;
const AMBER_TEXT: u32 = 0x92400e;

const DANGER: u32 = 0xb91c1c;
const DANGER_HOVER: u32 = 0x991b1b;
const DANGER_DOT: u32 = 0xdc2626;
const DANGER_SOFT: u32 = 0xfee2e2;

pub fn run(root: PathBuf) {
    Application::new()
        .with_assets(UiAssets)
        .run(move |cx: &mut App| {
            input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(760.), px(640.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(520.), px(440.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("音频共享台".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |_, cx| cx.new(|cx| Hub::new(root, cx)),
            )
            .expect("创建音频共享台窗口失败");
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

struct Active {
    status: String,
    session: Session,
    events: Receiver<Event>,
}

struct Stopping {
    worker: thread::JoinHandle<()>,
    restart: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Console,
    Automation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddMode {
    Manual,
    Pair,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Info,
    Success,
    Error,
}

#[derive(Clone)]
struct Notice {
    text: String,
    tone: NoticeTone,
}

enum ChannelState {
    Stopped,
    Stopping,
    Restarting,
    Live,
    Connecting(String),
    Attention(String),
}

struct DeviceEditor {
    id: u64,
    original_address: String,
}

struct Hub {
    root: PathBuf,
    tools: Tools,
    config: Config,
    config_error: Option<String>,
    active: BTreeMap<u64, Active>,
    stopping: BTreeMap<u64, Stopping>,
    editing: Option<DeviceEditor>,
    confirm_remove: Option<u64>,
    add_open: bool,
    add_mode: AddMode,
    page: Page,
    last_device_id: u64,
    automation: Automation,
    rename: Entity<TextInput>,
    edit_address: Entity<TextInput>,
    routes: Receiver<()>,
    address: Entity<TextInput>,
    name: Entity<TextInput>,
    pair_address: Entity<TextInput>,
    pair_code: Entity<TextInput>,
    buffer: Entity<TextInput>,
    notice: Option<Notice>,
    _subscriptions: Vec<Subscription>,
}

impl Hub {
    fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (config, config_error) = match Config::load(&root) {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error)),
        };

        let rename = cx.new(|cx| TextInput::new("输入新的设备名称", cx));
        let address = cx.new(|cx| TextInput::new("192.168.1.10:5555", cx).with_monospace());
        let edit_address = cx.new(|cx| TextInput::new("IP:端口", cx).with_monospace());
        let name = cx.new(|cx| TextInput::new("设备名称（可选，如客厅平板）", cx));
        let pair_address = cx.new(|cx| TextInput::new("192.168.1.10:37123", cx).with_monospace());
        let pair_code = cx.new(|cx| TextInput::new("6 位配对码", cx).with_monospace());
        let buffer = cx.new(|cx| {
            let mut field = TextInput::new("毫秒", cx).with_monospace().with_compact();
            field.set(buffer_value(&config.scrcpy_args).unwrap_or(""), cx);
            field
        });

        let subscriptions = vec![
            cx.subscribe(&rename, |hub, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => {
                    if let Some(id) = hub.editing_id() {
                        hub.save_device(id, cx);
                        cx.notify();
                    }
                }
                InputEvent::Cancel => {
                    hub.editing = None;
                    hub.confirm_remove = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&edit_address, |hub, _, event: &InputEvent, cx| {
                match event {
                    InputEvent::Submit => {
                        if let Some(id) = hub.editing_id() {
                            hub.save_device(id, cx);
                        }
                    }
                    InputEvent::Cancel => {
                        hub.editing = None;
                        hub.confirm_remove = None;
                    }
                }
                cx.notify();
            }),
            cx.subscribe(&address, |hub, _, event: &InputEvent, cx| {
                if *event == InputEvent::Submit {
                    hub.submit_manual_add(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&name, |hub, _, event: &InputEvent, cx| {
                if *event == InputEvent::Submit {
                    hub.submit_manual_add(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&pair_address, |hub, _, event: &InputEvent, cx| {
                if *event == InputEvent::Submit {
                    hub.submit_pair(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&pair_code, |hub, _, event: &InputEvent, cx| {
                if *event == InputEvent::Submit {
                    hub.submit_pair(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&buffer, |hub, _, event: &InputEvent, cx| {
                if *event == InputEvent::Submit {
                    hub.save_buffer(cx);
                    cx.notify();
                }
            }),
            cx.observe(&buffer, |_, _, cx| {
                cx.notify();
            }),
        ];

        let executor = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            loop {
                executor.timer(Duration::from_millis(120)).await;
                if view.update(cx, |hub, cx| hub.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let last_device_id = config
            .devices
            .iter()
            .map(|device| device.id)
            .max()
            .unwrap_or(0);
        let mut hub = Self {
            tools: Tools::discover(&root),
            root,
            config,
            config_error,
            active: BTreeMap::new(),
            stopping: BTreeMap::new(),
            editing: None,
            confirm_remove: None,
            add_open: false,
            add_mode: AddMode::Manual,
            page: Page::Console,
            last_device_id,
            automation: Automation::new(),
            rename,
            edit_address,
            routes: route::watch(),
            address,
            name,
            pair_address,
            pair_code,
            buffer,
            notice: None,
            _subscriptions: subscriptions,
        };
        hub.tools.set_fixed_port(hub.config.fixed_port);
        hub.log("音频共享台已启动");
        if hub.config.restore_sessions {
            for id in hub.config.resume_ids.clone() {
                hub.start(id);
            }
        }
        hub.schedule_discovery();
        hub
    }

    fn set_notice(&mut self, tone: NoticeTone, text: impl Into<String>) {
        let text = text.into();
        self.log(text.clone());
        self.notice = Some(Notice { text, tone });
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let route_changed = self.routes.try_iter().fold(false, |_, _| true);
        if route_changed && !self.active.is_empty() {
            for active in self.active.values() {
                active.session.restart_audio();
            }
            self.log(format!(
                "音频输出已变更，重新绑定 {} 路会话",
                self.active.len()
            ));
        }

        let mut events = Vec::new();
        let mut ended = Vec::new();
        for (&id, active) in &mut self.active {
            loop {
                match active.events.try_recv() {
                    Ok(event) => events.push((id, event)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        ended.push(id);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }

        let mut changed = !events.is_empty() || !ended.is_empty() || route_changed;
        let mut identification_changed = false;
        let mut previous_devices = None;
        for (id, event) in events {
            match event {
                Event::Status(status) => {
                    if let Some(active) = self.active.get_mut(&id) {
                        active.status.clone_from(&status);
                    }
                    self.log(format!("{} · {status}", self.device_name(id)));
                }
                Event::Identified { addr, identity } => {
                    previous_devices.get_or_insert_with(|| self.config.devices.clone());
                    identification_changed |= self.apply_identification(id, &addr, &identity);
                }
            }
        }
        if identification_changed
            && let Some(previous_devices) = previous_devices
            && let Err(error) = self.persist()
        {
            self.config.devices = previous_devices;
            self.set_notice(NoticeTone::Error, error);
        }

        for id in ended {
            if let Some(active) = self.active.remove(&id) {
                self.set_notice(
                    NoticeTone::Info,
                    format!(
                        "{} · 共享会话已结束（{}）",
                        self.device_name(id),
                        active.status
                    ),
                );
            }
        }

        let completed: Vec<_> = self
            .stopping
            .iter()
            .filter_map(|(&id, item)| item.worker.is_finished().then_some(id))
            .collect();
        for id in completed {
            if let Some(item) = self.stopping.remove(&id) {
                let _ = item.worker.join();
                if item.restart {
                    self.start(id);
                }
            }
            changed = true;
        }

        changed |= self.poll_discovery(cx);
        if changed {
            cx.notify();
        }
    }

    fn device_name(&self, id: u64) -> String {
        self.config
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.clone())
            .unwrap_or_else(|| format!("设备 {id}"))
    }

    fn persist(&self) -> Result<(), String> {
        match &self.config_error {
            Some(error) => Err(format!("配置读取失败，无法保存：{error}")),
            None => self
                .config
                .save(&self.root)
                .map_err(|error| format!("保存失败：{error}")),
        }
    }

    fn submit_manual_add(&mut self, cx: &mut Context<Self>) {
        let address = self.address.read(cx).value().trim().to_owned();
        let name = self.name.read(cx).value().trim().to_owned();
        if self.add(&address, &name) {
            self.address.update(cx, |field, cx| field.set("", cx));
            self.name.update(cx, |field, cx| field.set("", cx));
        }
    }

    fn submit_pair(&mut self, cx: &mut Context<Self>) {
        if self.automation.pairing() {
            return;
        }
        let address = self.pair_address.read(cx).value().trim().to_owned();
        let code = self.pair_code.read(cx).value().trim().to_owned();
        if !valid_address(&address)
            || code.len() != 6
            || !code.bytes().all(|byte| byte.is_ascii_digit())
        {
            self.set_notice(NoticeTone::Error, "请输入有效的配对地址和 6 位数字配对码");
        } else {
            self.begin_pair(address, code);
        }
    }

    fn add(&mut self, address: &str, name: &str) -> bool {
        if !valid_address(address) {
            self.set_notice(
                NoticeTone::Error,
                "请输入有效的 IP 地址与端口（如 192.168.1.10:5555）",
            );
            return false;
        }
        if let Some(device) = self
            .config
            .devices
            .iter()
            .find(|device| same_address(&device.addr, address))
        {
            let existing_name = device.name.clone();
            self.set_notice(
                NoticeTone::Info,
                format!("该地址已存在于「{existing_name}」"),
            );
            return false;
        }
        let display_name = if name.is_empty() { address } else { name };
        if let Err(error) = self.append_device(address, display_name, None) {
            self.set_notice(NoticeTone::Error, error);
            return false;
        }
        match self.persist() {
            Ok(()) => {
                self.set_notice(NoticeTone::Success, format!("已添加设备「{display_name}」"));
                true
            }
            Err(error) => {
                self.config.devices.pop();
                self.set_notice(NoticeTone::Error, error);
                false
            }
        }
    }

    fn save_buffer(&mut self, cx: &App) {
        let Ok(value) = self.buffer.read(cx).value().trim().parse::<u32>() else {
            self.set_notice(NoticeTone::Error, "请输入有效的缓冲毫秒数");
            return;
        };
        self.apply_buffer(value);
    }

    fn apply_buffer(&mut self, value: u32) {
        let previous = self.config.scrcpy_args.clone();
        let args = &mut self.config.scrcpy_args;
        let value_str = value.to_string();
        match args
            .iter()
            .rposition(|arg| arg == "--audio-buffer" || arg.starts_with("--audio-buffer="))
        {
            Some(index) if args[index] != "--audio-buffer" => {
                args[index] = format!("--audio-buffer={value_str}");
            }
            Some(index) if args.get(index + 1).is_some_and(|arg| !arg.starts_with('-')) => {
                args[index + 1] = value_str;
            }
            Some(index) => args.insert(index + 1, value_str),
            None => args.extend(["--audio-buffer".into(), value_str]),
        }
        match self.persist() {
            Ok(()) => {
                let mut restarted = 0usize;
                if previous != self.config.scrcpy_args {
                    let active: Vec<_> = self.active.keys().copied().collect();
                    restarted = active.len();
                    for id in active {
                        self.stop(id, true);
                    }
                }
                if restarted > 0 {
                    self.set_notice(
                        NoticeTone::Success,
                        format!("已保存 {value} ms 缓冲，正在重启 {restarted} 路活动音频流"),
                    );
                } else {
                    self.set_notice(NoticeTone::Success, format!("已保存音频缓冲：{value} ms"));
                }
            }
            Err(error) => {
                self.config.scrcpy_args = previous;
                self.set_notice(NoticeTone::Error, error);
            }
        }
    }

    fn editing_id(&self) -> Option<u64> {
        self.editing.as_ref().map(|editor| editor.id)
    }

    fn open_editor(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        if self.editing_id() == Some(id) {
            self.editing = None;
            return;
        }
        let Some(device) = self.config.devices.iter().find(|device| device.id == id) else {
            return;
        };
        self.editing = Some(DeviceEditor {
            id,
            original_address: device.addr.clone(),
        });
        self.rename.update(cx, |field, cx| {
            field.set(&device.name, cx);
            field.select_all_text(cx);
        });
        self.edit_address
            .update(cx, |field, cx| field.set(&device.addr, cx));
        window.focus(&self.rename.read(cx).focus_handle(cx));
    }

    fn save_device(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(editor) = self.editing.as_ref().filter(|editor| editor.id == id) else {
            return;
        };
        let name = self.rename.read(cx).value().trim();
        let address_input = self.edit_address.read(cx).value().trim();
        let Some(index) = self
            .config
            .devices
            .iter()
            .position(|device| device.id == id)
        else {
            return;
        };
        let previous = self.config.devices[index].clone();
        let address = if address_input == editor.original_address {
            previous.addr.as_str()
        } else {
            address_input
        };
        let address_changed = previous.addr != address;
        if name.is_empty() {
            self.set_notice(NoticeTone::Error, "请输入设备名称");
            return;
        }
        if address_changed && !valid_address(address) {
            self.set_notice(NoticeTone::Error, "请输入有效的 IP 地址与端口");
            return;
        }
        if address_changed
            && self
                .config
                .devices
                .iter()
                .any(|device| device.id != id && same_address(&device.addr, address))
        {
            self.set_notice(NoticeTone::Error, "该连接地址已由另一台设备使用");
            return;
        }
        self.config.devices[index].name = name.into();
        self.config.devices[index].addr = address.into();
        if let Err(error) = self.persist() {
            self.config.devices[index] = previous;
            self.set_notice(NoticeTone::Error, error);
            return;
        }
        self.editing = None;
        self.confirm_remove = None;
        if address_changed && self.running(id) {
            self.stop(id, true);
        }
        self.set_notice(NoticeTone::Success, format!("已保存设备「{name}」"));
    }

    fn remove_device(&mut self, id: u64) {
        let Some(index) = self
            .config
            .devices
            .iter()
            .position(|device| device.id == id)
        else {
            return;
        };
        let device = self.config.devices.remove(index);
        let previous_excluded = self.config.excluded_identities.clone();
        if let Some(serial) = &device.identity
            && !self.config.excluded_identities.contains(serial)
        {
            self.config.excluded_identities.push(serial.clone());
        }
        let previous_resume = self.config.resume_ids.clone();
        self.config.resume_ids.retain(|&saved| saved != id);
        match self.persist() {
            Ok(()) => {
                self.stop(device.id, false);
                if self.editing_id() == Some(id) {
                    self.editing = None;
                }
                self.confirm_remove = None;
                self.set_notice(
                    NoticeTone::Success,
                    format!("已移除设备「{}」", device.name),
                );
            }
            Err(error) => {
                self.config.devices.insert(index, device);
                self.config.resume_ids = previous_resume;
                self.config.excluded_identities = previous_excluded;
                self.set_notice(NoticeTone::Error, error);
            }
        }
    }

    fn append_device(
        &mut self,
        address: &str,
        name: &str,
        identity: Option<String>,
    ) -> Result<(), String> {
        let id = self.last_device_id.checked_add(1).ok_or("设备编号已用尽")?;
        self.last_device_id = id;
        let mut device = Device::new(id, name.into(), address.into());
        device.identity = identity;
        self.config.devices.push(device);
        Ok(())
    }

    fn command_sessions(&mut self, ids: &[u64], enable: bool, restart: bool) {
        let previous = self.config.resume_ids.clone();
        let mut desired = previous.clone();
        desired.retain(|id| !ids.contains(id));
        if enable {
            desired.extend(ids.iter().copied());
            desired.sort_unstable();
            desired.dedup();
        }
        if desired != previous {
            self.config.resume_ids = desired;
            if let Err(error) = self.persist() {
                self.config.resume_ids = previous;
                self.set_notice(NoticeTone::Error, error);
            }
        }
        for &id in ids {
            if restart && self.running(id) {
                self.stop(id, true);
            } else if enable {
                self.start(id);
            } else {
                self.stop(id, false);
            }
        }
    }

    fn command_all(&mut self, enable: bool) {
        let ids: Vec<_> = self.config.devices.iter().map(|device| device.id).collect();
        self.command_sessions(&ids, enable, false);
    }

    fn running(&self, id: u64) -> bool {
        self.active.contains_key(&id) || self.stopping.get(&id).is_some_and(|item| item.restart)
    }

    fn channel_state(&self, id: u64) -> ChannelState {
        if let Some(active) = self.active.get(&id) {
            classify_active_status(&active.status)
        } else if let Some(item) = self.stopping.get(&id) {
            if item.restart {
                ChannelState::Restarting
            } else {
                ChannelState::Stopping
            }
        } else {
            ChannelState::Stopped
        }
    }

    fn stop(&mut self, id: u64, restart: bool) {
        if self.running(id) {
            let verb = if restart {
                "重启共享"
            } else {
                "停止共享"
            };
            self.log(format!("{} · {verb}", self.device_name(id)));
        }
        if let Some(item) = self.stopping.get_mut(&id) {
            item.restart = restart;
        } else if let Some(active) = self.active.remove(&id) {
            let worker = thread::spawn(move || drop(active));
            self.stopping.insert(id, Stopping { worker, restart });
        }
    }

    fn start(&mut self, id: u64) {
        if let Some(item) = self.stopping.get_mut(&id) {
            item.restart = true;
            return;
        }
        if self.active.contains_key(&id) {
            return;
        }
        let Some(device) = self.config.devices.iter().find(|device| device.id == id) else {
            return;
        };
        let device = device.clone();
        self.log(format!("{} · 开始共享", device.name));
        let (send, events) = mpsc::channel();
        let session = self
            .tools
            .start(device, self.config.scrcpy_args.clone(), send);
        self.active.insert(
            id,
            Active {
                status: "正在连接…".into(),
                session,
                events,
            },
        );
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.tools.set_usb_recovery(false);
        self.active.clear();
        for (_, item) in std::mem::take(&mut self.stopping) {
            let _ = item.worker.join();
        }
    }
}

fn classify_active_status(status: &str) -> ChannelState {
    if status == "共享会话运行中" {
        ChannelState::Live
    } else if status.starts_with("正在连接")
        || status.starts_with("正在启动")
        || status.starts_with("正在等待")
    {
        ChannelState::Connecting(status.to_owned())
    } else {
        ChannelState::Attention(status.to_owned())
    }
}

fn valid_address(value: &str) -> bool {
    value
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|address| address.port() != 0)
}

fn same_address(a: &str, b: &str) -> bool {
    a == b
        || a.parse::<std::net::SocketAddr>()
            .is_ok_and(|address| b.parse() == Ok(address))
}

fn buffer_value(args: &[String]) -> Option<&str> {
    args.iter().enumerate().rev().find_map(|(index, arg)| {
        if arg == "--audio-buffer" {
            args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--audio-buffer=")
        }
    })
}

fn card_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba(0x181a1b0d).into(),
        offset: point(px(0.), px(1.)),
        blur_radius: px(2.),
        spread_radius: px(0.),
    }]
}

fn pill_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba(0x181a1b14).into(),
        offset: point(px(0.), px(1.)),
        blur_radius: px(2.),
        spread_radius: px(0.),
    }]
}

#[derive(Clone, Copy)]
enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    DangerSolid,
    DangerGhost,
    StopAction,
    ActiveToggle,
}

fn action_button(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    icon_path: Option<&'static str>,
    kind: ButtonKind,
    enabled: bool,
    compact: bool,
) -> Stateful<Div> {
    let label = label.into();
    let height = if compact { px(30.) } else { px(34.) };
    let pad_x = if compact { px(10.) } else { px(13.) };
    let font_size = if compact { px(12.5) } else { px(13.) };
    let icon_size = if compact { 12.5 } else { 13.5 };

    let mut el = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .flex_none()
        .h(height)
        .px(pad_x)
        .rounded(px(7.))
        .text_size(font_size)
        .line_height(px(18.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap();

    if !enabled {
        el = el
            .bg(rgb(BG_SUBTLE))
            .border_1()
            .border_color(rgb(BORDER_HAIRLINE))
            .text_color(rgb(0xa3a8ae));
        if let Some(path) = icon_path {
            el = el.child(icon(path, icon_size, rgb(0xa3a8ae)));
        }
        return el.child(label);
    }

    let icon_color: Rgba;
    el = match kind {
        ButtonKind::Primary => {
            icon_color = rgb(0xffffff);
            el.bg(rgb(ACCENT))
                .text_color(rgb(0xffffff))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(ACCENT_HOVER)))
                .active(|s| s.bg(rgb(0x094e49)))
        }
        ButtonKind::Secondary => {
            icon_color = rgb(INK_SECONDARY);
            el.bg(rgb(BG_SURFACE))
                .border_1()
                .border_color(rgb(BORDER_CONTROL))
                .text_color(rgb(INK_PRIMARY))
                .shadow(card_shadow())
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_SUBTLE)).border_color(rgb(0xbdb8ae)))
                .active(|s| s.bg(rgb(BG_HOVER)))
        }
        ButtonKind::Ghost => {
            icon_color = rgb(INK_SECONDARY);
            el.text_color(rgb(INK_SECONDARY))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER)).text_color(rgb(INK_PRIMARY)))
                .active(|s| s.bg(rgb(BG_PRESSED)))
        }
        ButtonKind::ActiveToggle => {
            icon_color = rgb(ACCENT_TEXT);
            el.bg(rgb(ACCENT_SOFT))
                .border_1()
                .border_color(rgb(BORDER_LIVE))
                .text_color(rgb(ACCENT_TEXT))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(0xd7efe9)))
        }
        ButtonKind::StopAction => {
            icon_color = rgb(DANGER_DOT);
            el.bg(rgb(BG_SURFACE))
                .border_1()
                .border_color(rgb(BORDER_CONTROL))
                .text_color(rgb(INK_PRIMARY))
                .shadow(card_shadow())
                .cursor_pointer()
                .hover(|s| {
                    s.bg(rgb(BG_ERROR))
                        .border_color(rgb(BORDER_ERROR))
                        .text_color(rgb(DANGER))
                })
                .active(|s| s.bg(rgb(DANGER_SOFT)))
        }
        ButtonKind::DangerSolid => {
            icon_color = rgb(0xffffff);
            el.bg(rgb(DANGER))
                .text_color(rgb(0xffffff))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(DANGER_HOVER)))
        }
        ButtonKind::DangerGhost => {
            icon_color = rgb(DANGER);
            el.text_color(rgb(DANGER))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(DANGER_SOFT)))
        }
    };

    if let Some(path) = icon_path {
        el = el.child(icon(path, icon_size, icon_color));
    }
    el.child(label)
}

fn field_group(label: &'static str, field: Entity<TextInput>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(INK_SECONDARY))
                .child(label),
        )
        .child(field)
}

fn status_badge(label: &'static str, bg: u32, fg: u32) -> Div {
    div()
        .flex_none()
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_size(px(11.5))
        .line_height(px(15.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label)
}

impl Hub {
    fn render_header(&self, pad_x: gpui::Pixels, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .flex_none()
            .h(px(54.))
            .px(pad_x)
            .bg(rgb(BG_BAR))
            .border_b_1()
            .border_color(rgb(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(16.))
                    .child(icon(ICON_AUDIO, 18., rgb(ACCENT)))
                    .child("音频共享台"),
            )
            .child(
                div().flex().items_center().gap(px(4.)).children(
                    [
                        ("console-tab", "控制台", Page::Console),
                        ("automation-tab", "自动化", Page::Automation),
                    ]
                    .map(|(id, label, page)| {
                        action_button(
                            id,
                            label,
                            None,
                            if self.page == page {
                                ButtonKind::ActiveToggle
                            } else {
                                ButtonKind::Ghost
                            },
                            true,
                            false,
                        )
                        .on_click(cx.listener(
                            move |hub, _: &ClickEvent, _, cx| {
                                hub.page = page;
                                cx.notify();
                            },
                        ))
                    }),
                ),
            )
    }

    fn add_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        action_button(
            "toggle-add",
            if self.add_open {
                "收起添加"
            } else {
                "配对 / 添加"
            },
            Some(if self.add_open { ICON_CLOSE } else { ICON_PLUS }),
            if self.add_open {
                ButtonKind::ActiveToggle
            } else {
                ButtonKind::Secondary
            },
            true,
            true,
        )
        .on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
            hub.add_open = !hub.add_open;
            cx.notify();
        }))
    }

    fn render_console(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let total = self.config.devices.len();
        let live = self
            .active
            .values()
            .filter(|active| matches!(classify_active_status(&active.status), ChannelState::Live))
            .count();
        let can_start = self
            .config
            .devices
            .iter()
            .any(|device| !self.running(device.id));
        let can_stop = self
            .config
            .devices
            .iter()
            .any(|device| self.running(device.id))
            || !self.config.resume_ids.is_empty();

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .child(self.scan_button("console-scan", true, cx))
                    .child(self.add_button(cx))
                    .when(total > 0, |row| {
                        row.child(
                            action_button("start-all", "全部开始", None, ButtonKind::Ghost, can_start, true)
                                .when(can_start, |btn| btn.on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                                    hub.command_all(true);
                                    cx.notify();
                                }))),
                        )
                        .child(
                            action_button("stop-all", "全部停止", None, ButtonKind::Ghost, can_stop, true)
                                .when(can_stop, |btn| btn.on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                                    hub.command_all(false);
                                    cx.notify();
                                }))),
                        )
                    }),
            )
            .when(self.add_open, |body| body.child(self.render_add_form(narrow, cx)))
            .when(total == 0, |body| {
                body.child(
                    div()
                        .p(px(18.))
                        .bg(rgb(BG_SURFACE))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(BORDER_HAIRLINE))
                        .text_color(rgb(INK_SECONDARY))
                        .child("尚未添加音频设备。在安卓设备开启「无线调试」后，可扫描、配对或手动添加。"),
                )
            })
            .when(total > 0, |body| {
                body.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_SECONDARY))
                        .child(format!("{total} 台设备 · {live} 路共享中")),
                )
                .children(self.config.devices.iter().map(|device| self.render_channel_card(device, cx)))
            })
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let notice = self.notice.as_ref()?;
        let (bg, border, fg, icon_path) = match notice.tone {
            NoticeTone::Success => (BG_LIVE, BORDER_LIVE, ACCENT_TEXT, ICON_CHECK),
            NoticeTone::Error => (BG_ERROR, BORDER_ERROR, DANGER, ICON_ALERT),
            NoticeTone::Info => (BG_SURFACE, BORDER_HAIRLINE, INK_SECONDARY, ICON_AUDIO),
        };
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(rgb(bg))
                .border_1()
                .border_color(rgb(border))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .flex_1()
                        .min_w_0()
                        .child(icon(icon_path, 14., rgb(fg)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.5))
                                .line_height(px(18.))
                                .text_color(rgb(fg))
                                .child(notice.text.clone()),
                        ),
                )
                .child(
                    div()
                        .id("dismiss-notice")
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_none()
                        .size(px(22.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0x181a1b12)))
                        .on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                            hub.notice = None;
                            cx.notify();
                        }))
                        .child(icon(ICON_CLOSE, 12., rgb(fg))),
                ),
        )
    }

    fn render_add_form(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let pairing_busy = self.automation.pairing();
        let idle = !pairing_busy;
        let add_mode = self.add_mode;
        let tab_button =
            |id: &'static str, label: &'static str, mode: AddMode, cx: &mut Context<Self>| {
                let active = add_mode == mode;
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_1()
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(16.))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .cursor_pointer()
                    .when(active, |tab| {
                        tab.bg(rgb(BG_SURFACE))
                            .text_color(rgb(INK_PRIMARY))
                            .shadow(pill_shadow())
                    })
                    .when(!active, |tab| {
                        tab.text_color(rgb(INK_SECONDARY))
                            .hover(|s| s.text_color(rgb(INK_PRIMARY)))
                    })
                    .on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                        hub.add_mode = mode;
                        if mode == AddMode::Pair && !hub.automation.pairing() {
                            hub.refresh_services();
                        }
                        cx.notify();
                    }))
                    .child(label)
            };

        let form_body = match self.add_mode {
            AddMode::Manual => {
                let submit_btn = action_button(
                    "add-manual",
                    "添加设备",
                    Some(ICON_PLUS),
                    ButtonKind::Primary,
                    true,
                    false,
                )
                .on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                    hub.submit_manual_add(cx);
                    cx.notify();
                }));

                if narrow {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(field_group("连接地址（IP:端口）", self.address.clone()))
                        .child(field_group("设备名称", self.name.clone()))
                        .child(div().flex().justify_end().child(submit_btn))
                } else {
                    div()
                        .flex()
                        .items_end()
                        .gap(px(10.))
                        .child(field_group("连接地址（IP:端口）", self.address.clone()))
                        .child(field_group("设备名称", self.name.clone()))
                        .child(submit_btn)
                }
            }
            AddMode::Pair => {
                let pair_btn = action_button(
                    "pair-submit",
                    if pairing_busy {
                        "配对中…"
                    } else {
                        "开始配对"
                    },
                    None,
                    ButtonKind::Primary,
                    idle,
                    false,
                )
                .when(idle, |btn| {
                    btn.on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                        hub.submit_pair(cx);
                        cx.notify();
                    }))
                });

                let inputs = if narrow {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(field_group(
                            "配对地址（IP:端口）",
                            self.pair_address.clone(),
                        ))
                        .child(field_group("6 位配对码", self.pair_code.clone()))
                        .child(div().flex().justify_end().child(pair_btn))
                } else {
                    div()
                        .flex()
                        .items_end()
                        .gap(px(10.))
                        .child(field_group(
                            "配对地址（IP:端口）",
                            self.pair_address.clone(),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(5.))
                                .w(px(148.))
                                .flex_none()
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(INK_SECONDARY))
                                        .child("6 位配对码"),
                                )
                                .child(self.pair_code.clone()),
                        )
                        .child(pair_btn)
                };

                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(INK_MUTED))
                            .child(
                                "保持平板的「使用配对码配对设备」弹窗打开，填写同一弹窗里的 IP、配对端口和六位码。无线调试主页显示的是连接端口。",
                            ),
                    )
                    .child({
                        let scanning = self.automation.scanning();
                        let mut row = div().flex().items_center().flex_wrap().gap(px(6.)).child(
                            action_button("find-pair", if scanning { "正在查找…" } else { "查找配对地址" },
                                Some(ICON_SCAN), ButtonKind::Secondary, idle && !scanning, true)
                            .when(idle && !scanning, |b| b.on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                                hub.refresh_services();
                                cx.notify();
                            })))
                        );
                        for (i, addr) in self.automation.pairing_addresses().enumerate() {
                            let addr = addr.to_owned();
                            row = row.child(div().id(("pair-addr", i)).px(px(8.)).py(px(4.))
                                .text_color(rgb(ACCENT_TEXT)).bg(rgb(ACCENT_SOFT)).rounded(px(5.))
                                .child(format!("填入 {addr}")).cursor_pointer()
                                .when(idle, |b| b.on_click(cx.listener(move |hub, _: &ClickEvent, window, cx| {
                                    hub.pair_address.update(cx, |field, cx| field.set(&addr, cx));
                                    window.focus(&hub.pair_code.read(cx).focus_handle(cx));
                                    cx.notify();
                                }))));
                        }
                        row
                    })
                    .child(inputs)
            }
        };

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(16.))
            .bg(rgb(BG_SURFACE))
            .border_1()
            .border_color(rgb(BORDER_HAIRLINE))
            .rounded(px(10.))
            .shadow(card_shadow())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .pt(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .p(px(3.))
                            .rounded(px(8.))
                            .bg(rgb(BG_SUBTLE))
                            .border_1()
                            .border_color(rgb(BORDER_HAIRLINE))
                            .child(tab_button("tab-manual", "手动添加", AddMode::Manual, cx))
                            .child(tab_button("tab-pair", "无线配对码", AddMode::Pair, cx)),
                    )
                    .child(form_body),
            )
    }

    fn render_channel_card(&self, device: &Device, cx: &mut Context<Self>) -> Div {
        let id = device.id;
        let editing = self.editing_id() == Some(id);
        let confirming_remove = self.confirm_remove == Some(id);
        let state = self.channel_state(id);
        let is_live = matches!(state, ChannelState::Live);
        let is_running = self.running(id);

        let (card_bg, card_border, rail_color, dot_color, pill_label, pill_bg, pill_fg, detail) =
            match state {
                ChannelState::Live => (
                    BG_LIVE,
                    BORDER_LIVE,
                    ACCENT,
                    LIVE_DOT,
                    "运行中",
                    ACCENT_SOFT,
                    ACCENT_TEXT,
                    None,
                ),
                ChannelState::Connecting(msg) => (
                    BG_CONNECTING,
                    BORDER_CONNECTING,
                    AMBER_DOT,
                    AMBER_DOT,
                    "连接中",
                    AMBER_SOFT,
                    AMBER_TEXT,
                    Some((msg, AMBER_TEXT)),
                ),
                ChannelState::Restarting => (
                    BG_CONNECTING,
                    BORDER_CONNECTING,
                    AMBER_DOT,
                    AMBER_DOT,
                    "重启中",
                    AMBER_SOFT,
                    AMBER_TEXT,
                    None,
                ),
                ChannelState::Stopping => (
                    BG_SURFACE,
                    BORDER_HAIRLINE,
                    BORDER_CONTROL,
                    0x9ca3af,
                    "停止中",
                    BG_HOVER,
                    INK_SECONDARY,
                    None,
                ),
                ChannelState::Attention(msg) => (
                    BG_ERROR,
                    BORDER_ERROR,
                    DANGER_DOT,
                    DANGER_DOT,
                    "重试中",
                    DANGER_SOFT,
                    DANGER,
                    Some((msg, DANGER)),
                ),
                ChannelState::Stopped => (
                    BG_SURFACE,
                    if editing {
                        BORDER_CONTROL
                    } else {
                        BORDER_HAIRLINE
                    },
                    0xe8e5de,
                    0xc2c6cc,
                    "已停止",
                    BG_SUBTLE,
                    INK_MUTED,
                    None,
                ),
            };

        let is_stopping_only = self.stopping.get(&id).is_some_and(|item| !item.restart);

        let primary_action = if is_stopping_only {
            action_button(
                ("toggle", id),
                "停止中…",
                None,
                ButtonKind::Secondary,
                false,
                false,
            )
            .min_w(px(96.))
        } else if is_running {
            action_button(
                ("toggle", id),
                "停止共享",
                Some(ICON_STOP),
                ButtonKind::StopAction,
                true,
                false,
            )
            .min_w(px(96.))
            .on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                hub.command_sessions(&[id], false, false);
                cx.notify();
            }))
        } else {
            action_button(
                ("toggle", id),
                "开始共享",
                Some(ICON_PLAY),
                ButtonKind::Primary,
                true,
                false,
            )
            .min_w(px(96.))
            .on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                hub.command_sessions(&[id], true, false);
                cx.notify();
            }))
        };

        let can_restart = self.active.contains_key(&id);
        let restart_btn = action_button(
            ("restart-device", id),
            "重启",
            None,
            ButtonKind::Ghost,
            can_restart,
            true,
        )
        .when(can_restart, |btn| {
            btn.on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                hub.command_sessions(&[id], true, true);
                cx.notify();
            }))
        });
        let edit_btn = action_button(
            ("edit-device", id),
            "编辑",
            Some(ICON_EDIT),
            if editing {
                ButtonKind::ActiveToggle
            } else {
                ButtonKind::Ghost
            },
            true,
            true,
        )
        .on_click(cx.listener(move |hub, _: &ClickEvent, window, cx| {
            hub.open_editor(id, window, cx);
            cx.notify();
        }));

        let status_dot = if is_live {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(12.))
                .rounded_full()
                .flex_none()
                .bg(rgba(0x10b98130))
                .child(div().size(px(7.)).rounded_full().bg(rgb(dot_color)))
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(12.))
                .flex_none()
                .child(div().size(px(7.)).rounded_full().bg(rgb(dot_color)))
        };

        let mut card = div()
            .flex()
            .flex_col()
            .bg(rgb(card_bg))
            .border_1()
            .border_color(rgb(card_border))
            .rounded(px(10.))
            .overflow_hidden()
            .shadow(card_shadow())
            .child(
                div()
                    .flex()
                    .child(div().w(px(3.5)).flex_none().bg(rgb(rail_color)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .flex_1()
                            .min_w_0()
                            .px(px(14.))
                            .py(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .min_w_0()
                                            .child(status_dot)
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .text_size(px(15.))
                                                    .line_height(px(21.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK_PRIMARY))
                                                    .truncate()
                                                    .child(device.name.clone()),
                                            )
                                            .child(status_badge(pill_label, pill_bg, pill_fg)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .pl(px(20.))
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .font_family(MONO_FONT)
                                                    .text_size(px(12.5))
                                                    .line_height(px(17.))
                                                    .text_color(rgb(INK_SECONDARY))
                                                    .truncate()
                                                    .child(device.addr.clone()),
                                            )
                                            .when(device.identity.is_some(), |row| {
                                                row.child(
                                                    div()
                                                        .flex_none()
                                                        .text_size(px(11.5))
                                                        .line_height(px(16.))
                                                        .text_color(rgb(INK_MUTED))
                                                        .child("· 已记录序列号"),
                                                )
                                            }),
                                    )
                                    .when_some(detail, |info, (message, color)| {
                                        info.child(
                                            div()
                                                .pl(px(20.))
                                                .pt(px(2.))
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .text_color(rgb(color))
                                                .child(message),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .flex_none()
                                    .when(is_running, |row| row.child(restart_btn))
                                    .child(edit_btn)
                                    .child(primary_action),
                            ),
                    ),
            );

        if editing {
            let identity_text = match &device.identity {
                Some(serial) => format!("设备序列号：{serial}"),
                None => "首次成功连接后自动记录序列号，重启后可自动找回地址".into(),
            };

            let remove_controls = if confirming_remove {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .flex_none()
                    .child(
                        action_button(
                            ("confirm-remove", id),
                            "确认移除",
                            Some(ICON_TRASH),
                            ButtonKind::DangerSolid,
                            true,
                            true,
                        )
                        .on_click(cx.listener(
                            move |hub, _: &ClickEvent, _, cx| {
                                hub.remove_device(id);
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        action_button(
                            ("cancel-remove", id),
                            "取消",
                            None,
                            ButtonKind::Ghost,
                            true,
                            true,
                        )
                        .on_click(cx.listener(
                            |hub, _: &ClickEvent, _, cx| {
                                hub.confirm_remove = None;
                                cx.notify();
                            },
                        )),
                    )
            } else {
                div().flex_none().child(
                    action_button(
                        ("ask-remove", id),
                        "移除设备",
                        Some(ICON_TRASH),
                        ButtonKind::DangerGhost,
                        true,
                        true,
                    )
                    .on_click(cx.listener(
                        move |hub, _: &ClickEvent, _, cx| {
                            hub.confirm_remove = Some(id);
                            cx.notify();
                        },
                    )),
                )
            };

            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(12.))
                    .bg(rgb(BG_SUBTLE))
                    .border_t_1()
                    .border_color(rgb(BORDER_HAIRLINE))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(8.))
                            .child(field_group("设备名称", self.rename.clone()))
                            .child(field_group("连接地址", self.edit_address.clone()))
                            .child(
                                action_button(
                                    ("save-rename", id),
                                    "保存",
                                    None,
                                    ButtonKind::Secondary,
                                    true,
                                    false,
                                )
                                .on_click(cx.listener(
                                    move |hub, _: &ClickEvent, _, cx| {
                                        hub.save_device(id, cx);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                action_button(
                                    ("cancel-edit", id),
                                    "收起",
                                    None,
                                    ButtonKind::Ghost,
                                    true,
                                    false,
                                )
                                .on_click(cx.listener(
                                    |hub, _: &ClickEvent, _, cx| {
                                        hub.editing = None;
                                        hub.confirm_remove = None;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(180.))
                                    .text_size(px(12.))
                                    .line_height(px(17.))
                                    .text_color(rgb(INK_MUTED))
                                    .child(identity_text),
                            )
                            .child(remove_controls),
                    ),
            );
        }

        card
    }

    fn render_buffer_footer(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let current_saved =
            buffer_value(&self.config.scrcpy_args).and_then(|value| value.parse::<u32>().ok());
        let input_trimmed = self.buffer.read(cx).value().trim().to_owned();
        let input_dirty = current_saved
            .map(|value| value.to_string() != input_trimmed)
            .unwrap_or(!input_trimmed.is_empty());

        let pad_x = if narrow { px(14.) } else { px(20.) };

        let presets = [50u32, 100, 150].map(|value| {
            let active = current_saved == Some(value);
            div()
                .id(("preset", value))
                .flex()
                .items_center()
                .justify_center()
                .h(px(26.))
                .px(px(10.))
                .rounded(px(6.))
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .cursor_pointer()
                .whitespace_nowrap()
                .when(active, |pill| {
                    pill.bg(rgb(BG_SURFACE))
                        .text_color(rgb(ACCENT_TEXT))
                        .shadow(pill_shadow())
                })
                .when(!active, |pill| {
                    pill.text_color(rgb(INK_SECONDARY))
                        .hover(|s| s.text_color(rgb(INK_PRIMARY)))
                })
                .on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                    hub.buffer
                        .update(cx, |field, cx| field.set(&value.to_string(), cx));
                    hub.apply_buffer(value);
                    cx.notify();
                }))
                .child(format!("{value} ms"))
        });

        let apply_btn = action_button(
            "save-buffer",
            "应用",
            None,
            if input_dirty {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            true,
            true,
        )
        .on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
            hub.save_buffer(cx);
            cx.notify();
        }));

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(px(14.))
            .gap_y(px(8.))
            .flex_none()
            .px(pad_x)
            .py(px(10.))
            .bg(rgb(BG_BAR))
            .border_t_1()
            .border_color(rgb(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(icon(ICON_SLIDERS, 14.5, rgb(INK_SECONDARY)))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .line_height(px(18.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(INK_PRIMARY))
                                    .whitespace_nowrap()
                                    .child("音频缓冲"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .p(px(2.))
                            .rounded(px(8.))
                            .bg(rgb(BG_HOVER))
                            .children(presets),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(rgb(INK_MUTED))
                            .whitespace_nowrap()
                            .child("自定义"),
                    )
                    .child(div().w(px(72.)).flex_none().child(self.buffer.clone()))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(rgb(INK_MUTED))
                            .child("ms"),
                    )
                    .child(apply_btn),
            )
    }
}

impl Render for Hub {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = window.viewport_size().width < px(600.);
        let pad_x = if narrow { px(14.) } else { px(20.) };
        let pad_y = if narrow { px(12.) } else { px(16.) };

        div()
            .flex()
            .flex_col()
            .size_full()
            .font_family(UI_FONT)
            .bg(rgb(BG_CANVAS))
            .text_color(rgb(INK_PRIMARY))
            .text_size(px(13.5))
            .child(self.render_header(pad_x, cx))
            .child(
                div()
                    .id(match self.page {
                        Page::Console => "console-workspace",
                        Page::Automation => "automation-workspace",
                    })
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .px(pad_x)
                            .py(pad_y)
                            .when_some(self.config_error.clone(), |body, error| {
                                body.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .px(px(12.))
                                        .py(px(10.))
                                        .rounded(px(8.))
                                        .bg(rgb(BG_ERROR))
                                        .border_1()
                                        .border_color(rgb(BORDER_ERROR))
                                        .child(icon(ICON_ALERT, 15., rgb(DANGER)))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(px(12.5))
                                                .line_height(px(18.))
                                                .text_color(rgb(DANGER))
                                                .child(format!("无法读取设备配置：{error}")),
                                        ),
                                )
                            })
                            .when_some(self.render_notice(cx), |body, notice| body.child(notice))
                            .child(match self.page {
                                Page::Console => self.render_console(narrow, cx),
                                Page::Automation => self.render_automation(narrow, cx),
                            }),
                    ),
            )
            .child(match self.page {
                Page::Console => self.render_buffer_footer(narrow, cx),
                Page::Automation => self.render_session_log(pad_x),
            })
    }
}
