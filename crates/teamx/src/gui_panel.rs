//! gui_panel.rs — native control-panel window for the teamx tray (L1).
//!
//! `teamx gui-panel` opens an egui/eframe window showing the status of the
//! tun0 proxy and SOCKS5 proxy with start/stop controls, the default exit,
//! and a live log panel. All operations go through the `teamx` CLI as child
//! processes; their stdout/stderr is captured into the log panel so failures
//! are visible instead of silent.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Maximum log lines kept in the panel.
const LOG_CAP: usize = 400;

/// A shared ring buffer of log lines.
#[derive(Clone, Default)]
pub struct LogBuf(Arc<Mutex<VecDeque<String>>>);

impl LogBuf {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP))))
    }

    fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap();
        if q.len() >= LOG_CAP {
            q.pop_front();
        }
        q.push_back(line);
    }

    fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().cloned().collect()
    }
}

/// One managed worker: the child process + its stdout/stderr pump.
struct Worker {
    child: Option<Child>,
    log: LogBuf,
    name: &'static str,
}

impl Worker {
    fn new(name: &'static str, log: &LogBuf) -> Self {
        Worker { child: None, log: log.clone(), name }
    }

    fn is_running(&mut self) -> bool {
        self.child.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false)
    }

    /// Spawn `teamx <args>` with piped stdout/stderr, pumping lines into the
    /// log. Any env vars passed are applied.
    fn spawn(&mut self, args: &[&str], envs: &[(&str, String)]) -> Result<(), String> {
        self.kill();
        let mut cmd = Command::new(exe_path());
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", args.join(" ")))?;

        // Pump stdout + stderr into the shared log.
        let log = self.log.clone();
        let name = self.name;
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    log.push(format!("[{name}] {line}"));
                }
            });
        }
        let log = self.log.clone();
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    log.push(format!("[{name}] {line}"));
                }
            });
        }
        self.child = Some(child);
        Ok(())
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The control panel application state.
pub struct PanelApp {
    tun0_running: bool,
    proxy_running: bool,
    exit_name: String,
    log: LogBuf,
    show_log: bool,
    proxy_worker: Worker,
}

impl PanelApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_font(&cc.egui_ctx);
        let log = LogBuf::new();
        log.push("Teamx 控制面板已启动".to_string());
        let proxy_worker = Worker::new("proxy", &log);
        PanelApp {
            tun0_running: is_tun0_running(),
            proxy_running: false,
            exit_name: current_default_exit(),
            log,
            show_log: false,
            proxy_worker,
        }
    }

    fn refresh_status(&mut self) {
        // tun0 is launched as root (detached) — detect by pgrep.
        self.tun0_running = is_tun0_running();
        self.proxy_running = self.proxy_worker.is_running();
        self.exit_name = current_default_exit();
    }

    fn start_proxy(&mut self) {
        self.log.push("→ 启动 SOCKS5 代理 (1080) ...".to_string());
        match self.proxy_worker.spawn(&["proxy", "start", "--port", "1080"], &[]) {
            Ok(()) => self.log.push("SOCKS5 代理已启动".to_string()),
            Err(e) => self.log.push(format!("✗ 代理启动失败: {e}")),
        }
        self.refresh_status();
    }

    fn stop_proxy(&mut self) {
        self.log.push("→ 停止 SOCKS5 代理".to_string());
        self.proxy_worker.kill();
        self.refresh_status();
    }

    fn start_tun0(&mut self) {
        self.log.push("→ 启动 tun0（需要系统授权）...".to_string());
        match start_tun0_privileged(&self.log) {
            Ok(()) => self.log.push("已请求以 root 启动 tun0".to_string()),
            Err(e) => self.log.push(format!("✗ tun0 启动失败: {e}")),
        }
        self.refresh_status();
    }

    fn stop_tun0(&mut self) {
        self.log.push("→ 停止 tun0（需要系统授权）...".to_string());
        match stop_tun0_privileged(&self.log) {
            Ok(()) => self.log.push("已请求停止 tun0".to_string()),
            Err(e) => self.log.push(format!("✗ tun0 停止失败: {e}")),
        }
        self.refresh_status();
    }
}

