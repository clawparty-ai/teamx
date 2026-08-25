# Teamx Desktop App (Swift/AppKit) Design Document

- Document type: design (implementation blueprint, modeled on a mature macOS menu bar app)
- Related: `docs/09-design-tun0.md` (tun0 feature), `docs/20-manual-tunnel-proxy-cli.md`
- Date: 2026-08-23
- Reference: mature macOS menu bar app architecture (NSStatusItem + SMAppService privileged helper)
- Status: pending review (Phase B); implement after approval (Phase A)

## 1. Why Rewrite in Swift/AppKit

Problems with the current Rust egui/eframe approach:
1. **System modal dialogs (osascript authorization) conflict with the winit event loop** → panel crashes/closes
2. egui does not look native; visually disjointed from macOS style
3. No clean solution for privileged tun0 startup (osascript nohup and various other issues)

Mature macOS menu bar apps prove that **native AppKit + NSStatusItem + SMAppService privileged helper** is the standard, reliable architecture for macOS menu bar apps. This architecture has no crashes, no permission issues, and a native look.

## 2. Overall Architecture

```
Teamx.app (Swift + AppKit, non-sandboxed, LSUIElement=true)
├── AppDelegate.swift            # NSStatusItem + menu + lifecycle
├── Main.storyboard              # status bar menu (~12 items) + control panel window
├── Controllers/
│   ├── ControlPanelController.swift  # control panel window (NSViewController)
│   └── TeamxWindowController.swift   # generic single-window controller (common single-window pattern)
├── Managers/
│   ├── TeamxCoreManager.swift   # manages the teamx CLI subprocess (Process)
│   ├── PrivilegedHelperManager.swift # tun0 privileged operations (XPC helper)
│   └── RouteManager.swift       # route table read/write (via CLI)
├── PrivilegedHelper/            # standalone root helper target (Swift)
│   ├── TeamxPrivilegedHelper.m  # NSXPCListener + privileged operations
│   └── TeamxPrivilegedProtocol.h
├── Resources/
│   └── teamx                    # bundled Rust CLI binary (release build, copied at build time)
├── Info.plist                   # LSUIElement=true, empty entitlements (non-sandboxed)
└── Images.xcassets              # icons (generated from docs/logo)
```

## 3. Core Component Design

### 3.1 AppDelegate (Menu Bar)

Following the mature macOS menu bar app pattern:

```swift
@main
class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    @IBOutlet var statusMenu: NSMenu!

    func applicationDidFinishLaunching(_ n: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let btn = statusItem.button {
            btn.image = NSImage(named: "tray")?.withSymbolConfiguration(.init(pointSize: 14, weight: .medium))
            btn.image?.isTemplate = true   // adapts to light/dark mode
        }
        statusMenu.delegate = self
        statusItem.menu = statusMenu
        // start teamx CLI status monitoring (whether tun0/proxy are running)
        TeamxCoreManager.shared.startMonitoring()
    }
}
```

**Menu** (defined in storyboard, actions target AppDelegate):
```
Status bar icon
├─ Open Control Panel          → open the control panel window
├─ Start tun0 / Stop tun0      → privileged helper start/stop (enable/disable dynamic)
├─ Start Proxy / Stop Proxy    → start/stop SOCKS5 proxy (Process, no privileges needed)
├─ ──separator──
├─ Switch Default Exit…        → submenu lists egresses (queried via RouteManager)
├─ Show Log                    → open log window (or log file)
├─ ──separator──
└─ Quit
```

### 3.2 Control Panel Window (Native AppKit)

No egui — use **NSStackView + NSTextField + NSButton** or **SwiftUI NSHostingView**:

```
┌─ Teamx Control Panel ──────────────────────┐
│  ⚙️ Teamx                                 │
│                                             │
│  ┌─ tun0 Virtual NIC ───────── [Running/Stopped] ┐│
│  │  Transparent proxy · requires root       ││
│  │  [Start]  [Stop]                         ││
│  └──────────────────────────────────────────┘│
│  ┌─ SOCKS5 Proxy ──────────── [Running/Stopped] ┐│
│  │  Local port 1080                          ││
│  │  [Start]  [Stop]                         ││
│  └──────────────────────────────────────────┘│
│  ┌─ Default Exit ────────────────────────────┐│
│  │  egress                                ││
│  └──────────────────────────────────────────┘│
│  ┌─ Log ───────────────────────────────────┐│
│  │  [Show/Hide]  [Clear]                   ││
│  │  [scrolling log area]                   ││
│  └──────────────────────────────────────────┘│
└─────────────────────────────────────────────┘
```

- Use `NSViewController` + AutoLayout (or SwiftUI for brevity)
- Status refreshed every 2s (`Timer`) from the `TeamxCoreManager` cache
- Log area: `NSTextView` read-only + append

### 3.3 TeamxCoreManager (CLI Subprocess Management)

