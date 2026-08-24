//
//  ControlPanelController.swift — native control panel window.
//
//  Layout follows the macOS settings-panel pattern: a vertical NSStackView of
//  NSBox "card" groups, two-column NSGridView rows (label left / control
//  right), NSButton checkboxes, and system color hierarchy (labelColor titles,
//  secondaryLabelColor footnotes). Dark/light mode aware.
//

import AppKit

final class ControlPanelController: NSViewController {
    // Status
    private var connLabel = NSTextField(labelWithString: "连接: —")
    private var memberTable = DraggableTable(columns: ["成员", "角色", "IP", "状态", "ping", "↑", "↓"], widths: [120, 90, 120, 60, 60, 70, 70], id: "members", defaultHeight: 90)
    private var tun0Status = statusBadge()
    private var proxyStatus = statusBadge()
    private var exitPop = NSPopUpButton()
    private var memberPop = NSPopUpButton()
    private var logView = NSTextView()
    private var tunnelTable = DraggableTable(columns: ["", "名称", "模式", "公网端口", "提供者"], widths: [30, 140, 70, 80, 90], id: "tunnels", defaultHeight: 80)
    private var routesTable = DraggableTable(columns: ["规则", "出口"], widths: [220, 100], id: "routes", defaultHeight: 80)

    // Tab / card switching
    private var segmented = NSSegmentedControl()
    private var contentContainer = NSView()
    private var cards: [NSView] = []

    // "路由表" card
    private var routeDefaultLabel = NSTextField(labelWithString: "-")
    private var traceInput = NSTextField()
    private var traceOutput = NSTextView()

    // "DNS" card
    private var dnsLabel = NSTextField(labelWithString: "-")
    private var dnsInput = NSTextField()
    private var dnsOutput = NSTextView()

    private var refreshTimer: Timer?

    private static let tabTitles = ["连接状态", "虚拟网卡", "SOCKS5 代理", "默认出口", "隧道", "tun0 路由规则", "路由表", "DNS"]