/// Start tun0 as root via a system authorization prompt (macOS osascript /
/// Linux pkexec). The process is spawned detached (nohup ... &) so the auth
/// dialog returns quickly and the worker keeps running independently; status
/// is detected via pgrep.
fn start_tun0_privileged(log: &LogBuf) -> Result<(), String> {
    let teamx = exe_path().display().to_string();
    // Build a shell line that exports the mTLS env (if any) and launches
    // `teamx tun0 start` detached with a log file.
    let mut env_prefix = String::new();
    for k in ["TEAMX_HOME", "TEAMX_DB", "TEAMX_SERVER_URL", "TEAMX_MTLS_CERT", "TEAMX_MTLS_KEY", "TEAMX_MTLS_CA"] {
        if let Ok(v) = std::env::var(k) {
            env_prefix.push_str(&format!("export {}='{}'; ", k, v.replace('\'', "'\\''")));
        }
    }
    let cmd = format!(
        "{}nohup '{}' tun0 start > /tmp/teamx-tun0.log 2>&1 &",
        env_prefix, teamx
    );
    run_privileged(&cmd, log)
}

fn stop_tun0_privileged(log: &LogBuf) -> Result<(), String> {
    let cmd = "pkill -f 'teamx tun0 start' 2>/dev/null; pkill -f 'tun0 start' 2>/dev/null".to_string();
    run_privileged(&cmd, log)
}

/// Run a shell command with elevated privileges, pumping its output to the log.
#[cfg(target_os = "macos")]
fn run_privileged(cmd: &str, log: &LogBuf) -> Result<(), String> {
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        cmd.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("授权失败或被取消: {err}"));
    }
    log.push(format!("(sudo) {cmd}"));
    Ok(())
}

/// Linux: use pkexec to run the command as root.
#[cfg(target_os = "linux")]
fn run_privileged(cmd: &str, log: &LogBuf) -> Result<(), String> {
    let output = Command::new("pkexec")
        .args(["sh", "-c", cmd])
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("授权失败或被取消: {err}"));
    }
    log.push(format!("(sudo) {cmd}"));
    Ok(())
}

/// Whether a tun0 worker process is currently running (detected by pgrep,
/// since a root-launched process is not a child of this process).
fn is_tun0_running() -> bool {
    let out = Command::new("pgrep").args(["-f", "teamx tun0 start"]).output();
    if let Ok(o) = out {
        return o.status.success() && !o.stdout.is_empty();
    }
    false
}

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh_status();
        apply_style(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(style_bg()).inner_margin(16.0))
            .show(ctx, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Teamx").size(24.0).strong().color(style_accent()));
                    ui.label(egui::RichText::new("控制面板").size(14.0).color(style_muted()));
                });
                ui.add_space(2.0);
                ui.label(egui::RichText::new("tun0 透明代理 · SOCKS5 代理").size(12.0).color(style_muted()));
                ui.add_space(12.0);

                // --- tun0 card ---
                let act = status_card(ui, "tun0 虚拟网卡", "透明代理 · 需 root", self.tun0_running);
                match act {
                    CardAction::Start => self.start_tun0(),
                    CardAction::Stop => self.stop_tun0(),
                    CardAction::None => {}
                }

                ui.add_space(10.0);

                // --- SOCKS5 proxy card ---
                let act = status_card(ui, "SOCKS5 代理", "本地端口 1080", self.proxy_running);
                match act {
                    CardAction::Start => self.start_proxy(),
                    CardAction::Stop => self.stop_proxy(),
                    CardAction::None => {}
                }

                ui.add_space(10.0);

                // --- exit card ---
                egui::Frame::group(ui.style())
                    .fill(card_bg())
                    .corner_radius(8.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("默认出口").size(13.0).strong().color(style_fg()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new(&self.exit_name).size(13.0).color(style_accent()));
                            });
                        });
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("用 `teamx proxy routes set-default <exit>` 修改").size(11.0).color(style_muted()));
                    });

                ui.add_space(10.0);

                // --- log toggle ---
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new(if self.show_log { "隐藏日志" } else { "显示日志" })
                                .size(12.0)
                                .color(style_fg()),
                        ).fill(card_bg()).corner_radius(6.0))
                        .clicked()
                    {
                        self.show_log = !self.show_log;
                    }
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("清空日志").size(12.0).color(style_fg()),
                        ).fill(card_bg()).corner_radius(6.0))
                        .clicked()
                    {
                        self.log.0.lock().unwrap().clear();
                    }
                });

                // --- log panel ---
                if self.show_log {
                    ui.add_space(6.0);
                    let lines = self.log.snapshot();
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add_space(2.0);
                            for line in &lines {
                                let colored = if line.starts_with("✗") || line.contains("error") {
                                    red()
                                } else if line.starts_with("→") {
                                    style_accent()
                                } else {
                                    style_fg()
                                };
                                ui.label(egui::RichText::new(line).size(11.0).color(colored));
                            }
                        });
                }

                ui.add_space(12.0);
                if ui
                    .add(egui::Button::new(egui::RichText::new("退出").size(13.0).color(style_fg())).fill(card_bg()).corner_radius(6.0))
                    .clicked()
                {
                    self.proxy_worker.kill();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

        // Refresh every 2 seconds.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }
}

