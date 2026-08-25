//
//  TeamxCore.swift — manages the bundled `teamx` CLI binary.
//
//  All operations (proxy, tunnel, routes, status) go through the Rust CLI as
//  child processes. Long-lived workers (proxy start, tunnel expose/forward)
//  are spawned and their output is captured into a shared log buffer.
//

import Foundation

/// A shared ring buffer of log lines (thread-safe).
final class LogBuffer {
    static let shared = LogBuffer()
    private var lines: [String] = []
    private let lock = NSLock()
    private let cap = 500

    private init() {}

    func push(_ line: String) {
        lock.lock()
        if lines.count >= cap { lines.removeFirst() }
        lines.append(line)
        lock.unlock()
    }

    func snapshot() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return lines
    }

    func clear() {
        lock.lock()
        lines.removeAll()
        lock.unlock()
    }
}

/// A managed long-lived worker process (proxy / tunnel expose / forward).
final class WorkerProcess {
    let label: String
    private var process: Process?
    private let lock = NSLock()

    init(label: String) {
        self.label = label
    }

    var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return process.map { $0.isRunning } ?? false
    }

    /// Spawn `teamx <args>`, capturing stdout/stderr into the log.
    @discardableResult
    func spawn(args: [String], env: [String: String] = [:]) -> Bool {
        kill()
        guard let teamx = TeamxCore.teamxURL() else {
            LogBuffer.shared.push("[\(label)] teamx 二进制未找到")
            return false
        }
        let p = Process()
        p.executableURL = teamx
        p.arguments = args
        var fullEnv = ProcessInfo.processInfo.environment
        for (k, v) in env { fullEnv[k] = v }
        p.environment = fullEnv

        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        pipe.fileHandleForReading.readabilityHandler = { [label] handle in
            let data = handle.availableData
            if let s = String(data: data, encoding: .utf8), !s.isEmpty {
                for line in s.split(separator: "\n") {
                    LogBuffer.shared.push("[\(label)] \(line)")
                }
            }
        }

        do {
            try p.run()
        } catch {
            LogBuffer.shared.push("[\(label)] 启动失败: \(error.localizedDescription)")
            return false
        }
        lock.lock()
        process = p
        lock.unlock()
        LogBuffer.shared.push("[\(label)] 已启动: teamx \(args.joined(separator: " "))")
        return true
    }

    func kill() {
        lock.lock()
        let p = process
        process = nil
        lock.unlock()
        if let p, p.isRunning {
            p.terminate()
            LogBuffer.shared.push("[\(label)] 已停止")
        }
    }
}

/// Central manager: bundled teamx binary, proxy + tunnel workers, status.
final class TeamxCore {
    static let shared = TeamxCore()

    let proxy = WorkerProcess(label: "proxy")
    /// name -> worker for running tunnel expose/forward
    private var tunnels: [String: WorkerProcess] = [:]
    private let tunnelsLock = NSLock()

    // Exit-menu cache: `tunnel list` and route lookup hit the server / DB and
    // must NOT run synchronously on the main thread (they can block for seconds
    // and freeze the menu bar + keyboard). We refresh them on a background
    // queue and serve the menu from these cached values.
    private var cachedExits: [String] = []
    private var cachedDefaultExit: String = "(none)"
    private let exitCacheLock = NSLock()

    private init() {}

    /// Path to the bundled teamx binary (inside the .app Resources, or next to
    /// the executable during development).
    static func teamxURL() -> URL? {
        let bundle = Bundle.main.resourceURL?.appendingPathComponent("teamx")
        if FileManager.default.isExecutableFile(atPath: bundle?.path ?? "") {
            return bundle
        }
        // Development: ../target/release/teamx relative to the package.
        let dev = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("../target/release/teamx")
        if FileManager.default.isExecutableFile(atPath: dev.path) {
            return dev
        }
        return nil
    }

    // MARK: - Generic CLI run (one-shot)

