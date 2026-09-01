//! teamx-win — Windows GUI launcher.
//!
//! Double-clicking `teamx-win.exe` opens the teamx member-side panel
//! (`gui-member`: import invitation letter, tunnel port mappings, SOCKS5
//! proxy) directly — no terminal, no subcommands.
//!
//! On Windows the binary uses the window subsystem so no console window is
//! created on launch; errors are surfaced inside the panel / an error dialog
//! instead of a terminal. Requires the `gui` feature (enforced in Cargo.toml
//! via `required-features`).

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(feature = "gui")]
    {
        match teamx::gui_member_panel::run_panel() {
            Ok(()) => {}
            Err(e) => {
                // The GUI entry point failed before a window could show (e.g.
                // no display / GPU context). Surface it in a message box on
                // Windows, stderr elsewhere.
                #[cfg(windows)]
                {
                    let _ = windows_message_box(&format!("teamx-win error: {e}"));
                }
                #[cfg(not(windows))]
                {
                    eprintln!("teamx-win error: {e}");
                }
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(feature = "gui"))]
    {
        eprintln!("teamx-win requires the `gui` feature — rebuild with --features gui");
        std::process::exit(1);
    }
}

/// Show a native Windows message box (error surface for a GUI app with no
/// console). Returns 0 on success / a Win32 error code on failure.
#[cfg(windows)]
fn windows_message_box(message: &str) -> i32 {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: MessageBoxW only reads the null-terminated wide string.
    unsafe {
        winapi_fallback::MessageBoxW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            "teamx-win".encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>().as_ptr(),
            0x10, // MB_ICONERROR
        )
    }
}

/// Minimal `MessageBoxW` binding via `#[link(name = "user32")]` so we do not
/// need the `windows` crate for one call.
#[cfg(windows)]
mod winapi_fallback {
    #[link(name = "user32")]
    extern "system" {
        pub fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
}