/// What button the user pressed on a status card.
enum CardAction {
    Start,
    Stop,
    None,
}

/// A status card: title + description + on/off badge + start/stop buttons.
fn status_card(ui: &mut egui::Ui, title: &str, desc: &str, running: bool) -> CardAction {
    let mut action = CardAction::None;
    egui::Frame::group(ui.style())
        .fill(card_bg())
        .corner_radius(8.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).size(14.0).strong().color(style_fg()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, running);
                });
            });
            ui.add_space(2.0);
            ui.label(egui::RichText::new(desc).size(11.0).color(style_muted()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("启动").color(style_fg())).fill(btn_start_bg()).corner_radius(6.0))
                    .clicked()
                {
                    action = CardAction::Start;
                }
                ui.add_space(6.0);
                if ui
                    .add(egui::Button::new(egui::RichText::new("停止").color(style_fg())).fill(btn_stop_bg()).corner_radius(6.0))
                    .clicked()
                {
                    action = CardAction::Stop;
                }
            });
        });
    action
}

/// A green/red status pill.
fn badge(ui: &mut egui::Ui, on: bool) {
    let (text, color, bg) = if on {
        ("运行中", green(), rgba(46, 204, 113, 36))
    } else {
        ("已停止", red(), rgba(231, 76, 60, 30))
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).strong().color(color));
        });
}

// ---------------------------------------------------------------------------
// Style helpers
// ---------------------------------------------------------------------------

fn rgba(r: u8, g: u8, b: u8, a: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}
fn style_bg() -> egui::Color32 { rgba(18, 22, 32, 255) }
fn card_bg() -> egui::Color32 { rgba(28, 34, 48, 255) }
fn style_accent() -> egui::Color32 { rgba(80, 160, 255, 255) }
fn style_fg() -> egui::Color32 { rgba(232, 238, 248, 255) }
fn style_muted() -> egui::Color32 { rgba(140, 150, 168, 255) }
fn green() -> egui::Color32 { rgba(60, 210, 130, 255) }
fn red() -> egui::Color32 { rgba(240, 90, 80, 255) }
fn btn_start_bg() -> egui::Color32 { rgba(40, 140, 90, 255) }
fn btn_stop_bg() -> egui::Color32 { rgba(180, 60, 55, 255) }

fn apply_style(ctx: &egui::Context) {
    let mut visual = egui::Visuals::dark();
    visual.panel_fill = style_bg();
    visual.window_fill = style_bg();
    visual.extreme_bg_color = card_bg();
    visual.faint_bg_color = rgba(34, 40, 56, 255);
    visual.widgets.noninteractive.bg_fill = card_bg();
    visual.widgets.inactive.bg_fill = rgba(42, 50, 68, 255);
    visual.widgets.hovered.bg_fill = rgba(52, 62, 84, 255);
    visual.widgets.active.bg_fill = rgba(60, 72, 96, 255);
    visual.widgets.noninteractive.fg_stroke.color = style_fg();
    visual.widgets.inactive.fg_stroke.color = style_fg();
    visual.selection.bg_fill = style_accent();
    ctx.set_visuals(visual);
}

/// Path to the teamx binary (this executable).
fn exe_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "teamx".into())
}

/// Load a CJK system font into egui so Chinese text renders (not tofu).
fn setup_cjk_font(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};

    let candidates: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
        "C:/Windows/Fonts/msyh.ttc",
    ];
    let mut found: Option<Vec<u8>> = None;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            found = Some(bytes);
            break;
        }
    }
    let Some(bytes) = found else { return };
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), std::sync::Arc::new(FontData::from_owned(bytes)));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Read the default exit from the SQLite route table (best-effort).
fn current_default_exit() -> String {
    let out = Command::new(exe_path()).args(["proxy", "routes", "list", "--json"]).output();
    if let Ok(o) = out {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
            if let Some(d) = v.get("default").and_then(|d| d.as_str()) {
                return d.to_string();
            }
        }
    }
    "(none)".to_string()
}

/// Blocking entrypoint: run the native control-panel window.
pub fn run_panel() -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([500.0, 520.0])
            .with_title("Teamx 控制面板"),
        ..Default::default()
    };
    eframe::run_native(
        "Teamx 控制面板",
        options,
        Box::new(|cc| Ok(Box::new(PanelApp::new(cc)))),
    )
    .map_err(|e| format!("panel: {e}"))
}