```swift
final class TeamxCoreManager {
    static let shared = TeamxCoreManager()
    // state cache (ObservableObject / Combine, for UI binding)
    @Published var tun0Running: Bool = false
    @Published var proxyRunning: Bool = false
    @Published var logs: [String] = []

    func startTun0()        // → PrivilegedHelperManager.shared.startTun0()
    func stopTun0()         // → PrivilegedHelperManager.shared.stopTun0()
    func startProxy()       // Process: teamx proxy start --port 1080 (capture output→logs)
    func stopProxy()        // Process kill
    func refreshStatus()    // query tun0/proxy process status (pgrep)
    private func teamxURL() -> URL  // Bundle.main.resourceURL/teamx
}
```

**Bundled teamx binary**:
- Copy the Rust `cargo build --release` output to `Resources/teamx`
- AppKit launches it with `Process`: `Process()` + `executableURL = teamxURL()` + arguments
- Capture stdout/stderr → logs (`Pipe` + reader thread)

### 3.4 PrivilegedHelperManager (tun0 Privileged Operations)

Follow the mature macOS menu bar app SMAppService/SMJobBless + XPC pattern (not osascript):

**Protocol** (ObjC, imported via bridging header):
```objc
@protocol TeamxPrivilegedProtocol <NSObject>
- (void)startTun0WithEnv:(NSDictionary *)env reply:(void(^)(NSString *))reply;
- (void)stopTun0WithReply:(void(^)(NSString *))reply;
- (void)getTun0StatusWithReply:(void(^)(BOOL))reply;
- (void)getVersion:(void(^)(NSString *))reply;
@end
```

**Helper side** (standalone target, root LaunchDaemon):
- `NSXPCListener` mach service `io.flomesh.teamx.helper`
- `startTun0`: run `teamx tun0 start` as root via `NSTask` (env carries mTLS parameters), detached
- `stopTun0`: kill the tun0 process
- Auto-exit after 5s with no connections (following the common implementation)

**Installation**: SMJobBless (modern) → fall back to AppleScript manual install on failure (following legacy fallback approaches)

**Version comparison**: helper bundle version embedded in the app vs version in `/Library/PrivilegedHelperTools/`

### 3.5 Window Management (Common Single-Window Controller)

```swift
final class TeamxWindowsRecorder {
    static let shared = ...
    var windowControllers: [NSWindowController] {
        didSet { /* empty→.accessory, non-empty→.regular */ }
    }
}
final class TeamxWindowController<T: NSViewController>: NSWindowController, NSWindowDelegate {
    static func create() -> NSWindowController { /* reuse/create + lastSize */ }
}
```

### 3.6 RouteManager (Exit Management)

- Call `teamx proxy routes list --json` to read the default exit + rules
- Menu "Switch Default Exit" submenu: lists available egresses; selecting one → `teamx proxy routes set-default <exit>`
- The control panel shows the current default exit

## 4. Build and Packaging

```
# 1. Build the Rust CLI
cargo build --release

# 2. Build the Xcode project (xcodebuild)
xcodebuild -project Teamx.xcodeproj -scheme Teamx -configuration Release \
           -derivedDataPath build build

# 3. Copy the teamx binary into .app Resources
cp target/release/teamx build/Teamx.app/Contents/Resources/teamx

# 4. Generate icons (from docs/logo)
#    logo → AppIcon.icns (iconutil) + tray icon

# 5. Sign (ad-hoc for development; Developer ID for release)
codesign --force --deep --sign - Teamx.app

# 6. Install to /Applications + LaunchAgent
```

## 5. Xcode Project Structure

```
Teamx.xcodeproj
├── target: Teamx (Swift, AppKit)
│   ├── AppDelegate.swift, Main.storyboard
│   ├── Controllers/, Managers/, Models/
│   ├── Resources/teamx (Copy Files: Resources)
│   └── Info.plist (LSUIElement)
└── target: TeamxPrivilegedHelper (ObjC + Swift mix)
    ├── TeamxPrivilegedHelper.m
    ├── TeamxPrivilegedProtocol.h
    ├── Helper-Info.plist (embedded __TEXT __info_plist)
    └── Helper-Launchd.plist (embedded __TEXT __launchd_plist)
```

## 6. Privilege Model

| Operation | Mechanism | Privileges |
|---|---|---|
| SOCKS5 proxy start/stop | Process (Resources/teamx) | none needed |
| tun0 start/stop | XPC → privileged helper (root) | first-time helper install needs admin authorization |
| Route table read/write | Process (local DB) | none needed |
| Status/logs | Process / file reads | none needed |

## 6b. Tunnel Configuration & Usage (App UI)

### 6b.1 Capability List (Mapped to CLI)

teamx tunnel offers two roles + three queries:

