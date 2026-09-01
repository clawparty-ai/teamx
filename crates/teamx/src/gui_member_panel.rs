//! gui_member_panel.rs — member-side window for the teamx tray (L1, cross-platform).
//!
//! `teamx gui-member` opens an egui/eframe window for a *team member* (not the
//! owner): import an invitation letter, manage reverse-tunnel port mappings
//! (expose a local service / forward a teammate's tunnel), toggle the SOCKS5
//! proxy, and watch a live log. All operations go through the `teamx` CLI as
//! child processes (same thin-shell architecture as the macOS Swift app), so
//! this panel is fully cross-platform: macOS / Linux / Windows.
//!
//! Unlike `teamx gui-panel` (owner-side tun0 + SOCKS5 controls), this panel has
//! no privileged operations — tun0 is deliberately not offered here.

use std::collections::HashMap;
use std::process::Command;

use crate::gui_panel::{exe_path, setup_cjk_font, LogBuf, Worker};
use crate::gui_panel::{
    apply_style, btn_start_bg, btn_stop_bg, card_bg, green, red, style_accent, style_bg, style_fg,
    style_muted,
};

/// Session key used for every CLI call from the member panel.
const SESSION: &str = "member-panel";

/// A parsed `teamx tunnel list` entry.
struct TunnelEntry {
    name: String,
    mode: String,
    port: i64,
    #[allow(dead_code)] // provider identity shown in future UI
    provider_member_id: Option<String>,
    target_port: Option<i64>,
    #[allow(dead_code)] // provider LAN IP for direct access hints
    lan_ip: Option<String>,
}

/// The member panel application state.
pub struct MemberPanelApp {
    // import
    letter_input: String,
    name_input: String,
    import_done: String,
    // env forwarded to child processes (server url + mTLS material)
    server_url: String,
    // tunnel ops
    tunnels: Vec<TunnelEntry>,
    expose_name: String,
    expose_port: String,
    forward_name: String,
    /// active long-lived tunnel workers: `expose:<name>` / `forward:<name>`
    tunnel_workers: HashMap<String, Worker>,
    // proxy
    proxy_worker: Worker,
    proxy_running: bool,
    // log
    log: LogBuf,
    show_log: bool,
    wants_close: bool,
}

