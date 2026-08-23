//! gui.rs — L1 desktop tray app (cross-platform: macOS menu bar / Linux tray).
//!
//! `teamx gui` shows a tray icon that lets the user start/stop the tun0
//! proxy and a SOCKS5 proxy, switch the default exit, and see live status —
//! without opening a terminal. It spawns the actual workers as child
//! processes (`teamx tun0 start` / `teamx proxy start`) and manages their
//! lifecycle, so the tray itself stays lightweight.
//!
//! Built on `tray-icon` + `tao` (pure Rust, macOS menu bar + Linux appindicator).

use std::io::Write;
use std::process::{Child, Command, Stdio};

/// Current state of one managed worker process.
#[derive(Default)]
pub struct ManagedProc {
    child: Option<Child>,
}

impl ManagedProc {
    pub fn is_running(&mut self) -> bool {
        self.child.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false)
    }

    /// Spawn (or replace) the worker with the given args + env.
    pub fn spawn(&mut self, args: &[&str], envs: &[(&str, String)]) -> Result<(), String> {
        self.kill();
        let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", args.join(" ")))?;
        self.child = Some(child);
        Ok(())
    }

    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Everything the tray UI needs to drive.
pub struct GuiState {
    pub tun0: ManagedProc,
    pub proxy: ManagedProc,
}

impl GuiState {
    pub fn new() -> Self {
        GuiState { tun0: ManagedProc::default(), proxy: ManagedProc::default() }
    }
}

/// Open the native control-panel window (spawn `teamx gui-panel`).
fn open_control_panel() {
    let _ = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string()).unwrap_or_else(|_| "teamx".into()))
        .arg("gui-panel")
        .spawn();
}

/// Environment shared by worker processes (mTLS material etc.) — read from
/// the current process env so the user can launch the tray from a configured
/// shell.
fn worker_env() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for k in [
        "TEAMX_HOME", "TEAMX_DB", "TEAMX_SERVER_URL",
        "TEAMX_MTLS_CERT", "TEAMX_MTLS_KEY", "TEAMX_MTLS_CA",
    ] {
        if let Ok(v) = std::env::var(k) {
            let key: &'static str = Box::leak(k.to_string().into_boxed_str());
            out.push((key, v));
        }
    }
    out
}

