//
//  TeamxServer.swift — network status: server info + member presence.
//
//  Talks to the teamx server over mTLS using the bundled `curl` (macOS ships
//  it) with the client certificate from the imported letter:
//    POST /rpc   {"method":"team.status", ...}  -> teams[].members[]
//    GET  /health                               -> online?
//

import Foundation

/// One team member, as reported by the server.
struct TeamMember {
    let name: String
    let role: String
    let state: String
    let id: String
    let ip: String?
    let online: Bool
}

/// Live network metrics for a member.
struct MemberMetrics {
    let pingMs: Double?
    let rxBps: Int
    let txBps: Int
    let online: Bool
}

/// Server connection info.
struct ServerInfo {
    let url: String
    let host: String
    let port: String
}

/// mTLS material from the currently selected invitation letter.
struct ServerMaterial {
    let serverURL: String
    let caPath: String
    let certPath: String
    let keyPath: String
}

/// Discovers the server URL + mTLS material from imported letters
/// (~/.teamx/letters/<id>/) and calls RPCs over mTLS via curl.
enum TeamxServer {
    /// Find the most recently imported letter (skip current.json).
    static func currentMaterial(letterID: String? = nil, serverURL: String? = nil) -> ServerMaterial? {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let lettersDir = home.appendingPathComponent(".teamx/letters")
        guard let names = try? FileManager.default.contentsOfDirectory(atPath: lettersDir.path) else {
            return nil
        }
        // If a specific letter is requested, use it.
        if let lid = letterID {
            let dir = lettersDir.appendingPathComponent(lid)
            let letterPath = dir.appendingPathComponent("letter.json")
            guard FileManager.default.fileExists(atPath: letterPath.path),
                  let data = try? Data(contentsOf: URL(fileURLWithPath: letterPath.path)),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let inv = obj["teamx_invitation"] as? [String: Any],
                  let server = inv["server"] as? [String: Any] else {
                return nil
            }
            let url = serverURL ?? (server["url"] as? String ?? "")
            return ServerMaterial(
                serverURL: url,
                caPath: dir.appendingPathComponent("ca.crt").path,
                certPath: dir.appendingPathComponent("client.crt").path,
                keyPath: dir.appendingPathComponent("client.key").path
            )
        }
        // Otherwise fall back to the most recent letter.
        var best: (path: String, mtime: Date)?
        for name in names where name != "current.json" {
            let dir = lettersDir.appendingPathComponent(name)
            let letterPath = dir.appendingPathComponent("letter.json")
            guard FileManager.default.fileExists(atPath: letterPath.path) else { continue }
            let mtime = ((try? FileManager.default.attributesOfItem(atPath: letterPath.path))?[.modificationDate] as? Date) ?? .distantPast
            if best == nil || mtime > best!.mtime {
                best = (letterPath.path, mtime)
            }
        }
        guard let letterPath = best?.path,
              let data = try? Data(contentsOf: URL(fileURLWithPath: letterPath)),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let inv = obj["teamx_invitation"] as? [String: Any],
              let server = inv["server"] as? [String: Any],
              let url = server["url"] as? String else { return nil }
        let dir = URL(fileURLWithPath: letterPath).deletingLastPathComponent()
        return ServerMaterial(
            serverURL: serverURL ?? url,
            caPath: dir.appendingPathComponent("ca.crt").path,
            certPath: dir.appendingPathComponent("client.crt").path,
            keyPath: dir.appendingPathComponent("client.key").path
        )
    }

    /// Server host + port.
    static func serverInfo(_ material: ServerMaterial? = nil) -> ServerInfo? {
        guard let m = material ?? currentMaterial() else { return nil }
        let cleaned = m.serverURL
            .replacingOccurrences(of: "https://", with: "")
            .replacingOccurrences(of: "http://", with: "")
        let parts = cleaned.split(separator: ":")
        let host = parts.first.map(String.init) ?? ""
        let port = parts.count > 1 ? String(parts[1]) : "443"
        return ServerInfo(url: m.serverURL, host: host, port: port)
    }

    /// Fetch the team member list via `team.status` RPC (mTLS via curl).
    static func teamMembers(_ material: ServerMaterial? = nil) -> [TeamMember] {
        guard let m = material ?? currentMaterial() else { return [] }
        let body = "{\"method\":\"team.status\",\"args\":{\"session\":\"app\"}}"
        guard let data = curlRPC(m, body: body) else { return [] }
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let d = obj["data"] as? [String: Any],
              let teams = d["teams"] as? [[String: Any]],
              let team = teams.first,
              let members = team["members"] as? [[String: Any]] else { return [] }
        return members.compactMap { mm in
            guard let id = mm["id"] as? String else { return nil }
            return TeamMember(
                name: mm["display_name"] as? String ?? "?",
                role: mm["role"] as? String ?? "?",
                state: mm["state"] as? String ?? "?",
                id: id,
                ip: mm["ip"] as? String,
                online: mm["online"] as? Bool ?? false
            )
        }
    }

    /// Live network metrics for all members (team.metrics RPC).
    static func memberMetrics(_ material: ServerMaterial? = nil) -> [String: MemberMetrics] {
        guard let m = material ?? currentMaterial() else { return [:] }
        let body = "{\"method\":\"team.metrics\",\"args\":{}}"
        guard let data = curlRPC(m, body: body) else { return [:] }
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let d = obj["data"] as? [String: Any],
              let metrics = d["metrics"] as? [String: Any] else { return [:] }
        var out: [String: MemberMetrics] = [:]
        for (id, raw) in metrics {
            guard let r = raw as? [String: Any] else { continue }
            out[id] = MemberMetrics(
                pingMs: r["ping_ms"] as? Double,
                rxBps: r["rx_bps"] as? Int ?? 0,
                txBps: r["tx_bps"] as? Int ?? 0,
                online: r["online"] as? Bool ?? false
            )
        }
        return out
    }

    /// Whether the server is reachable (health endpoint returns ok).
    static func serverOnline(_ material: ServerMaterial? = nil) -> Bool {
        guard let m = material ?? currentMaterial() else { return false }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/curl")
        p.arguments = ["-s", "--max-time", "5", "--cacert", m.caPath,
                       "--cert", m.certPath, "--key", m.keyPath,
                       m.serverURL + "/health"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        try? p.run()
        p.waitUntilExit()
        let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return out.contains("\"ok\":true") || out.contains("\"ok\": true")
    }

    /// POST to /rpc via curl with mTLS.
    private static func curlRPC(_ m: ServerMaterial, body: String) -> Data? {
        // Write body to a temp file to avoid quoting issues.
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("teamx-rpc-\(UUID().uuidString).json")
        try? body.write(to: tmp, atomically: true, encoding: .utf8)

        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/curl")
        p.arguments = ["-s", "--max-time", "8",
                       "--cacert", m.caPath, "--cert", m.certPath, "--key", m.keyPath,
                       "-H", "Content-Type: application/json",
                       "--data-binary", "@\(tmp.path)",
                       m.serverURL + "/rpc"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        try? p.run()
        p.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        try? FileManager.default.removeItem(at: tmp)
        return data
    }
}