impl MemberPanelApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_font(&cc.egui_ctx);
        let log = LogBuf::new();
        log.push("Teamx 成员端已启动".to_string());
        let proxy_worker = Worker::new("proxy", &log);
        let server_url = std::env::var("TEAMX_SERVER_URL").unwrap_or_default();
        let mut app = MemberPanelApp {
            letter_input: String::new(),
            name_input: String::new(),
            import_done: String::new(),
            server_url,
            tunnels: Vec::new(),
            expose_name: String::new(),
            expose_port: String::new(),
            forward_name: String::new(),
            tunnel_workers: HashMap::new(),
            proxy_worker,
            proxy_running: false,
            log,
            show_log: false,
            wants_close: false,
        };
        app.refresh();
        app
    }

    /// Env vars passed to every CLI child (server URL + mTLS material).
    fn envs(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        if !self.server_url.is_empty() {
            out.push(("TEAMX_SERVER_URL", self.server_url.clone()));
        }
        for k in [
            "TEAMX_HOME", "TEAMX_DB",
            "TEAMX_MTLS_CERT", "TEAMX_MTLS_KEY", "TEAMX_MTLS_CA",
        ] {
            if let Ok(v) = std::env::var(k) {
                let key: &'static str = Box::leak(k.to_string().into_boxed_str());
                out.push((key, v));
            }
        }
        out
    }

    /// Run a one-shot `teamx <args>` and capture stdout (for parsing).
    fn run_capture(&self, args: &[&str]) -> (bool, String) {
        let envs = self.envs();
        let mut cmd = Command::new(exe_path());
        cmd.args(args).stdin(std::process::Stdio::null());
        for (k, v) in &envs {
            cmd.env(k, v);
        }
        match cmd.output() {
            Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
            Err(e) => (false, format!("{e}")),
        }
    }

    /// Run a one-shot command and push the outcome into the log.
    fn run_logged(&mut self, label: &str, args: &[&str]) {
        self.log.push(format!("→ {label}: teamx {}", args.join(" ")));
        let envs = self.envs();
        let mut cmd = Command::new(exe_path());
        cmd.args(args).stdin(std::process::Stdio::null());
        for (k, v) in &envs {
            cmd.env(k, v);
        }
        match cmd.output() {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if o.status.success() {
                    self.log.push(format!("✓ {label}: {}", stdout.trim()));
                } else {
                    self.log.push(format!(
                        "✗ {label} 失败 ({}): {}",
                        o.status,
                        if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() }
                    ));
                }
            }
            Err(e) => self.log.push(format!("✗ {label}: {e}")),
        }
    }

    // ---- import ----

    fn do_import(&mut self) {
        let letter = self.letter_input.trim().to_string();
        if letter.is_empty() {
            self.log.push("✗ 请粘贴邀请函（teamx-inv:v1:...）或 letter 文件路径".to_string());
            return;
        }
        self.log.push("→ 导入邀请函…".to_string());
        let mut args = vec!["team", "import", &letter, "--session", SESSION];
        let mut extra: Vec<String> = Vec::new();
        if !self.name_input.trim().is_empty() {
            extra.push("--name".into());
            extra.push(self.name_input.trim().into());
        }
        args.extend(extra.iter().map(|s| s.as_str()));
        let (ok, out) = self.run_capture(&args);
        self.log.push(
            if ok { "✓ 邀请函导入成功".to_string() } else { "✗ 邀请函导入失败".to_string() },
        );
        self.log.push(format!("  {}", out.trim()));
        if ok {
            // Pick up the server URL embedded in the letter.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
                if let Some(url) = v.get("server_url").and_then(|s| s.as_str()) {
                    if !url.is_empty() {
                        self.server_url = url.to_string();
                        self.log.push(format!("→ 服务器: {url}"));
                    }
                }
            }
            self.import_done = "已导入".to_string();
        } else {
            self.import_done = "导入失败".to_string();
        }
        self.refresh();
    }

    // ---- tunnels ----

    /// Refresh `tunnel list` into `self.tunnels`.
    fn refresh_tunnels(&mut self) {
        let (ok, out) = self.run_capture(&["tunnel", "list", "--json", "--session", SESSION]);
        if !ok {
            self.tunnels.clear();
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) else {
            self.tunnels.clear();
            return;
        };
        let data = v.get("data").unwrap_or(&v);
        let Some(arr) = data.get("tunnels").and_then(|t| t.as_array()) else {
            self.tunnels.clear();
            return;
        };
        self.tunnels = arr
            .iter()
            .filter_map(|d| {
                let name = d.get("name")?.as_str()?.to_string();
                Some(TunnelEntry {
                    name,
                    mode: d.get("mode").and_then(|m| m.as_str()).unwrap_or("local").to_string(),
                    port: d.get("port").and_then(|p| p.as_i64()).unwrap_or(0),
                    provider_member_id: d.get("provider_member_id").and_then(|m| m.as_str()).map(String::from),
                    target_port: d.get("target_port").and_then(|p| p.as_i64()),
                    lan_ip: d.get("lan_ip").and_then(|m| m.as_str()).map(String::from),
                })
            })
            .collect();
    }

    /// Whether a tunnel worker (expose/forward) is running locally.
    fn tunnel_running(&mut self, kind: &str, name: &str) -> bool {
        let key = format!("{kind}:{name}");
        self.tunnel_workers
            .get_mut(&key)
            .map(|w| w.is_running())
            .unwrap_or(false)
    }

    fn do_expose(&mut self) {
        let name = self.expose_name.trim().to_string();
        let port: u16 = match self.expose_port.trim().parse() {
            Ok(p) if p > 0 => p,
            _ => {
                self.log.push("✗ 请输入合法的本地端口（如 8080）".to_string());
                return;
            }
        };
        let key = format!("expose:{name}");
        let worker = Worker::new(Box::leak(key.clone().into_boxed_str()), &self.log);
        let mut w = worker;
        let args = vec!["tunnel", "expose", &name, "--port", &self.expose_port, "--session", SESSION];
        match w.spawn(&args, &self.envs()) {
            Ok(()) => {
                self.log.push(format!("→ 已启动 expose {name}:{port}"));
                self.tunnel_workers.insert(key, w);
            }
            Err(e) => self.log.push(format!("✗ expose 失败: {e}")),
        }
    }

    fn do_forward(&mut self) {
        let name = self.forward_name.trim().to_string();
        if name.is_empty() {
            self.log.push("✗ 请输入要转发的隧道名称".to_string());
            return;
        }
        let key = format!("forward:{name}");
        let worker = Worker::new(Box::leak(key.clone().into_boxed_str()), &self.log);
        let mut w = worker;
        let args = vec!["tunnel", "forward", &name, "--session", SESSION];
        match w.spawn(&args, &self.envs()) {
            Ok(()) => {
                self.log.push(format!("→ 已启动 forward {name}"));
                self.tunnel_workers.insert(key, w);
            }
            Err(e) => self.log.push(format!("✗ forward 失败: {e}")),
        }
    }

    fn do_close_tunnel(&mut self, name: &str) {
        // Kill any local worker and ask the server to release the tunnel.
        let keys = [
            format!("expose:{name}"),
            format!("forward:{name}"),
        ];
        for k in keys {
            if let Some(mut w) = self.tunnel_workers.remove(&k) {
                w.kill();
            }
        }
        self.run_logged(&format!("关闭隧道 {name}"), &["tunnel", "close", name, "--session", SESSION]);
        self.refresh();
    }

    // ---- proxy ----

    fn do_start_proxy(&mut self) {
        match self.proxy_worker.spawn(&["proxy", "start", "--port", "1080"], &self.envs()) {
            Ok(()) => self.log.push("→ SOCKS5 代理启动中 (127.0.0.1:1080)…".to_string()),
            Err(e) => self.log.push(format!("✗ 代理启动失败: {e}")),
        }
    }

    fn do_stop_proxy(&mut self) {
        self.proxy_worker.kill();
        self.log.push("→ SOCKS5 代理已停止".to_string());
    }

    fn refresh(&mut self) {
        self.proxy_running = self.proxy_worker.is_running();
        self.refresh_tunnels();
    }
}