/// The L1 tray entrypoint. Blocks forever (event loop).
pub fn run_tray() -> Result<(), String> {
    // Startup diagnostics: write to a log file so LaunchServices launches
    // (which drop stdout/stderr) can be debugged.
    let log_path = std::env::temp_dir().join("teamx-gui.log");
    let mut log = std::io::BufWriter::new(
        std::fs::File::create(&log_path).expect("create teamx-gui.log in temp dir"),
    );
    let _ = writeln!(log, "teamx gui starting at {}", crate::db::now());
    let _ = log.flush();
    eprintln!("teamx gui: log -> {}", log_path.display());

    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

    enum UserEvent {
        Menu(MenuEvent),
        Tray(TrayIconEvent),
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Tray(e));
    }));
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    // Build the menu.
    let menu = Menu::new();
    let status_item = MenuItem::new("status: idle", false, None); // disabled label
    let open_panel = MenuItem::new("Open Control Panel", true, None);
    let start_tun = MenuItem::new("Start tun0", true, None);
    let stop_tun = MenuItem::new("Stop tun0", true, None);
    let start_proxy = MenuItem::new("Start SOCKS5 proxy", true, None);
    let stop_proxy = MenuItem::new("Stop SOCKS5 proxy", true, None);
    let quit = MenuItem::new("Quit teamx", true, None);
    menu.append_items(&[
        &status_item,
        &open_panel,
        &PredefinedMenuItem::separator(),
        &start_tun,
        &stop_tun,
        &start_proxy,
        &stop_proxy,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| format!("menu: {e}"))?;

    let mut tray: Option<tray_icon::TrayIcon> = None;
    let mut state = GuiState::new();
    let envs = worker_env();

    // Tray icon: load the teamx logo PNG if available (env TEAMX_TRAY_ICON,
    // or a sibling resources path), else fall back to a placeholder square.
    let icon_rgba: Option<(Vec<u8>, u32, u32)> = load_tray_icon_png();

    event_loop.run(move |event, _elwt, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let icon = match &icon_rgba {
                    Some((px, w, h)) => Icon::from_rgba(px.clone(), *w, *h),
                    None => Icon::from_rgba(placeholder_icon(), 16, 16),
                };
                let icon = match icon {
                    Ok(i) => i,
                    Err(e) => {
                        let _ = writeln!(log, "tray icon error: {e}");
                        let _ = log.flush();
                        eprintln!("tray icon: {e}");
                        return;
                    }
                };
                tray = Some(
                    match TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip("teamx")
                        .with_icon(icon)
                        .build()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = writeln!(log, "tray build error: {e}");
                            let _ = log.flush();
                            eprintln!("tray: {e}");
                            return;
                        }
                    },
                );
                let _ = writeln!(log, "tray built OK");
                let _ = log.flush();
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == open_panel.id() {
                    open_control_panel();
                } else if e.id == start_tun.id() {
                    match state.tun0.spawn(&["tun0", "start"], &envs) {
                        Ok(_) => status_item.set_text("tun0: starting…"),
                        Err(err) => status_item.set_text(&format!("tun0 error: {err}")),
                    }
                } else if e.id == stop_tun.id() {
                    state.tun0.kill();
                    status_item.set_text("status: tun0 stopped");
                } else if e.id == start_proxy.id() {
                    match state.proxy.spawn(&["proxy", "start", "--port", "1080"], &envs) {
                        Ok(_) => status_item.set_text("proxy: starting…"),
                        Err(err) => status_item.set_text(&format!("proxy error: {err}")),
                    }
                } else if e.id == stop_proxy.id() {
                    state.proxy.kill();
                    status_item.set_text("status: proxy stopped");
                } else if e.id == quit.id() {
                    state.tun0.kill();
                    state.proxy.kill();
                    tray.take();
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::Tray(_e)) => {
                // Clicking the tray icon opens the native control panel.
                open_control_panel();
                // And refresh the status line.
                let s = format!(
                    "tun0: {}  proxy: {}",
                    if state.tun0.is_running() { "on" } else { "off" },
                    if state.proxy.is_running() { "on" } else { "off" },
                );
                status_item.set_text(&s);
            }
            _ => {}
        }
    });

    // event_loop.run() never returns normally; this keeps the Result type.
    #[allow(unreachable_code)]
    Ok(())
}

/// Load the tray icon from a PNG. Resolution order:
///   1. `TEAMX_TRAY_ICON` env var
///   2. `<current_exe_dir>/../Resources/tray.png` (inside a .app bundle)
///   3. a `tray.png` next to the binary
/// Returns `(rgba, width, height)` or `None` to fall back to the placeholder.
fn load_tray_icon_png() -> Option<(Vec<u8>, u32, u32)> {
    use std::io::BufReader;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("TEAMX_TRAY_ICON") {
        candidates.push(std::path::PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Inside a .app bundle: <app>/Contents/MacOS/teamx
            // Resources live at <app>/Contents/Resources/tray.png
            if let Some(contents) = dir.parent() {
                candidates.push(contents.join("Resources").join("tray.png"));
            }
            candidates.push(dir.join("tray.png"));
        }
    }

    for path in candidates {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let decoder = match image::ImageReader::new(BufReader::new(file)).with_guessed_format() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let img = match decoder.decode() {
            Ok(i) => i,
            Err(_) => continue,
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        return Some((rgba.into_raw(), w, h));
    }
    None
}

/// 16x16 placeholder (used only when no logo PNG is found).
fn placeholder_icon() -> Vec<u8> {
    let mut px = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16u32 {
        for x in 0..16u32 {
            let on = (x / 4 + y / 4) % 2 == 0;
            if on {
                px.extend_from_slice(&[40, 120, 220, 255]); // blue
            } else {
                px.extend_from_slice(&[20, 40, 80, 255]);
            }
        }
    }
    px
}
