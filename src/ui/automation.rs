use super::*;
use crate::engine::Found;
use std::{collections::VecDeque, sync::mpsc::TryRecvError, time::Instant};
use windows::Win32::Foundation::SYSTEMTIME;

const SCAN_INTERVAL: Duration = Duration::from_secs(20);
const USB_INTERVAL: Duration = Duration::from_secs(8);
const LOG_LIMIT: usize = 250;

type ScanResults = Result<Vec<Found>, String>;
type UsbResults = Result<Vec<(u64, String, String)>, String>;

struct ScanJob {
    result: Receiver<ScanResults>,
    previous_devices: Vec<Device>,
    manual: bool,
    obsolete: bool,
}

struct UsbJob {
    result: Receiver<UsbResults>,
    previous_devices: Vec<Device>,
    obsolete: bool,
}

struct ScanReport {
    time: String,
    summary: String,
    failed: bool,
}

pub(super) struct Automation {
    scan: Option<ScanJob>,
    pair: Option<Receiver<Result<(), String>>>,
    usb: Option<UsbJob>,
    scan_requested: bool,
    next_scan: Instant,
    next_usb: Instant,
    last_scan: Option<ScanReport>,
    last_usb_error: Option<String>,
    found: Vec<Found>,
    log: VecDeque<(String, String)>,
}

impl Automation {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        Self {
            scan: None,
            pair: None,
            usb: None,
            scan_requested: false,
            next_scan: now,
            next_usb: now,
            last_scan: None,
            last_usb_error: None,
            found: Vec::new(),
            log: VecDeque::new(),
        }
    }

    pub(super) fn pairing(&self) -> bool {
        self.pair.is_some()
    }

    pub(super) fn pairing_addresses(&self) -> impl Iterator<Item = &str> {
        self.found
            .iter()
            .filter(|item| item.pairing)
            .map(|item| item.addr.as_str())
    }

    pub(super) fn scanning(&self) -> bool {
        self.scan.is_some() || self.scan_requested
    }
}

#[derive(Clone, Copy)]
enum Setting {
    Scan,
    Add,
    Resume,
    Usb,
    Fixed,
}

impl Setting {
    fn value(self, config: &mut Config) -> &mut bool {
        match self {
            Self::Scan => &mut config.auto_scan,
            Self::Add => &mut config.auto_add,
            Self::Resume => &mut config.restore_sessions,
            Self::Usb => &mut config.usb_recovery,
            Self::Fixed => &mut config.fixed_port,
        }
    }
}

fn timestamp() -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SYSTEMTIME);
    }
    let mut time = SYSTEMTIME::default();
    // GetLocalTime writes the Windows local clock into a writable SYSTEMTIME.
    unsafe { GetLocalTime(&mut time) };
    format!("{:02}:{:02}:{:02}", time.wHour, time.wMinute, time.wSecond)
}

fn job_result<T>(receiver: &Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err("后台操作已中断".into())),
    }
}

impl Hub {
    pub(super) fn log(&mut self, text: impl Into<String>) {
        self.automation.log.push_front((timestamp(), text.into()));
        self.automation.log.truncate(LOG_LIMIT);
    }

    pub(super) fn trigger_scan(&mut self) {
        self.page = Page::Automation;
        self.refresh_services();
    }

    pub(super) fn refresh_services(&mut self) {
        if !self.automation.pairing()
            && let Some(scan) = &mut self.automation.scan
            && !scan.obsolete
        {
            scan.manual = true;
        } else {
            self.automation.scan_requested = true;
        }
        self.schedule_discovery();
    }

    pub(super) fn begin_pair(&mut self, address: String, code: String) {
        let tools = self.tools.clone();
        let (send, result) = mpsc::channel();
        self.automation.pair = Some(result);
        thread::spawn(move || {
            let _ = send.send(tools.pair(&address, &code));
        });
        self.set_notice(NoticeTone::Info, "正在与设备配对…");
    }