impl eframe::App for MemberPanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh();
        apply_style(ctx);

        // Same close guard as gui_panel: only exit on the 退出 button.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.wants_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(style_bg()).inner_margin(16.0))
            .show(ctx, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Teamx").size(24.0).strong().color(style_accent()));
                    ui.label(egui::RichText::new("成员端").size(14.0).color(style_muted()));
                    if !self.server_url.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("服务器: {}", self.server_url))
                                    .size(11.0)
                                    .color(style_muted()),
                            );
                        });
                    }
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("导入邀请函 · 隧道端口映射 · SOCKS5 代理")
                        .size(12.0)
                        .color(style_muted()),
                );
                ui.add_space(12.0);

                // --- import letter card ---
                egui::Frame::group(ui.style())
                    .fill(card_bg())
                    .corner_radius(8.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("① 导入邀请函").size(13.0).strong().color(style_fg()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !self.import_done.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&self.import_done).size(12.0).color(green()),
                                    );
                                }
                            });
                        });
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("粘贴 owner 发的邀请函（teamx-inv:v1:...）或 letter 文件路径")
                                .size(11.0)
                                .color(style_muted()),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            egui::TextEdit::multiline(&mut self.letter_input)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("teamx-inv:v1:... 或 /path/to/letter.json"),
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("显示名").size(12.0).color(style_fg()));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.name_input)
                                    .desired_width(180.0)
                                    .hint_text("可选"),
                            );
                            ui.add_space(8.0);
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("导入").color(style_fg()),
                                ).fill(btn_start_bg()).corner_radius(6.0))
                                .clicked()
                            {
                                self.do_import();
                            }
                        });
                    });

                ui.add_space(10.0);

                // --- tunnel card ---
                egui::Frame::group(ui.style())
                    .fill(card_bg())
                    .corner_radius(8.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("② 隧道端口映射").size(13.0).strong().color(style_fg()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("刷新").size(12.0).color(style_fg()),
                                    ).fill(card_bg()).corner_radius(6.0))
                                    .clicked()
                                {
                                    self.refresh_tunnels();
                                }
                            });
                        });
                        ui.add_space(6.0);

                        // Expose a local service.
                        ui.label(egui::RichText::new("暴露本地服务（provider）").size(11.0).color(style_muted()));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("名称").size(12.0).color(style_fg()));
                            ui.add(egui::TextEdit::singleline(&mut self.expose_name).desired_width(110.0).hint_text("如 httpbin"));
                            ui.label(egui::RichText::new("端口").size(12.0).color(style_fg()));
                            ui.add(egui::TextEdit::singleline(&mut self.expose_port).desired_width(70.0).hint_text("8080"));
                            ui.add_space(8.0);
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("暴露").color(style_fg()),
                                ).fill(btn_start_bg()).corner_radius(6.0))
                                .clicked()
                            {
                                self.do_expose();
                            }
                        });
                        ui.add_space(6.0);

                        // Forward a teammate's tunnel.
                        ui.label(egui::RichText::new("转发队友隧道（consumer）").size(11.0).color(style_muted()));
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("名称").size(12.0).color(style_fg()));
                            ui.add(egui::TextEdit::singleline(&mut self.forward_name).desired_width(160.0).hint_text("要转发的隧道名"));
                            ui.add_space(8.0);
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("转发").color(style_fg()),
                                ).fill(btn_start_bg()).corner_radius(6.0))
                                .clicked()
                            {
                                self.do_forward();
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        // Tunnel list.
                        if self.tunnels.is_empty() {
                            ui.label(egui::RichText::new("（暂无隧道）").size(12.0).color(style_muted()));
                        } else {
                            // Collect display rows first so the render closures
                            // don't borrow self mutably while iterating.
                            let names: Vec<String> = self.tunnels.iter().map(|t| t.name.clone()).collect();
                            let metas: Vec<String> = self
                                .tunnels
                                .iter()
                                .map(|t| {
                                    format!(
                                        "{}:{}",
                                        t.mode,
                                        if t.target_port.unwrap_or(t.port) > 0 {
                                            t.target_port.unwrap_or(t.port).to_string()
                                        } else {
                                            "-".to_string()
                                        }
                                    )
                                })
                                .collect();
                            let running: Vec<(bool, bool)> = names
                                .iter()
                                .map(|n| {
                                    let e = self.tunnel_running("expose", n);
                                    let f = self.tunnel_running("forward", n);
                                    (e, f)
                                })
                                .collect();
                            egui::ScrollArea::vertical()
                                .max_height(180.0)
                                .show(ui, |ui| {
                                    for i in 0..names.len() {
                                        let (e, f) = running[i];
                                        let close_name = names[i].clone();
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(&names[i]).size(12.0).strong().color(style_fg()),
                                            );
                                            ui.label(egui::RichText::new(&metas[i]).size(11.0).color(style_muted()));
                                            if e || f {
                                                ui.label(
                                                    egui::RichText::new("本地运行中").size(11.0).color(green()),
                                                );
                                            }
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui
                                                    .add(egui::Button::new(
                                                        egui::RichText::new("关闭").size(11.0).color(style_fg()),
                                                    ).fill(btn_stop_bg()).corner_radius(6.0))
                                                    .clicked()
                                                {
                                                    self.do_close_tunnel(&close_name);
                                                }
                                            });
                                        });
                                    }
                                });
                        }
                    });

                ui.add_space(10.0);

                // --- SOCKS5 proxy card ---
                let act = crate::gui_panel::status_card(
                    ui,
                    "③ SOCKS5 代理",
                    "本地端口 1080 · 通过团队出口转发",
                    self.proxy_running,
                );
                match act {
                    crate::gui_panel::CardAction::Start => self.do_start_proxy(),
                    crate::gui_panel::CardAction::Stop => self.do_stop_proxy(),
                    crate::gui_panel::CardAction::None => {}
                }

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
                        self.log.clear();
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
                    for (_, mut w) in self.tunnel_workers.drain() {
                        w.kill();
                    }
                    self.wants_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

        // Refresh every 3 seconds.
        ctx.request_repaint_after(std::time::Duration::from_secs(3));
    }
}

/// Blocking entrypoint: run the member-side panel window.
pub fn run_panel() -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 640.0])
            .with_title("Teamx 成员端"),
        ..Default::default()
    };
    eframe::run_native(
        "Teamx 成员端",
        options,
        Box::new(|cc| Ok(Box::new(MemberPanelApp::new(cc)))),
    )
    .map_err(|e| format!("member panel: {e}"))
}