    @discardableResult
    func run(args: [String], env: [String: String] = [:]) -> (status: Int32, stdout: String, stderr: String) {
        guard let teamx = TeamxCore.teamxURL() else {
            LogBuffer.shared.push("[core] teamx 二进制未找到")
            return (-1, "", "no binary")
        }
        let p = Process()
        p.executableURL = teamx
        p.arguments = args
        var fullEnv = ProcessInfo.processInfo.environment
        for (k, v) in env { fullEnv[k] = v }
        p.environment = fullEnv

        let out = Pipe()
        let err = Pipe()
        p.standardOutput = out
        p.standardError = err
        do {
            try p.run()
            // Read both pipes to EOF *before* waiting for exit: if the child
            // fills the 64KB pipe buffer while we're in waitUntilExit, it would
            // block forever (classic pipe deadlock).
            let outData = out.fileHandleForReading.readDataToEndOfFile()
            let errData = err.fileHandleForReading.readDataToEndOfFile()
            p.waitUntilExit()
            let so = String(data: outData, encoding: .utf8) ?? ""
            let se = String(data: errData, encoding: .utf8) ?? ""
            return (p.terminationStatus, so, se)
        } catch {
            LogBuffer.shared.push("[core] \(error.localizedDescription)")
            return (-1, "", error.localizedDescription)
        }
    }

    // MARK: - Proxy

    /// The active local member (config from local_members).
    var currentMember: LocalMember?

    /// Load all local members from the DB.
    func listLocalMembers() -> [LocalMember] {
        let r = run(args: ["local", "member-list", "--json"])
        guard let data = r.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let arr = obj["members"] as? [[String: Any]] else { return [] }
        return arr.compactMap { m in
            guard let key = m["member_key"] as? String else { return nil }
            return LocalMember(
                key: key,
                name: m["display_name"] as? String ?? key,
                serverURL: m["server_url"] as? String ?? "",
                letterID: m["letter_id"] as? String,
                proxyPort: m["proxy_port"] as? Int ?? 1080,
                dnsPort: m["dns_port"] as? Int ?? 53
            )
        }
    }

    /// Whether a TCP/UDP port is currently in use (by any process).
    /// Uses lsof with a bounded wait so the UI thread is never blocked.
    func isPortInUse(_ port: Int) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/lsof")
        p.arguments = ["-t", "-iTCP:\(port)", "-sTCP:LISTEN"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        do {
            try p.run()
        } catch {
            return false
        }
        // Bound the wait (lsof can hang in sandboxed/LSUIElement contexts).
        let deadline = DispatchTime.now() + .seconds(2)
        while p.isRunning && DispatchTime.now() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        if p.isRunning {
            p.terminate()
            return false
        }
        let out = pipe.fileHandleForReading.readDataToEndOfFile()
        // lsof -t prints PIDs of listeners; non-empty means in use.
        return !out.isEmpty
    }

    func startProxy(port: Int? = nil) {
        let p = port ?? currentMember?.proxyPort ?? 1080
        LogBuffer.shared.push("[proxy] 启动请求: 端口 \(p) …")
        if isPortInUse(p) {
            LogBuffer.shared.push("[proxy] 端口 \(p) 已被占用 — 请在设置中更换端口")
            return
        }
        proxy.spawn(args: ["proxy", "start", "--port", "\(p)"])
    }

    func stopProxy() {
        proxy.kill()
    }

    var proxyRunning: Bool { proxy.isRunning }

    // MARK: - Tun0 (managed via Privileged — status check here)

    var tun0Running: Bool {
        // Robust check: a running `teamx tun0 start` process OR a tun device
        // with our fake-ip gateway (198.18.0.1). Either is proof the proxy is
        // up, even if the other is momentarily missing.
        if processRunning("teamx tun0 start") { return true }
        return hasTunDevice()
    }

