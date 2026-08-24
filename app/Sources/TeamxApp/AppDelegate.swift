//
//  AppDelegate.swift — menu bar (NSStatusItem) + menu + window management.
//

import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate, NSWindowDelegate {
    private var statusItem: NSStatusItem!
    private var panelController: NSWindowController?
    private var refreshTimer: Timer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildStatusItem()
        buildMenu()
        // Kick off a background refresh so the exit menu is populated without
        // ever blocking the main thread (server round-trips can take seconds).
        TeamxCore.shared.refreshExitCache()
        startMonitoring()
    }

    // MARK: - Status item

    private func buildStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem.button {
            // Prefer a compact "Tx" letter badge (crisp at any scale, adapts
            // to light/dark via isTemplate); fall back to the logo PNG.
            if let letter = TeamxIcons.statusBarImage() {
                button.image = letter
            } else if let logo = NSImage(named: "tray") {
                let icon = logo
                icon.isTemplate = true
                button.image = icon
            }
            button.toolTip = "Teamx"
        }
    }

    // MARK: - Menu

    private func buildMenu() {
        let menu = NSMenu()
        menu.delegate = self

        menu.addItem(item("打开控制面板", #selector(openPanel)))
        menu.addItem(.separator())

        let tun0 = NSMenuItem(title: "启动 tun0", action: #selector(startTun0), keyEquivalent: "")
        tun0.target = self
        menu.addItem(tun0)
        let tun0Stop = NSMenuItem(title: "停止 tun0", action: #selector(stopTun0), keyEquivalent: "")
        tun0Stop.target = self
        menu.addItem(tun0Stop)

        menu.addItem(.separator())

        let proxy = NSMenuItem(title: "启动 SOCKS5 代理", action: #selector(startProxy), keyEquivalent: "")
        proxy.target = self
        menu.addItem(proxy)
        let proxyStop = NSMenuItem(title: "停止 SOCKS5 代理", action: #selector(stopProxy), keyEquivalent: "")
        proxyStop.target = self
        menu.addItem(proxyStop)

        menu.addItem(.separator())

        // Default exit submenu (populated dynamically in menuNeedsUpdate)
        let exitItem = NSMenuItem(title: "默认出口: —", action: nil, keyEquivalent: "")
        exitItem.submenu = NSMenu()
        menu.addItem(exitItem)

        menu.addItem(.separator())
        menu.addItem(item("退出", #selector(quit)))

        statusItem.menu = menu
    }

    private func item(_ title: String, _ action: Selector) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: "")
        i.target = self
        return i
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        // update menu item enabled/state based on running status
        let core = TeamxCore.shared
        for item in menu.items {
            switch item.title {
            case "启动 tun0":
                item.isEnabled = !core.tun0Running
                item.state = core.tun0Running ? .on : .off
            case "停止 tun0":
                item.isEnabled = core.tun0Running
            case "启动 SOCKS5 代理":
                item.isEnabled = !core.proxyRunning
                item.state = core.proxyRunning ? .on : .off
            case "停止 SOCKS5 代理":
                item.isEnabled = core.proxyRunning
            default: break
            }
        }
        refreshExitMenu(menu)
    }

    /// Populate the "默认出口" submenu with the current default + available exits.
    private func refreshExitMenu(_ menu: NSMenu) {
        guard let exitItem = menu.items.first(where: { $0.title.hasPrefix("默认出口") }),
              let sub = exitItem.submenu else { return }

        let core = TeamxCore.shared
        let current = core.defaultExit()
        exitItem.title = "默认出口: \(current)"

        sub.removeAllItems()
        // current default (checked)
        let cur = NSMenuItem(title: "当前: \(current)", action: nil, keyEquivalent: "")
        sub.addItem(cur)
        sub.addItem(.separator())

        let exits = core.listExits()
        if exits.isEmpty {
            let none = NSMenuItem(title: "（无可用出口 — 成员需先运行 proxy exit）", action: nil, keyEquivalent: "")
            none.isEnabled = false
            sub.addItem(none)
        } else {
            for name in exits {
                let it = NSMenuItem(title: name, action: #selector(setExit(_:)), keyEquivalent: "")
                it.target = self
                it.state = (name == current) ? .on : .off
                sub.addItem(it)
            }
        }
    }

    @objc private func setExit(_ sender: NSMenuItem) {
        TeamxCore.shared.setDefaultExit(sender.title)
        // refresh the menu to reflect the new selection
        if let menu = statusItem.menu {
            refreshExitMenu(menu)
        }
    }

    // MARK: - Monitoring

    private func startMonitoring() {
        // Periodically refresh the exit-menu cache in the background. The menu
        // itself only reads cached values (defaultExit/listExits are non-blocking),
        // so the UI and keyboard stay responsive even if the server is slow.
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { _ in
            TeamxCore.shared.refreshExitCache()
        }
    }

    // MARK: - Actions

    @objc private func openPanel() {
        // With a visible window, switch to regular activation (Dock icon) so
        // system dialogs (e.g. the sudo authorization prompt) do not tear the
        // panel window down — the known accessory-app pitfall.
        NSApp.setActivationPolicy(.regular)
        if let wc = panelController {
            wc.showWindow(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let vc = ControlPanelController()
        let win = NSWindow(contentViewController: vc)
        win.title = "Teamx 控制面板"
        win.styleMask = [.titled, .closable, .resizable, .miniaturizable]
        win.isReleasedWhenClosed = false
        win.delegate = self
        win.setContentSize(NSSize(width: 840, height: 660))
        win.center()
        let wc = NSWindowController(window: win)
        panelController = wc
        wc.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        // Panel closed — back to pure menu-bar (accessory) mode.
        NSApp.setActivationPolicy(.accessory)
    }

    @objc private func startTun0() { Privileged.startTun0() }
    @objc private func stopTun0() { Privileged.stopTun0() }
    @objc private func startProxy() { TeamxCore.shared.startProxy() }
    @objc private func stopProxy() { TeamxCore.shared.stopProxy() }

    @objc private func quit() {
        TeamxCore.shared.stopProxy()
        // Unload the LaunchAgent (KeepAlive would otherwise relaunch us).
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        p.arguments = ["remove", "io.flomesh.teamx"]
        try? p.run()
        p.waitUntilExit()
        NSApp.terminate(nil)
    }
}
