# Teamx 桌面 App（Swift/AppKit）设计文档

- 文档类型: design（实现蓝图，参考成熟 macOS 菜单栏 app 实现）
- 关联: `docs/09-design-tun0.md`（tun0 功能）、`docs/20-manual-tunnel-proxy-cli.md`
- 日期: 2026-08-23
- 参考: 成熟的 macOS 菜单栏 app 架构（NSStatusItem + SMAppService 特权助手）
- 状态: 待审阅（B 阶段），确认后实现（A 阶段）

## 1. 为什么用 Swift/AppKit 重写

当前 Rust egui/eframe 方案的问题：
1. **系统模态框（osascript 授权）与 winit 事件循环冲突** → 面板闪退/关闭
2. egui 非原生观感，与 macOS 风格割裂
3. tun0 特权启动没有干净方案（osascript nohup 各种问题）

成熟 macOS 菜单栏 app 的实现证明：**原生 AppKit + NSStatusItem + SMAppService 特权助手** 是 macOS 菜单栏 app 的标准且可靠的架构。这套架构无闪退、无权限问题、观感原生。

## 2. 总体架构

```
Teamx.app（Swift + AppKit，非沙盒，LSUIElement=true）
├── AppDelegate.swift            # NSStatusItem + 菜单 + 生命周期
├── Main.storyboard              # 状态栏菜单（约 12 项）+ 控制面板窗口
├── Controllers/
│   ├── ControlPanelController.swift  # 控制面板窗口（NSViewController）
│   └── TeamxWindowController.swift   # 泛型单窗口控制器（通用单窗口模式）
├── Managers/
│   ├── TeamxCoreManager.swift   # 管理 teamx CLI 子进程（Process）
│   ├── PrivilegedHelperManager.swift # tun0 特权操作（XPC helper）
│   └── RouteManager.swift       # 路由表读写（调 CLI）
├── PrivilegedHelper/            # 独立 root helper target（Swift）
│   ├── TeamxPrivilegedHelper.m  # NSXPCListener + 特权操作
│   └── TeamxPrivilegedProtocol.h
├── Resources/
│   └── teamx                    # 内置 Rust CLI 二进制（release，编译时拷贝）
├── Info.plist                   # LSUIElement=true, 空 entitlements（非沙盒）
└── Images.xcassets              # 图标（从 docs/logo 生成）
```

## 3. 核心组件设计

### 3.1 AppDelegate（菜单栏）

参考成熟 macOS 菜单栏 app 模式：

```swift
@main
class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    @IBOutlet var statusMenu: NSMenu!

    func applicationDidFinishLaunching(_ n: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let btn = statusItem.button {
            btn.image = NSImage(named: "tray")?.withSymbolConfiguration(.init(pointSize: 14, weight: .medium))
            btn.image?.isTemplate = true   // 适配深浅色
        }
        statusMenu.delegate = self
        statusItem.menu = statusMenu
        // 启动 teamx CLI 状态检测（tun0/proxy 是否在跑）
        TeamxCoreManager.shared.startMonitoring()
    }
}
```

**菜单**（storyboard 定义，action 指向 AppDelegate）：
```
状态栏图标
├─ Open Control Panel          → 打开控制面板窗口
├─ Start tun0 / Stop tun0      → 特权 helper 启停（enable/disable 动态）
├─ Start Proxy / Stop Proxy    → 起停 SOCKS5 proxy（Process，无需特权）
├─ ──separator──
├─ Switch Default Exit…        → 子菜单列出 egress（RouteManager 查询）
├─ Show Log                    → 打开日志窗口（或日志文件）
├─ ──separator──
└─ Quit
```

### 3.2 控制面板窗口（原生 AppKit）

不用 egui —— 用 **NSStackView + NSTextField + NSButton** 或 **SwiftUI NSHostingView**：

```
┌─ Teamx 控制面板 ────────────────────────────┐
│  ⚙️ Teamx                                 │
│                                             │
│  ┌─ tun0 虚拟网卡 ──────────── [运行中/已停止] ┐│
│  │  透明代理 · 需 root                       ││
│  │  [启动]  [停止]                           ││
│  └──────────────────────────────────────────┘│
│  ┌─ SOCKS5 代理 ────────────── [运行中/已停止] ┐│
│  │  本地端口 1080                            ││
│  │  [启动]  [停止]                           ││
│  └──────────────────────────────────────────┘│
│  ┌─ 默认出口 ────────────────────────────────┐│
│  │  egress                                ││
│  └──────────────────────────────────────────┘│
│  ┌─ 日志 ───────────────────────────────────┐│
│  │  [显示/隐藏]  [清空]                     ││
│  │  [滚动日志区域]                          ││
│  └──────────────────────────────────────────┘│
└─────────────────────────────────────────────┘
```

- 用 `NSViewController` + AutoLayout（或 SwiftUI 更简洁）
- 状态每 2s 刷新（`Timer`），读 `TeamxCoreManager` 缓存
- 日志区域：`NSTextView` 只读 + append

### 3.3 TeamxCoreManager（CLI 子进程管理）

