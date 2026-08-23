//! gui_panel.rs — native control-panel window for the teamx tray (L1).
//!
//! `teamx gui-panel` opens an egui/eframe window showing the status of the
//! tun0 proxy and SOCKS5 proxy with start/stop controls and the default exit.
//! All operations go through the `teamx` CLI as child processes (the same
//! binary this panel is built from), so the panel is a thin UI over the CLI.

use std::process::{Child, Command, Stdio};

/// The control panel application state.
pub struct PanelApp {
    tun0_running: bool,
    proxy_running: bool,
    last_msg: String,
    exit_name: String,
    workers: WorkerSet,
}

struct WorkerSet {
    tun0: Option<Child>,
    proxy: Option<Child>,
}

impl Default for WorkerSet {
    fn default() -> Self {
        WorkerSet { tun0: None, proxy: None }
    }
}

impl PanelApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Load a CJK-capable system font so Chinese labels render (egui's
        // default font has no CJK glyphs -> tofu boxes).
        setup_cjk_font(&cc.egui_ctx);
        PanelApp {
            tun0_running: false,
            proxy_running: false,
            last_msg: String::new(),
            exit_name: current_default_exit(),
            workers: WorkerSet::default(),
        }
    }

    fn refresh_status(&mut self) {
        self.tun0_running = self.workers.tun0.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false);
        self.proxy_running = self.workers.proxy.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false);
        self.exit_name = current_default_exit();
    }

    fn start_tun0(&mut self) {
        if let Some(mut c) = self.workers.tun0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut cmd = Command::new(exe_path());
        cmd.args(["tun0", "start"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        self.workers.tun0 = cmd.spawn().ok();
        self.last_msg = "tun0 start requested".to_string();
        self.refresh_status();
    }

    fn stop_tun0(&mut self) {
        if let Some(mut c) = self.workers.tun0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.last_msg = "tun0 stopped".to_string();
        self.refresh_status();
    }

    fn start_proxy(&mut self) {
        if let Some(mut c) = self.workers.proxy.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut cmd = Command::new(exe_path());
        cmd.args(["proxy", "start", "--port", "1080"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        self.workers.proxy = cmd.spawn().ok();
        self.last_msg = "SOCKS5 proxy start requested".to_string();
        self.refresh_status();
    }

    fn stop_proxy(&mut self) {
        if let Some(mut c) = self.workers.proxy.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.last_msg = "SOCKS5 proxy stopped".to_string();
        self.refresh_status();
    }
}

/// Path to the teamx binary (this executable).
fn exe_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "teamx".into())
}

/// Load a CJK system font into egui so Chinese text renders (not tofu).
/// Tries known font paths per platform; best-effort (silently no-op on
/// failure — Latin text still works).
fn setup_cjk_font(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily, FontId};

    let candidates: &[&str] = &[
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Linux
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
        // Windows
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
    fonts.font_data.insert(
        "cjk".to_owned(),
        std::sync::Arc::new(FontData::from_owned(bytes)),
    );
    // Put CJK font as fallback for both proportional and monospace families.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    let _ = FontId::default(); // silence unused import in some builds
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

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh_status();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Teamx");
            ui.label("tun0 透明代理 & SOCKS5 代理控制面板");
            ui.separator();

            // tun0 card
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("tun0 虚拟网卡");
                    ui.label(if self.tun0_running { "🟢 on" } else { "⚪ off" });
                });
                ui.label("透明代理（需 root）");
                ui.horizontal(|ui| {
                    if ui.button("启动").clicked() {
                        self.start_tun0();
                    }
                    if ui.button("停止").clicked() {
                        self.stop_tun0();
                    }
                });
            });

            ui.add_space(6.0);

            // SOCKS5 proxy card
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("SOCKS5 代理");
                    ui.label(if self.proxy_running { "🟢 on" } else { "⚪ off" });
                });
                ui.label("本地端口 1080");
                ui.horizontal(|ui| {
                    if ui.button("启动").clicked() {
                        self.start_proxy();
                    }
                    if ui.button("停止").clicked() {
                        self.stop_proxy();
                    }
                });
            });

            ui.add_space(6.0);

            // exit info
            ui.group(|ui| {
                ui.strong("默认出口");
                ui.label(format!("{}", self.exit_name));
                ui.label("用 `teamx proxy routes set-default <exit>` 修改");
            });

            if !self.last_msg.is_empty() {
                ui.add_space(6.0);
                ui.label(&self.last_msg);
            }

            ui.add_space(10.0);
            if ui.button("退出").clicked() {
                self.stop_tun0();
                self.stop_proxy();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });

        // Refresh every 2 seconds.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }
}

/// Blocking entrypoint: run the native control-panel window.
pub fn run_panel() -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 380.0])
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