    /// Whether any process matching `pattern` is running.
    private func processRunning(_ pattern: String) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        p.arguments = ["-f", pattern]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        try? p.run()
        p.waitUntilExit()
        return p.terminationStatus == 0
    }

    /// Whether an interface with our fake-ip gateway address exists.
    private func hasTunDevice() -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/sbin/ifconfig")
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        try? p.run()
        p.waitUntilExit()
        let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return out.contains("198.18.0.1")
    }

    /// Tail the tun0 launch log ($TEAMX_HOME/tun0.log) into the app log so the
    /// user can see why tun0 failed when it doesn't come up.
    func pumpTun0Log() {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: NSHomeDirectory() + "/.teamx/tun0.log")),
              let s = String(data: data, encoding: .utf8) else { return }
        let lines = s.split(separator: "\n")
        guard let last = lines.last else { return }
        let key = "[tun0] " + String(last)
        let recent = LogBuffer.shared.snapshot()
        if recent.last != key {
            LogBuffer.shared.push(key)
        }
    }

    // MARK: - Tunnel

    func listTunnels() -> [TunnelEntry] {
        let r = run(args: ["tunnel", "list", "--json", "--session", "gui"])
        guard let data = r.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return []
        }
        let dataDict = obj["data"] as? [String: Any] ?? obj
        let arr = dataDict["tunnels"] as? [[String: Any]] ?? []
        return arr.compactMap { d in
            guard let name = d["name"] as? String else { return nil }
            return TunnelEntry(
                name: name,
                mode: d["mode"] as? String ?? "local",
                port: d["port"] as? Int ?? 0,
                providerMemberID: d["provider_member_id"] as? String,
                targetPort: d["target_port"] as? Int,
                lanIP: d["lan_ip"] as? String
            )
        }
    }

    func startExpose(name: String, port: Int, mode: String) {
        let worker = WorkerProcess(label: "tunnel-\(name)")
        tunnelsLock.lock()
        tunnels[name] = worker
        tunnelsLock.unlock()
        var args = ["tunnel", "expose", name, "--port", "\(port)", "--session", "gui"]
        if mode != "local" { args.append(contentsOf: ["--mode", mode]) }
        worker.spawn(args: args)
    }

    func startForward(name: String, localPort: Int?) {
        let worker = WorkerProcess(label: "forward-\(name)")
        tunnelsLock.lock()
        tunnels[name] = worker
        tunnelsLock.unlock()
        var args = ["tunnel", "forward", name, "--session", "gui"]
        if let p = localPort { args.append(contentsOf: ["--local-port", "\(p)"]) }
        worker.spawn(args: args)
    }

    func stopTunnel(name: String) {
        tunnelsLock.lock()
        let w = tunnels.removeValue(forKey: name)
        tunnelsLock.unlock()
        w?.kill()
        // Also close on the server (best-effort).
        run(args: ["tunnel", "close", name, "--session", "gui"])
    }

    func tunnelWorkerRunning(_ name: String) -> Bool {
        tunnelsLock.lock()
        let w = tunnels[name]
        tunnelsLock.unlock()
        return w?.isRunning ?? false
    }

    // MARK: - Routes / default exit

    /// Current default exit from the SQLite route table ("(none)" if unset).
    /// Served from the cache (see `refreshExitCache`); never blocks.
    func defaultExit() -> String {
        exitCacheLock.lock()
        defer { exitCacheLock.unlock() }
        return cachedDefaultExit
    }

    /// Set the default exit.
    func setDefaultExit(_ exit: String) {
        _ = run(args: ["proxy", "routes", "set-default", exit])
        LogBuffer.shared.push("[routes] 默认出口 → \(exit)")
        refreshExitCache()
    }

    /// List team names + exit candidates (from `tunnel list`), served from the
    /// cache. Never blocks the main thread.
    func listExits() -> [String] {
        exitCacheLock.lock()
        defer { exitCacheLock.unlock() }
        return cachedExits
    }

    /// Refresh the exit menu cache on a background queue. Safe to call often.
    func refreshExitCache() {
        DispatchQueue.global(qos: .utility).async {
            let exits = self.fetchExits()
            let def = self.fetchDefaultExit()
            self.exitCacheLock.lock()
            self.cachedExits = exits
            self.cachedDefaultExit = def
            self.exitCacheLock.unlock()
        }
    }

    private func fetchExits() -> [String] {
        let r = run(args: ["tunnel", "list", "--json", "--session", "gui"])
        guard let data = r.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return []
        }
        let dataDict = obj["data"] as? [String: Any] ?? obj
        let arr = dataDict["tunnels"] as? [[String: Any]] ?? []
        return arr.compactMap { $0["name"] as? String }
    }

    private func fetchDefaultExit() -> String {
        let r = run(args: ["proxy", "routes", "list", "--json"])
        guard let data = r.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return "(none)"
        }
        return obj["default"] as? String ?? "(none)"
    }
}

/// A tunnel entry from `tunnel list`.
struct TunnelEntry {
    let name: String
    let mode: String
    let port: Int
    let providerMemberID: String?
    let targetPort: Int?
    let lanIP: String?
}

/// A local member config (from `teamx local member-list`).
struct LocalMember {
    let key: String
    let name: String
    let serverURL: String
    let letterID: String?
    let proxyPort: Int
    let dnsPort: Int
}