```swift
final class TeamxCoreManager {
    static let shared = TeamxCoreManager()
    // 状态缓存（ObservableObject / Combine，供 UI 绑定）
    @Published var tun0Running: Bool = false
    @Published var proxyRunning: Bool = false
    @Published var logs: [String] = []

    func startTun0()        // → PrivilegedHelperManager.shared.startTun0()
    func stopTun0()         // → PrivilegedHelperManager.shared.stopTun0()
    func startProxy()       // Process: teamx proxy start --port 1080（捕获输出→logs）
    func stopProxy()        // Process kill
    func refreshStatus()    // 查询 tun0/proxy 进程状态（pgrep）
    private func teamxURL() -> URL  // Bundle.main.resourceURL/teamx
}
```

**内置 teamx 二进制**：
- Rust `cargo build --release` 产物拷到 `Resources/teamx`
- AppKit 用 `Process` 启动：`Process()` + `executableURL = teamxURL()` + 参数
- 捕获 stdout/stderr → 日志（`Pipe` + 读线程）

### 3.4 PrivilegedHelperManager（tun0 特权操作）

参考成熟 macOS 菜单栏 app 的 SMAppService/SMJobBless + XPC 模式（不是 osascript）：

**协议**（ObjC，桥接头引入）：
```objc
@protocol TeamxPrivilegedProtocol <NSObject>
- (void)startTun0WithEnv:(NSDictionary *)env reply:(void(^)(NSString *))reply;
- (void)stopTun0WithReply:(void(^)(NSString *))reply;
- (void)getTun0StatusWithReply:(void(^)(BOOL))reply;
- (void)getVersion:(void(^)(NSString *))reply;
@end
```

**Helper 端**（独立 target，root LaunchDaemon）：
- `NSXPCListener` mach service `io.flomesh.teamx.helper`
- `startTun0`：`NSTask` 以 root 跑 `teamx tun0 start`（env 传 mTLS 参数），detached
- `stopTun0`：kill tun0 进程
- 5s 无连接自动退出（参考通用实现）

**安装**：SMJobBless（现代）→ 失败降级 AppleScript 手动装（参考 legacy 降级方案）

**版本比对**：app 内嵌 helper bundle 版本 vs `/Library/PrivilegedHelperTools/` 版本

### 3.5 窗口管理（通用单窗口控制器）

```swift
final class TeamxWindowsRecorder {
    static let shared = ...
    var windowControllers: [NSWindowController] {
        didSet { /* 空→.accessory，非空→.regular */ }
    }
}
final class TeamxWindowController<T: NSViewController>: NSWindowController, NSWindowDelegate {
    static func create() -> NSWindowController { /* 复用/新建 + lastSize */ }
}
```

### 3.6 RouteManager（出口管理）

- 调 `teamx proxy routes list --json` 读默认出口 + 规则
- 菜单"Switch Default Exit"子菜单：列出可用 egress，选中 → `teamx proxy routes set-default <exit>`
- 控制面板显示当前默认出口

## 4. 构建与打包

```
# 1. 构建 Rust CLI
cargo build --release

# 2. 构建 Xcode 工程（xcodebuild）
xcodebuild -project Teamx.xcodeproj -scheme Teamx -configuration Release \
           -derivedDataPath build build

# 3. 拷贝 teamx 二进制到 .app Resources
cp target/release/teamx build/Teamx.app/Contents/Resources/teamx

# 4. 生成图标（从 docs/logo）
#    logo → AppIcon.icns（iconutil）+ tray icon

# 5. 签名（开发用 ad-hoc；发布用 Developer ID）
codesign --force --deep --sign - Teamx.app

# 6. 安装到 /Applications + LaunchAgent
```

## 5. Xcode 工程结构

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
    ├── Helper-Info.plist (嵌入 __TEXT __info_plist)
    └── Helper-Launchd.plist (嵌入 __TEXT __launchd_plist)
```

## 6. 权限模型

| 操作 | 方式 | 权限 |
|---|---|---|
| SOCKS5 proxy 启停 | Process（Resources/teamx） | 无需特权 |
| tun0 启停 | XPC → 特权 helper（root） | 首次装 helper 需管理员授权 |
| 路由表读写 | Process（本地 DB） | 无需特权 |
| 状态/日志 | Process / 读文件 | 无需特权 |

## 6b. Tunnel 配置与使用（app UI）

### 6b.1 能力清单（对应 CLI）

teamx tunnel 提供两类角色 + 三类查询：

| 角色 | 命令 | 生命周期 | 权限 |
|---|---|---|---|
| **Provider**（暴露本地服务） | `tunnel expose <name> --port <p> --mode local\|frp [--lan-ip]` | 长驻 WS 客户端 | 网络模式 mTLS |
| **Consumer**（转发队友服务） | `tunnel forward <name> [--local-port <p>]` | 长驻 WS 客户端 | 网络模式 mTLS |
| **查询** | `tunnel list` / `tunnel status <name>` / `tunnel close <name>` | 一次性 RPC | 网络模式 mTLS |

三种 expose 模式：
- `local`（默认）：服务器不绑端口，队友用 `forward` 本地映射（最安全）
- `frp`：服务器分配公网端口（9100-9999），队友直接 `tcp://<server>:<port>`
- `proxy`：SOCKS5 出口（已并入 proxy 功能，tunnel UI 不重复）