| Role | Command | Lifecycle | Privileges |
|---|---|---|---|
| **Provider** (expose local services) | `tunnel expose <name> --port <p> --mode local\|frp [--lan-ip]` | long-lived WS client | network-mode mTLS |
| **Consumer** (forward teammates' services) | `tunnel forward <name> [--local-port <p>]` | long-lived WS client | network-mode mTLS |
| **Queries** | `tunnel list` / `tunnel status <name>` / `tunnel close <name>` | one-shot RPC | network-mode mTLS |

Three expose modes:
- `local` (default): server binds no port; teammates map locally with `forward` (safest)
- `frp`: server allocates a public port (9100-9999); teammates connect directly via `tcp://<server>:<port>`
- `proxy`: SOCKS5 egress (already folded into proxy features; not duplicated in tunnel UI)

### 6b.2 UI Design: Add a "Tunnels" Section to the Control Panel

```
┌─ Tunnels ───────────────────────────────────────┐
│  [Expose Service]  [Forward Service]  [Refresh] │  ← top actions
│                                                 │
│  ── Services I expose (Provider) ──             │
│  │ Name      Port  Mode  Public Port  State  [Close]  │  ← table/list
│  │ svc-web    8080  local  -      Running  [×]   │
│  │ svc-frp    9090  frp    9103   Running  [×]   │
│                                                 │
│  ── Services exposed by teammates (Consumer) ── │
│  │ Name      Provider  Mode  Local Mapping  State  [Forward] │
│  │ db         exit-b  local  127.0.0.1:5432 Running│
│                                                 │
│  ── New Expose ──                               │
│  │ Name: [______]  Port: [____]  Mode: [local▼]  │
│  │ [Expose This Service]                         │
│  └──────────────────────────────────────────────┘
```

### 6b.3 Interaction Flows

**Expose a local service (Provider)**:
1. User fills in: name + local port + mode (local/frp)
2. Click "Expose This Service" → TeamxCoreManager spawns `teamx tunnel expose <name> --port <p> --mode <m>` (long-lived)
3. Status becomes "Running"; `tunnel list` shows the public port (frp mode)
4. Click "Close" → kill subprocess + `tunnel close <name>` RPC

**Forward a teammate's service (Consumer)**:
1. See teammates' tunnels in `tunnel list` (with provider, mode)
2. Click "Forward" → spawn `teamx tunnel forward <name> [--local-port]` (long-lived)
3. Show the local mapping address (defaults to the provider's target port)
4. Status "Running"; click again to stop

**Query/refresh**:
- `tunnel list` RPC → table refresh (polled every 2s)
- Long-lived WS processes managed by TeamxCoreManager (same as proxy); process alive = state running

### 6b.4 Relationship to proxy/tun0

| Feature | Shared with tunnels? |
|---|---|
| Underlying channel | All go through the teamx server (WS/RPC mTLS) — same `TEAMX_SERVER_URL` + certificates |
| session identity | tunnel commands need `--session`; the app uses one fixed GUI session key (e.g. `gui:<instance>`) |
| Dependencies | Server URL + mTLS must be configured first (teamx environment); the GUI should offer a "Connection Settings" entry |

### 6b.5 TeamxCoreManager Extensions (Tunnel Management)

```swift
// Tunnel entries (parsed from the tunnel list RPC)
struct TunnelEntry: Codable {
    let name: String
    let mode: String
    let port: Int          // frp: public port; local/proxy: 0
    let providerMemberID: String?
    let targetPort: Int?
    let lanIP: String?
}

final class TeamxCoreManager {
    // running expose/forward subprocesses (same management model as proxy)
    private var tunnels: [String: Process] = [:]   // name -> running expose/forward

    func listTunnels() -> [TunnelEntry]            // teamx tunnel list --json
    func expose(name: String, port: Int, mode: String)
    func forward(name: String, localPort: Int?)
    func close(name: String)                        // kill + tunnel close RPC
}
```

### 6b.6 Additional Milestone

| M | Content | Effort |
|---|---|---|
| M6 | Tunnel UI (expose/forward/list/close + TeamxCoreManager extensions) | 1.5 days |

## 7. Milestones

| M | Content | Effort |
|---|---|---|
| M1 | SPM project + AppDelegate menu bar + tray icon (logo) | 1 day |
| M2 | Control panel window (native controls, show status/start-stop) | 1 day |
| M3 | TeamxCoreManager (bundled teamx binary, proxy start/stop + logs) | 1 day |
| M4 | PrivilegedHelper (privileged tun0 start/stop, XPC + SMJobBless) | 2 days |
| M5 | RouteManager + exit-switching menu + icons/packaging/signing | 1 day |
| M6 | Tunnel UI (expose/forward/list/close) | 1.5 days |
| **Total** | | **≈ 7.5 days** |

## 8. Risks

| Risk | Mitigation |
|---|---|
| SMJobBless signing requirements (SMAuthorizedClients) | ad-hoc signing for development + consistency strings; documented |
| Helper version drift | Version comparison + check at launch |
| Non-sandboxed distribution restrictions | Follow mature solutions and distribute directly as DMG (not via MAS) |
| Rust binary platform | macOS only (currently arm64; x86_64 possible via fat binary or Rosetta) |
| Tunnel requires network mode | GUI provides "Connection Settings" (server URL + mTLS import); prompt when unconfigured |