    pub(super) fn schedule_discovery(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.automation.scan.is_none()
            && !self.automation.pairing()
            && (self.automation.scan_requested
                || (self.config.auto_scan && now >= self.automation.next_scan))
        {
            let manual = std::mem::take(&mut self.automation.scan_requested);
            let tools = self.tools.clone();
            let (send, result) = mpsc::channel();
            self.automation.scan = Some(ScanJob {
                result,
                previous_devices: self.config.devices.clone(),
                manual,
                obsolete: false,
            });
            thread::spawn(move || {
                let _ = send.send(tools.scan());
            });
            if manual {
                self.log("开始扫描局域网无线调试服务");
            }
            changed = true;
        }

        if self.config.usb_recovery
            && self.automation.usb.is_none()
            && now >= self.automation.next_usb
            && self
                .config
                .devices
                .iter()
                .any(|device| device.identity.is_some())
        {
            self.tools.set_usb_recovery(true);
            let devices = self.config.devices.clone();
            let previous_devices = devices.clone();
            let tools = self.tools.clone();
            let (send, result) = mpsc::channel();
            self.automation.usb = Some(UsbJob {
                result,
                previous_devices,
                obsolete: false,
            });
            thread::spawn(move || {
                let _ = send.send(tools.recover_usb(&devices));
            });
        }
        changed
    }

    pub(super) fn poll_discovery(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if let Some(result) = self
            .automation
            .usb
            .as_ref()
            .and_then(|job| job_result(&job.result))
        {
            let job = self.automation.usb.take().unwrap();
            self.tools.set_usb_recovery(false);
            if self.config.usb_recovery && !job.obsolete {
                self.automation.next_usb = Instant::now() + USB_INTERVAL;
                changed |= self.finish_usb(&job.previous_devices, result);
            }
        }

        if let Some(result) = self
            .automation
            .scan
            .as_ref()
            .and_then(|job| job_result(&job.result))
        {
            let job = self.automation.scan.take().unwrap();
            if !job.obsolete {
                self.automation.next_scan = Instant::now() + SCAN_INTERVAL;
                self.finish_scan(&job.previous_devices, result, job.manual);
            }
            changed = true;
        }

        if let Some(result) = self.automation.pair.as_ref().and_then(job_result) {
            self.automation.pair = None;
            match result {
                Ok(()) => {
                    self.pair_code.update(cx, |field, cx| field.set("", cx));
                    if self.add_open && self.add_mode == AddMode::Pair {
                        self.add_open = false;
                        self.page = Page::Automation;
                    }
                    self.set_notice(NoticeTone::Success, "配对成功，正在扫描连接地址…");
                    self.automation.scan_requested = true;
                }
                Err(error) => self.set_notice(NoticeTone::Error, error),
            }
            changed = true;
        }

        self.schedule_discovery() || changed
    }

    fn finish_scan(&mut self, snapshots: &[Device], result: ScanResults, manual: bool) {
        let (summary, failed) = match result {
            Ok(found) => {
                let summary = format!(
                    "发现 {} 个连接服务、{} 个配对服务",
                    found.iter().filter(|item| !item.pairing).count(),
                    found.iter().filter(|item| item.pairing).count()
                );
                self.merge_scan(snapshots, &found);
                self.automation.found = found;
                (summary, false)
            }
            Err(error) => (format!("扫描失败：{error}"), true),
        };
        let changed = self
            .automation
            .last_scan
            .as_ref()
            .is_none_or(|report| report.summary != summary);
        self.automation.last_scan = Some(ScanReport {
            time: timestamp(),
            summary: summary.clone(),
            failed,
        });
        if manual || changed {
            self.log(summary);
        }
    }