    override func loadView() {
        view = NSView(frame: NSRect(x: 0, y: 0, width: 880, height: 720))
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor

        // Root vertical stack with explicit side margins.
        let root = NSStackView()
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 10
        root.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(root)
        NSLayoutConstraint.activate([
            root.topAnchor.constraint(equalTo: view.topAnchor, constant: 20),
            root.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 20),
            root.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -20),
            root.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -20),
        ])

        // --- header ---
        let title = NSTextField(labelWithString: "Teamx")
        title.font = .boldSystemFont(ofSize: 22)
        title.textColor = .labelColor
        root.addArrangedSubview(title)
        let sub = NSTextField(labelWithString: "tun0 透明代理 · SOCKS5 代理 · 隧道")
        sub.font = .systemFont(ofSize: 12)
        sub.textColor = .secondaryLabelColor
        root.addArrangedSubview(sub)
        root.setCustomSpacing(8, after: sub)

        // --- member selector ---
        memberPop.bezelStyle = .rounded
        memberPop.font = .systemFont(ofSize: 12)
        memberPop.target = self
        memberPop.action = #selector(memberChanged(_:))
        let memberRow = NSStackView()
        memberRow.orientation = .horizontal
        memberRow.spacing = 8
        let ml = NSTextField(labelWithString: "本地成员")
        ml.font = .systemFont(ofSize: 12)
        ml.textColor = .secondaryLabelColor
        memberRow.addArrangedSubview(ml)
        memberRow.addArrangedSubview(memberPop)
        root.addArrangedSubview(memberRow)
        root.setCustomSpacing(12, after: memberRow)

        // --- tab bar (card switch) ---
        segmented.segmentCount = Self.tabTitles.count
        for (i, t) in Self.tabTitles.enumerated() {
            segmented.setLabel(t, forSegment: i)
        }
        segmented.selectedSegment = 0
        segmented.target = self
        segmented.action = #selector(tabChanged(_:))
        segmented.translatesAutoresizingMaskIntoConstraints = false
        root.addArrangedSubview(segmented)
        root.setCustomSpacing(12, after: segmented)

        // --- content container (shows the selected card) ---
        contentContainer.translatesAutoresizingMaskIntoConstraints = false
        root.addArrangedSubview(contentContainer)
        contentContainer.widthAnchor.constraint(equalTo: root.widthAnchor).isActive = true

        // Build all 8 cards.
        cards = buildCards()

        // --- log section (fixed at the bottom, terminal-style) ---
        let logActions = NSStackView()
        logActions.orientation = .horizontal
        logActions.spacing = 8
        logActions.addArrangedSubview(button("拷贝", action: { [weak self] in self?.copyLog() }))
        logActions.addArrangedSubview(button("清空", action: { [weak self] in
            LogBuffer.shared.clear()
            self?.refreshLog()
        }))
        let logScroll = makeTermScroll(logView)
        logScroll.translatesAutoresizingMaskIntoConstraints = false
        logScroll.heightAnchor.constraint(equalToConstant: 200).isActive = true
        root.addArrangedSubview(makeCard([
            (label: "日志", view: logActions),
            (label: "", view: logScroll),
        ]))
        // Log card stretches to root width.
        if let last = root.arrangedSubviews.last {
            last.translatesAutoresizingMaskIntoConstraints = false
            last.widthAnchor.constraint(equalTo: root.widthAnchor).isActive = true
        }

        showCard(0)

        refreshTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    override func viewWillAppear() {
        super.viewWillAppear()
        refreshMemberPop()
        refresh()
        refreshTunnels()
        refreshRoutes()
        refreshExitPop()
        refreshRouteTable()
        refreshDNS()
    }

    override func viewDidDisappear() {
        super.viewDidDisappear()
        refreshTimer?.invalidate()
        refreshTimer = nil
    }

    // MARK: - Tab / card switching

    @objc private func tabChanged(_ sender: NSSegmentedControl) {
        showCard(sender.selectedSegment)
    }

    private func showCard(_ index: Int) {
        contentContainer.subviews.forEach { $0.removeFromSuperview() }
        guard index >= 0 && index < cards.count else { return }
        let card = cards[index]
        card.translatesAutoresizingMaskIntoConstraints = false
        contentContainer.addSubview(card)
        NSLayoutConstraint.activate([
            card.topAnchor.constraint(equalTo: contentContainer.topAnchor),
            card.leadingAnchor.constraint(equalTo: contentContainer.leadingAnchor),
            card.trailingAnchor.constraint(equalTo: contentContainer.trailingAnchor),
            card.bottomAnchor.constraint(equalTo: contentContainer.bottomAnchor),
        ])
    }

    /// Build the eight card views (one per tab).
    private func buildCards() -> [NSView] {
        var out: [NSView] = []

        // 1. 连接状态
        connLabel.font = .systemFont(ofSize: 12)
        out.append(makeCard([
            (label: "连接状态", view: connLabel),
            (label: "", view: memberTable),
        ]))

        // 2. 虚拟网卡
        out.append(makeActionCard(
            title: "tun0 虚拟网卡",
            desc: "",
            status: tun0Status,
            startTitle: "启动",
            startColor: .systemGreen,
            start: { Privileged.startTun0() },
            stopTitle: "停止",
            stopColor: .systemRed,
            stop: { Privileged.stopTun0() },
            restartTitle: "重启",
            restart: { Privileged.restartTun0() }
        ))

        // 3. SOCKS5 代理
        out.append(makeActionCard(
            title: "SOCKS5 代理",
            desc: "",
            status: proxyStatus,
            startTitle: "启动",
            startColor: .systemGreen,
            start: { TeamxCore.shared.startProxy() },
            stopTitle: "停止",
            stopColor: .systemRed,
            stop: { TeamxCore.shared.stopProxy() }
        ))

        // 4. 默认出口
        exitPop.bezelStyle = .rounded
        exitPop.font = .systemFont(ofSize: 12)
        exitPop.target = self
        exitPop.action = #selector(exitChanged(_:))
        out.append(makeCard([
            (label: "默认出口", view: exitPop)
        ], footnotes: ["未匹配规则的流量走默认出口"]))

        // 5. 隧道
        let tunnelActions = NSStackView()
        tunnelActions.orientation = .horizontal
        tunnelActions.spacing = 8
        let tunnelHint = NSTextField(labelWithString: "配置/管理请用 CLI 或 opencode plugin")
        tunnelHint.font = .systemFont(ofSize: 11)
        tunnelHint.textColor = .secondaryLabelColor
        tunnelActions.addArrangedSubview(tunnelHint)
        tunnelActions.addArrangedSubview(button("刷新", action: { [weak self] in self?.refreshTunnels() }))
        out.append(makeCard([
            (label: "隧道", view: tunnelActions),
            (label: "", view: tunnelTable),
        ]))

        // 6. tun0 路由规则
        let routesRefresh = button("刷新", action: { [weak self] in self?.refreshRoutes() })
        out.append(makeCard([
            (label: "tun0 路由规则", view: routesRefresh),
            (label: "", view: routesTable),
        ], footnotes: ["按目标域名/IP 选择出口；未匹配走默认出口"]))

        // 7. 路由表（默认路由 + trace route）
        out.append(buildRouteTableCard())

        // 8. DNS（默认 DNS + 域名解析）
        out.append(buildDNSCard())

        return out
    }

    /// "路由表" card: default route + traceroute query.
    private func buildRouteTableCard() -> NSView {
        routeDefaultLabel.font = .systemFont(ofSize: 12)
        let defaultRow = NSStackView()
        defaultRow.orientation = .horizontal
        defaultRow.spacing = 8
        let dl = NSTextField(labelWithString: "默认路由")
        dl.font = .systemFont(ofSize: 13)
        dl.textColor = .labelColor
        defaultRow.addArrangedSubview(dl)
        defaultRow.addArrangedSubview(routeDefaultLabel)

        traceInput.placeholderString = "输入 IP 或域名，如 142.250.73.68"
        traceInput.font = .systemFont(ofSize: 12)
        let traceActions = NSStackView()
        traceActions.orientation = .horizontal
        traceActions.spacing = 8
        let tl = NSTextField(labelWithString: "Trace Route")
        tl.font = .systemFont(ofSize: 13)
        tl.textColor = .labelColor
        traceActions.addArrangedSubview(tl)
        traceActions.addArrangedSubview(traceInput)
        traceInput.widthAnchor.constraint(equalToConstant: 220).isActive = true
        traceActions.addArrangedSubview(button("查询", action: { [weak self] in self?.traceRoute() }))

        let scroll = makeTermScroll(traceOutput)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.heightAnchor.constraint(equalToConstant: 120).isActive = true

        return makeCard([
            (label: "", view: defaultRow),
            (label: "", view: traceActions),
            (label: "", view: scroll),
        ], footnotes: ["traceroute 到指定 IP/域名，展示每一跳的延迟"])
    }

    /// "DNS" card: default DNS + domain resolution query.
    private func buildDNSCard() -> NSView {
        dnsLabel.font = .systemFont(ofSize: 12)
        let dnsRow = NSStackView()
        dnsRow.orientation = .horizontal
        dnsRow.spacing = 8
        let dl = NSTextField(labelWithString: "默认 DNS")
        dl.font = .systemFont(ofSize: 13)
        dl.textColor = .labelColor
        dnsRow.addArrangedSubview(dl)
        dnsRow.addArrangedSubview(dnsLabel)

        dnsInput.placeholderString = "输入域名，如 www.google.com"
        dnsInput.font = .systemFont(ofSize: 12)
        let dnsActions = NSStackView()
        dnsActions.orientation = .horizontal
        dnsActions.spacing = 8
        let ql = NSTextField(labelWithString: "域名解析")
        ql.font = .systemFont(ofSize: 13)
        ql.textColor = .labelColor
        dnsActions.addArrangedSubview(ql)
        dnsActions.addArrangedSubview(dnsInput)
        dnsInput.widthAnchor.constraint(equalToConstant: 220).isActive = true
        dnsActions.addArrangedSubview(button("查询", action: { [weak self] in self?.resolveDNS() }))

        let scroll = makeTermScroll(dnsOutput)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.heightAnchor.constraint(equalToConstant: 120).isActive = true

        return makeCard([
            (label: "", view: dnsRow),
            (label: "", view: dnsActions),
            (label: "", view: scroll),
        ], footnotes: ["解析结果经 teamx 出口（无污染）返回"])
    }

    /// Wrap a text view in a terminal-style scroll view (monospaced, dark).
    private func makeTermScroll(_ tv: NSTextView) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        tv.isEditable = false
        tv.isRichText = false
        tv.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        tv.backgroundColor = NSColor.textBackgroundColor
        scroll.documentView = tv
        return scroll
    }

    // MARK: - Refresh

    /// Populate the member selector from local_members.
    private func refreshMemberPop() {
        let members = TeamxCore.shared.listLocalMembers()
        memberPop.removeAllItems()
        for m in members {
            memberPop.addItem(withTitle: "\(m.name)  ·  \(shortHost(m.serverURL))")
        }
        // keep selection
        if let cur = TeamxCore.shared.currentMember {
            memberPop.selectItem(at: members.firstIndex { $0.key == cur.key } ?? 0)
        } else if !members.isEmpty {
            TeamxCore.shared.currentMember = members[0]
            memberPop.selectItem(at: 0)
        }
    }

    @objc private func memberChanged(_ sender: NSPopUpButton) {
        let members = TeamxCore.shared.listLocalMembers()
        let idx = sender.indexOfSelectedItem
        if idx >= 0 && idx < members.count {
            TeamxCore.shared.currentMember = members[idx]
            refresh()
            refreshTunnels()
            refreshRoutes()
        }
    }

    private func shortHost(_ url: String) -> String {
        url.replacingOccurrences(of: "https://", with: "")
            .replacingOccurrences(of: "http://", with: "")
    }

    private func refresh() {
        let core = TeamxCore.shared
        tun0Status.stringValue = core.tun0Running ? "运行中" : "已停止"
        tun0Status.textColor = core.tun0Running ? .systemGreen : .secondaryLabelColor
        proxyStatus.stringValue = core.proxyRunning ? "运行中" : "已停止"
        proxyStatus.textColor = core.proxyRunning ? .systemGreen : .secondaryLabelColor
        exitPop.selectItem(withTitle: core.defaultExit())
        refreshConnection()
        core.pumpTun0Log()
        refreshLog()
    }

    /// Server address + online status + member presence with metrics.
    /// The network round-trips (mTLS curl, up to ~5-8 s each) run on a
    /// background queue; only the UI updates hop back to the main thread.
    private func refreshConnection() {
        // Use the active local member's server + letter (if any).
        let member = TeamxCore.shared.currentMember
        let material = TeamxServer.currentMaterial(letterID: member?.letterID, serverURL: member?.serverURL)
        if let info = TeamxServer.serverInfo(material) {
            connLabel.stringValue = "\(info.host):\(info.port)  …"
        } else {
            connLabel.stringValue = "未配置 server（无邀请函）"
            connLabel.textColor = .secondaryLabelColor
        }

        DispatchQueue.global(qos: .utility).async { [weak self] in
            let online = TeamxServer.serverOnline(material)
            let members = TeamxServer.teamMembers(material)
            let metrics = TeamxServer.memberMetrics(material)
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                if let info = TeamxServer.serverInfo(material) {
                    self.connLabel.stringValue = "\(info.host):\(info.port)  \(online ? "● 在线" : "○ 离线")"
                } else {
                    self.connLabel.stringValue = "未配置 server（无邀请函）"
                }
                self.connLabel.textColor = online ? .systemGreen : .systemOrange
                if members.isEmpty {
                    self.memberTable.setRows([["（无成员信息）", "", "", "", "", "", ""]])
                } else {
                    self.memberTable.setRows(members.map { m in
                        let isOnline = m.online || (metrics[m.id]?.online ?? false)
                        let icon = isOnline ? "● 在线" : "○ 离线"
                        let ip = m.ip ?? "-"
                        let met = metrics[m.id]
                        let ping = met?.pingMs.map { String(format: "%.0f", $0) } ?? "-"
                        let rx = met.map { Self.formatBps($0.rxBps) } ?? "-"
                        let tx = met.map { Self.formatBps($0.txBps) } ?? "-"
                        return [m.name, m.role, ip, icon, ping, rx, tx]
                    })
                }
            }
        }
    }

    private static func formatBps(_ bps: Int) -> String {
        if bps >= 1_048_576 { return String(format: "%.1f MB", Double(bps) / 1_048_576) }
        if bps >= 1024 { return String(format: "%.1f KB", Double(bps) / 1024) }
        return "\(bps) B"
    }

    private func refreshLog() {
        logView.string = LogBuffer.shared.snapshot().joined(separator: "\n")
        logView.scrollToEndOfDocument(nil)
    }

    /// Copy the current log buffer to the system clipboard.
    private func copyLog() {
        let text = LogBuffer.shared.snapshot().joined(separator: "\n")
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(text, forType: .string)
        LogBuffer.shared.push("[log] 日志已拷贝到剪贴板")
        refreshLog()
    }

    private func refreshExitPop() {
        let core = TeamxCore.shared
        let current = core.defaultExit()
        let exits = core.listExits()
        let titles = exits.isEmpty ? ["(无可用出口)"] : exits
        if exitPop.itemTitles != titles {
            exitPop.removeAllItems()
            exitPop.addItems(withTitles: titles)
        }
        exitPop.selectItem(withTitle: current)
    }

    @objc private func exitChanged(_ sender: NSPopUpButton) {
        if let t = sender.titleOfSelectedItem, !t.isEmpty, t != "(无可用出口)" {
            TeamxCore.shared.setDefaultExit(t)
        }
    }

    private func refreshTunnels() {
        let entries = TeamxCore.shared.listTunnels()
        if entries.isEmpty {
            tunnelTable.setRows([["", "（无活动隧道）", "", "", ""]])
        } else {
            tunnelTable.setRows(entries.map { e in
                let extra = e.mode == "frp" ? "\(e.port)" : "-"
                let provider = e.providerMemberID.map { String($0.prefix(8)) } ?? "?"
                let mine = TeamxCore.shared.tunnelWorkerRunning(e.name)
                let status = mine ? "●" : "○"
                return [status, e.name, e.mode, extra, provider]
            })
        }
    }

    /// tun0 路由规则（proxy routes list）: 规则 → 出口。
    private func refreshRoutes() {
        let r = TeamxCore.shared.run(args: ["proxy", "routes", "list", "--json"])
        guard let data = r.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            routesTable.setRows([["（无路由数据）", ""]])
            return
        }
        let defaultExit = obj["default"] as? String ?? "-"
        let rules = obj["rules"] as? [[String: Any]] ?? []
        var rows = rules.map { rule in
            let match = rule["match"] as? String ?? "?"
            let exit = rule["exit"] as? String ?? "?"
            return [match, exit]
        }
        rows.append(["default", defaultExit])
        routesTable.setRows(rows.isEmpty ? [["（无路由规则）", ""]] : rows)
    }

    /// "路由表" card: show the default route.
    private func refreshRouteTable() {
        routeDefaultLabel.stringValue = TeamxCore.shared.defaultExit()
    }

    /// Run `traceroute` to the given host and show the hops.
    private func traceRoute() {
        let host = traceInput.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !host.isEmpty else { return }
        traceOutput.string = "traceroute 到 \(host) …\n"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/sbin/traceroute")
        p.arguments = ["-q", "1", "-m", "15", host]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        do {
            try p.run()
        } catch {
            traceOutput.string = "无法运行 traceroute: \(error.localizedDescription)"
            return
        }
        // Bounded wait so the UI never freezes on a slow/blocked trace.
        let deadline = DispatchTime.now() + .seconds(30)
        while p.isRunning && DispatchTime.now() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        if p.isRunning { p.terminate() }
        let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        traceOutput.string = out.isEmpty ? "（无输出）" : out
    }

    /// "DNS" card: show the default system DNS servers.
    private func refreshDNS() {
        let r = TeamxCore.shared.run(args: ["dns", "list"])
        if !r.stdout.isEmpty {
            dnsLabel.stringValue = r.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
        } else {
            dnsLabel.stringValue = "-"
        }
    }

    /// Resolve a domain (via the teamx exit, uncensored) and show the result.
    private func resolveDNS() {
        let domain = dnsInput.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !domain.isEmpty else { return }
        dnsOutput.string = "解析 \(domain) …\n"
        let r = TeamxCore.shared.run(args: ["dns", "resolve", domain])
        dnsOutput.string = r.stdout.isEmpty ? (r.stderr.isEmpty ? "（无结果）" : r.stderr) : r.stdout
    }

    // MARK: - Card builders

    /// A bordered NSBox card containing one row (label + view).
    private func makeCard(_ rows: [(label: String, view: NSView)],
                          footnotes: [String] = []) -> NSView {
        // Modern card: a rounded-corner view with a subtle fill + border.
        let card = NSView()
        card.wantsLayer = true
        card.layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
        card.layer?.cornerRadius = 8
        card.layer?.borderWidth = 1
        card.layer?.borderColor = NSColor.separatorColor.cgColor
        card.translatesAutoresizingMaskIntoConstraints = false

        let content = NSStackView()
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 8
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: card.topAnchor, constant: 10),
            content.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 12),
            content.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -12),
            content.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -10),
        ])

        for (label, view) in rows {
            let row = NSStackView()
            row.orientation = .horizontal
            row.spacing = 8
            if !label.isEmpty {
                let l = NSTextField(labelWithString: label)
                l.font = .systemFont(ofSize: 13)
                l.textColor = .labelColor
                row.addArrangedSubview(l)
            }
            row.addArrangedSubview(view)
            content.addArrangedSubview(row)
        }
        for fn in footnotes {
            let f = NSTextField(labelWithString: fn)
            f.font = .systemFont(ofSize: 11)
            f.textColor = .secondaryLabelColor
            content.addArrangedSubview(f)
        }
        // Content fills the card width (so cards stretch on window resize).
        content.widthAnchor.constraint(equalTo: card.widthAnchor, constant: -24).isActive = true
        return card
    }

    /// An action card: title + status badge + start/stop (and optional restart) buttons.
    private func makeActionCard(title: String, desc: String, status: NSTextField,
                                startTitle: String, startColor: NSColor, start: @escaping () -> Void,
                                stopTitle: String, stopColor: NSColor, stop: @escaping () -> Void,
                                restartTitle: String? = nil, restart: (() -> Void)? = nil) -> NSView {
        let titleRow = NSStackView()
        titleRow.orientation = .horizontal
        titleRow.spacing = 8
        let t = NSTextField(labelWithString: title)
        t.font = .boldSystemFont(ofSize: 13)
        t.textColor = .labelColor
        titleRow.addArrangedSubview(t)
        titleRow.addArrangedSubview(status)

        let buttons = NSStackView()
        buttons.orientation = .horizontal
        buttons.spacing = 8
        buttons.addArrangedSubview(button(startTitle, action: start, color: startColor))
        if let restartTitle, let restart {
            buttons.addArrangedSubview(button(restartTitle, action: restart, color: .systemOrange))
        }
        buttons.addArrangedSubview(button(stopTitle, action: stop, color: stopColor))

        return makeCard([
            (label: "", view: titleRow),
            (label: "", view: buttons),
        ], footnotes: desc.isEmpty ? [] : [desc])
    }

    private func button(_ title: String, action: @escaping () -> Void, color: NSColor? = nil) -> NSButton {
        let b = NSButton(title: title, target: nil, action: nil)
        b.bezelStyle = .rounded
        b.controlSize = .regular
        b.contentTintColor = color ?? .controlAccentColor
        b.target = self
        b.action = #selector(buttonAction(_:))
        objc_setAssociatedObject(b, &buttonClosureKey, ButtonAction(action), .OBJC_ASSOCIATION_RETAIN)
        return b
    }

    @objc private func buttonAction(_ sender: NSButton) {
        if let ba = objc_getAssociatedObject(sender, &buttonClosureKey) as? ButtonAction {
            ba.action()
        }
    }

    static func statusBadge() -> NSTextField {
        let l = NSTextField(labelWithString: "已停止")
        l.font = .systemFont(ofSize: 12, weight: .semibold)
        l.textColor = .secondaryLabelColor
        return l
    }
}

private var buttonClosureKey: UInt8 = 0
private final class ButtonAction {
    let action: () -> Void
    init(_ action: @escaping () -> Void) { self.action = action }
}