### 6b.2 UI 设计：控制面板加「隧道」区域

```
┌─ 隧道 ──────────────────────────────────────────┐
│  [暴露服务]  [转发服务]  [刷新]                  │  ← 顶部动作
│                                                 │
│  ── 我暴露的服务 (Provider) ──                   │
│  │ 名称        端口  模式  公网端口  状态  [关闭]  │  ← 表/列表
│  │ svc-web    8080  local  -      运行中  [×]   │
│  │ svc-frp    9090  frp    9103   运行中  [×]   │
│                                                 │
│  ── 队友暴露的服务 (Consumer) ──                  │
│  │ 名称        提供者   模式  本地映射  状态  [转发] │
│  │ db         exit-b  local  127.0.0.1:5432 运行中│
│                                                 │
│  ── 新建暴露 ──                                   │
│  │ 名称: [______]  端口: [____]  模式: [local▼]  │
│  │ [暴露此服务]                                   │
│  └──────────────────────────────────────────────┘
```

### 6b.3 交互流程

**暴露本地服务（Provider）**：
1. 用户填：名称 + 本地端口 + 模式（local/frp）
2. 点「暴露此服务」→ TeamxCoreManager spawn `teamx tunnel expose <name> --port <p> --mode <m>`（长驻）
3. 状态更新为"运行中"，`tunnel list` 展示公网端口（frp 模式）
4. 点「关闭」→ kill 子进程 + `tunnel close <name>` RPC

**转发队友服务（Consumer）**：
1. 从 `tunnel list` 看到队友暴露的隧道（含 provider、模式）
2. 点「转发」→ spawn `teamx tunnel forward <name> [--local-port]`（长驻）
3. 显示本地映射地址（默认 provider 目标端口）
4. 状态"运行中"，再次点击停止

**查询刷新**：
- `tunnel list` RPC → 表格刷新（2s 轮询）
- 长驻 WS 进程由 TeamxCoreManager 管理（同 proxy），进程存活 = 状态运行中

### 6b.4 与 proxy/tun0 的关系

| 功能 | 隧道共享？ |
|---|---|
| 底层通道 | 全部走 teamx server（WS/RPC mTLS）—— 同一个 `TEAMX_SERVER_URL` + 证书 |
| session 标识 | tunnel 命令需要 `--session`；app 用一个固定 GUI session key（如 `gui:<instance>`） |
| 依赖 | 需先配置 server URL + mTLS（teamx 环境），GUI 应有"连接设置"入口 |

### 6b.5 TeamxCoreManager 扩展（隧道管理）

```swift
// 隧道条目（从 tunnel list RPC 解析）
struct TunnelEntry: Codable {
    let name: String
    let mode: String
    let port: Int          // frp: 公网端口; local/proxy: 0
    let providerMemberID: String?
    let targetPort: Int?
    let lanIP: String?
}

final class TeamxCoreManager {
    // 已运行的 expose/forward 子进程（同 proxy 管理模式）
    private var tunnels: [String: Process] = [:]   // name -> running expose/forward

    func listTunnels() -> [TunnelEntry]            // teamx tunnel list --json
    func expose(name: String, port: Int, mode: String)
    func forward(name: String, localPort: Int?)
    func close(name: String)                        // kill + tunnel close RPC
}
```

### 6b.6 里程碑追加

| M | 内容 | 工作量 |
|---|---|---|
| M6 | 隧道 UI（暴露/转发/列表/关闭 + TeamxCoreManager 扩展） | 1.5 天 |

## 7. 里程碑

| M | 内容 | 工作量 |
|---|---|---|
| M1 | SPM 工程 + AppDelegate 菜单栏 + 托盘图标（logo） | 1 天 |
| M2 | 控制面板窗口（原生控件，显示状态/启停） | 1 天 |
| M3 | TeamxCoreManager（内置 teamx 二进制，proxy 启停 + 日志） | 1 天 |
| M4 | PrivilegedHelper（tun0 特权启停，XPC + SMJobBless） | 2 天 |
| M5 | RouteManager + 出口切换菜单 + 图标/打包/签名 | 1 天 |
| M6 | 隧道 UI（暴露/转发/列表/关闭） | 1.5 天 |
| **合计** | | **约 7.5 天** |

## 8. 风险

| 风险 | 缓解 |
|---|---|
| SMJobBless 签名要求（SMAuthorizedClients） | 开发用 ad-hoc 签名 + 一致性字符串；文档说明 |
| Helper 版本同步 | 版本比对 + 启动检查 |
| 非沙盒分发限制 | 参考成熟方案直接 DMG 分发（不走 MAS） |
| Rust 二进制平台 | 只支持 macOS（当前 arm64；x86_64 可 fat 或 Rosetta） |
| tunnel 需 network 模式 | GUI 提供"连接设置"（server URL + mTLS 导入），未配置时提示 |