    fn merge_scan(&mut self, snapshots: &[Device], found: &[Found]) {
        let previous = self.config.devices.clone();
        let mut messages = Vec::new();
        let mut restart = Vec::new();
        for item in found
            .iter()
            .filter(|item| !item.pairing && item.identity.is_some())
        {
            match self.match_device(item) {
                Ok(Some(index)) => {
                    let device = &self.config.devices[index];
                    let unchanged = snapshots.iter().any(|snapshot| {
                        snapshot.id == device.id
                            && snapshot.addr == device.addr
                            && snapshot.identity == device.identity
                    });
                    // Keep a working endpoint when the same serial advertises multiple addresses.
                    let verified_current = found.iter().any(|other| {
                        !other.pairing
                            && same_address(&other.addr, &device.addr)
                            && other.identity == item.identity
                    });
                    if !same_address(&device.addr, &item.addr)
                        && (!unchanged
                            || verified_current
                            || matches!(self.channel_state(device.id), ChannelState::Live)
                            || self.keep_fixed_address(&device.addr, &item.addr))
                    {
                        continue;
                    }
                    if device.addr == item.addr && device.identity == item.identity {
                        continue;
                    }
                    if !same_address(&device.addr, &item.addr) {
                        messages.push(format!("{} · 无线地址更新为 {}", device.name, item.addr));
                        restart.push(device.id);
                    }
                    self.config.devices[index].addr.clone_from(&item.addr);
                    self.config.devices[index]
                        .identity
                        .clone_from(&item.identity);
                }
                Ok(None)
                    if self.config.auto_add
                        && item.identity.as_ref().is_some_and(|serial| {
                            !self.config.excluded_identities.contains(serial)
                        }) =>
                {
                    match self.append_device(&item.addr, &item.name, item.identity.clone()) {
                        Ok(()) => messages.push(format!("自动加入「{}」", item.name)),
                        Err(error) => {
                            self.set_notice(NoticeTone::Error, error);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        self.commit_discovery(previous, messages, restart);
    }

    fn keep_fixed_address(&self, saved: &str, found: &str) -> bool {
        (self.config.fixed_port || self.config.usb_recovery)
            && saved
                .parse::<std::net::SocketAddr>()
                .ok()
                .zip(found.parse::<std::net::SocketAddr>().ok())
                .is_some_and(|(saved, found)| saved.ip() == found.ip() && saved.port() == 5556)
    }

    fn match_device(&self, item: &Found) -> Result<Option<usize>, &'static str> {
        let devices = &self.config.devices;
        let by_address = devices
            .iter()
            .position(|device| same_address(&device.addr, &item.addr));
        let by_identity = item.identity.as_ref().and_then(|identity| {
            devices
                .iter()
                .position(|device| device.identity.as_ref() == Some(identity))
        });
        if let Some(address_index) = by_address {
            let conflicts = item
                .identity
                .as_ref()
                .zip(devices[address_index].identity.as_ref())
                .is_some_and(|(found, saved)| found != saved)
                || by_identity.is_some_and(|identity_index| identity_index != address_index);
            if conflicts {
                return Err("发现的设备身份与已有配置冲突，请检查连接地址");
            }
        }
        Ok(by_identity.or(by_address))
    }

    fn add_scan_result(&mut self, item: &Found) {
        let previous = self.config.devices.clone();
        let (message, restart) = match self.match_device(item) {
            Ok(Some(index)) => {
                let device = &mut self.config.devices[index];
                if same_address(&device.addr, &item.addr) {
                    return;
                }
                device.addr.clone_from(&item.addr);
                (
                    format!("{} · 无线地址更新为 {}", device.name, item.addr),
                    vec![device.id],
                )
            }
            Ok(None) => match self.append_device(&item.addr, &item.name, item.identity.clone()) {
                Ok(()) => (format!("已添加设备「{}」", item.name), Vec::new()),
                Err(error) => {
                    self.set_notice(NoticeTone::Error, error);
                    return;
                }
            },
            Err(error) => {
                self.set_notice(NoticeTone::Error, error);
                return;
            }
        };
        let previous_excluded = self.config.excluded_identities.clone();
        if let Some(serial) = &item.identity {
            self.config
                .excluded_identities
                .retain(|excluded| excluded != serial);
        }
        if self.commit_discovery(previous, Vec::new(), restart) {
            self.set_notice(NoticeTone::Success, message);
        } else {
            self.config.excluded_identities = previous_excluded;
        }
    }

    pub(super) fn apply_identification(&mut self, id: u64, addr: &str, serial: &str) -> bool {
        let Some(index) = self
            .config
            .devices
            .iter()
            .position(|device| device.id == id)
        else {
            return false;
        };
        let device = &self.config.devices[index];
        if device.addr == addr && device.identity.as_deref() == Some(serial) {
            return false;
        }
        if device
            .identity
            .as_deref()
            .is_some_and(|saved| saved != serial)
            || self.config.devices.iter().any(|other| {
                other.id != id
                    && (same_address(&other.addr, addr)
                        || other.identity.as_deref() == Some(serial))
            })
        {
            self.set_notice(
                NoticeTone::Error,
                format!("「{}」识别到的序列号或地址与已有配置冲突", device.name),
            );
            return false;
        }
        let name = device.name.clone();
        let device = &mut self.config.devices[index];
        device.addr = addr.into();
        device.identity = Some(serial.into());
        self.log(format!("{name} · 已确认设备身份（{addr}）"));
        true
    }

    fn finish_usb(&mut self, snapshots: &[Device], result: UsbResults) -> bool {
        let recovered = match result {
            Ok(recovered) => {
                self.automation.last_usb_error = None;
                recovered
            }
            Err(error) => {
                if self.automation.last_usb_error.as_ref() == Some(&error) {
                    return false;
                }
                self.log(format!("USB 恢复失败：{error}"));
                self.automation.last_usb_error = Some(error);
                return true;
            }
        };
        if recovered.is_empty() {
            return false;
        }

        let previous = self.config.devices.clone();
        let mut messages = Vec::new();
        let mut restart = Vec::new();
        for (id, addr, serial) in recovered {
            let Some(snapshot) = snapshots.iter().find(|device| device.id == id) else {
                continue;
            };
            let Some(index) = self
                .config
                .devices
                .iter()
                .position(|device| device.id == id)
            else {
                continue;
            };
            let device = &self.config.devices[index];
            if device.addr != snapshot.addr
                || device.identity != snapshot.identity
                || device.identity.as_deref() != Some(&serial)
                || same_address(&device.addr, &addr)
                || self
                    .config
                    .devices
                    .iter()
                    .any(|other| other.id != id && same_address(&other.addr, &addr))
            {
                continue;
            }
            messages.push(format!("{} · USB 固定端口已恢复（{addr}）", device.name));
            self.config.devices[index].addr = addr;
            restart.push(id);
        }
        if restart.is_empty() {
            return false;
        }
        self.commit_discovery(previous, messages, restart);
        true
    }

    fn commit_discovery(
        &mut self,
        previous: Vec<Device>,
        messages: Vec<String>,
        mut restart: Vec<u64>,
    ) -> bool {
        let devices = &self.config.devices;
        if previous.len() == devices.len()
            && previous.iter().zip(devices).all(|(before, after)| {
                before.addr == after.addr && before.identity == after.identity
            })
        {
            return false;
        }
        if let Err(error) = self.persist() {
            self.config.devices = previous;
            self.set_notice(NoticeTone::Error, error);
            return false;
        }
        for message in messages {
            self.log(message);
        }
        restart.sort_unstable();
        restart.dedup();
        for id in restart {
            if self.active.contains_key(&id) {
                self.stop(id, true);
            }
        }
        true
    }

    fn toggle_setting(&mut self, setting: Setting, label: &str) {
        let previous = *setting.value(&mut self.config);
        *setting.value(&mut self.config) = !previous;
        if let Err(error) = self.persist() {
            *setting.value(&mut self.config) = previous;
            self.set_notice(NoticeTone::Error, error);
            return;
        }
        match setting {
            Setting::Scan if previous => {
                if let Some(job) = &mut self.automation.scan
                    && !job.manual
                {
                    job.obsolete = true;
                }
            }
            Setting::Scan => self.automation.next_scan = Instant::now(),
            Setting::Usb if previous => {
                self.tools.set_usb_recovery(false);
                if let Some(job) = &mut self.automation.usb {
                    job.obsolete = true;
                }
                self.automation.last_usb_error = None;
            }
            Setting::Usb => self.automation.next_usb = Instant::now(),
            Setting::Fixed => self.tools.set_fixed_port(self.config.fixed_port),
            Setting::Add => self.automation.next_scan = Instant::now(),
            Setting::Resume => {}
        }
        self.log(format!(
            "{}「{label}」",
            if previous { "已关闭" } else { "已开启" }
        ));
        self.schedule_discovery();
    }
}

impl Hub {
    pub(super) fn scan_button(
        &self,
        id: &'static str,
        from_console: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let enabled = from_console || !self.automation.scanning();
        action_button(
            id,
            if enabled {
                "扫描局域网"
            } else {
                "扫描中…"
            },
            Some(ICON_SCAN),
            if from_console {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            enabled,
            true,
        )
        .when(enabled, |button| {
            button.on_click(cx.listener(|hub, _: &ClickEvent, _, cx| {
                hub.trigger_scan();
                cx.notify();
            }))
        })
    }

    pub(super) fn render_automation(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let (scan_summary, failed) = match &self.automation.last_scan {
            Some(report) => (
                format!("最近扫描 {} · {}", report.time, report.summary),
                report.failed,
            ),
            None if self.automation.scanning() => ("正在扫描局域网无线调试服务…".into(), false),
            None => ("尚未扫描".into(), false),
        };
        let settings = [
            (
                Setting::Scan,
                "自动发现",
                "启动时扫描，完成后每隔 20 秒再次扫描",
                self.config.auto_scan,
            ),
            (
                Setting::Add,
                "自动加入",
                "自动添加已授权且可验证身份的设备",
                self.config.auto_add,
            ),
            (
                Setting::Resume,
                "启动时恢复共享",
                "重新打开时恢复上次启用的设备",
                self.config.restore_sessions,
            ),
            (
                Setting::Usb,
                "USB 固定端口恢复",
                "通过已授权的 USB 恢复已识别设备的无线连接",
                self.config.usb_recovery,
            ),
            (
                Setting::Fixed,
                "共享时切换固定端口",
                "开启共享时尝试使用固定端口 5556",
                self.config.fixed_port,
            ),
        ];
        let switches = settings.map(|(setting, label, description, enabled)| {
            self.render_setting(setting, label, description, enabled, cx)
        });
        let found = &self.automation.found;

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(1.))
                    .rounded(px(9.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(BORDER_HAIRLINE))
                    .bg(rgb(BORDER_HAIRLINE))
                    .children(
                        switches
                            .into_iter()
                            .map(|row| row.w(gpui::relative(0.498)).flex_grow()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(12.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(rgb(BORDER_HAIRLINE))
                    .bg(rgb(BG_SURFACE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("局域网发现"),
                            )
                            .child(self.scan_button("automation-scan", false, cx))
                            .child(self.add_button(cx)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(if failed { DANGER } else { INK_SECONDARY }))
                            .child(scan_summary),
                    )
                    .when(self.add_open, |section| {
                        section.child(self.render_add_form(narrow, cx))
                    })
                    .when(failed && !found.is_empty(), |section| {
                        section.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(INK_SECONDARY))
                                .child("以下为上次成功扫描的结果"),
                        )
                    })
                    .when(!found.is_empty(), |section| {
                        section.child(
                            div()
                                .id("discovery-results")
                                .max_h(px(174.))
                                .overflow_y_scroll()
                                .rounded(px(7.))
                                .border_1()
                                .border_color(rgb(BORDER_HAIRLINE))
                                .children(
                                    found
                                        .iter()
                                        .enumerate()
                                        .map(|(index, item)| self.render_found(index, item, cx)),
                                ),
                        )
                    }),
            )
    }

    fn render_setting(
        &self,
        setting: Setting,
        label: &'static str,
        description: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(12.))
            .py(px(9.))
            .bg(rgb(BG_SURFACE))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SECONDARY))
                            .child(description),
                    ),
            )
            .child(
                action_button(
                    label,
                    if enabled { "已开启" } else { "已关闭" },
                    None,
                    if enabled {
                        ButtonKind::ActiveToggle
                    } else {
                        ButtonKind::Secondary
                    },
                    true,
                    true,
                )
                .on_click(cx.listener(move |hub, _: &ClickEvent, _, cx| {
                    hub.toggle_setting(setting, label);
                    cx.notify();
                })),
            )
    }

    fn render_found(&self, index: usize, item: &Found, cx: &mut Context<Self>) -> Div {
        let (status, bg, fg, action) = if item.pairing {
            ("配对", AMBER_SOFT, AMBER_TEXT, Some("填写配对码"))
        } else {
            match self.match_device(item) {
                Ok(Some(saved)) if same_address(&self.config.devices[saved].addr, &item.addr) => {
                    ("已添加", ACCENT_SOFT, ACCENT_TEXT, None)
                }
                Ok(Some(_)) => ("同一设备", ACCENT_SOFT, ACCENT_TEXT, Some("采用此地址")),
                Ok(None) if item.identity.is_some() => {
                    ("已授权", ACCENT_SOFT, ACCENT_TEXT, Some("添加"))
                }
                Ok(None) => ("身份未验证", AMBER_SOFT, AMBER_TEXT, Some("添加")),
                Err(_) => ("地址冲突", DANGER_SOFT, DANGER, None),
            }
        };
        let item = item.clone();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(9.))
            .py(px(7.))
            .when(index > 0, |row| {
                row.border_t_1().border_color(rgb(BORDER_HAIRLINE))
            })
            .child(status_badge(status, bg, fg))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(item.name.clone()))
                    .child(
                        div()
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .text_color(rgb(INK_SECONDARY))
                            .child(item.addr.clone()),
                    ),
            )
            .when_some(action, |row, label| {
                row.child(
                    action_button(
                        ("found", index),
                        label,
                        None,
                        ButtonKind::Secondary,
                        true,
                        true,
                    )
                    .on_click(cx.listener(
                        move |hub, _: &ClickEvent, window, cx| {
                            if item.pairing {
                                hub.add_open = true;
                                hub.add_mode = AddMode::Pair;
                                hub.pair_address
                                    .update(cx, |field, cx| field.set(&item.addr, cx));
                                window.focus(&hub.pair_code.read(cx).focus_handle(cx));
                            } else {
                                hub.add_scan_result(&item);
                            }
                            cx.notify();
                        },
                    )),
                )
            })
    }

    pub(super) fn render_session_log(&self, pad_x: gpui::Pixels) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .px(pad_x)
            .py(px(8.))
            .border_t_1()
            .border_color(rgb(BORDER_HAIRLINE))
            .bg(rgb(BG_BAR))
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("会话日志"),
            )
            .child(
                div()
                    .id("session-log")
                    .h(px(96.))
                    .overflow_y_scroll()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(rgb(BORDER_HAIRLINE))
                    .bg(rgb(BG_SURFACE))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .px(px(9.))
                            .py(px(7.))
                            .children(self.automation.log.iter().map(|(time, text)| {
                                div()
                                    .flex()
                                    .gap(px(10.))
                                    .text_size(px(12.))
                                    .line_height(px(17.))
                                    .child(
                                        div()
                                            .flex_none()
                                            .font_family(MONO_FONT)
                                            .text_color(rgb(INK_SECONDARY))
                                            .child(time.clone()),
                                    )
                                    .child(div().flex_1().min_w_0().child(text.clone()))
                            })),
                    ),
            )
    }
}
