//
//  Privileged.swift — tun0 start/stop with system authorization.
//
//  tun0 needs root. We elevate via AppleScript's `with administrator
//  privileges`, launching the command detached so the auth dialog returns
//  immediately and the worker keeps running independently. The command must
//  NOT use nohup (no tty under do shell script) — a plain background `&` with
//  stdio redirected detaches correctly.
//
//  System DNS is managed inside the teamx binary itself (set on start with the
//  original DNS kept as fallback, restored on `teamx tun0 stop`), so this file
//  only starts/stops the process.
//

import Foundation

enum Privileged {
    /// Start tun0 as root. Builds a shell command that exports the mTLS env
    /// and launches `teamx tun0 start` detached, logging to $TEAMX_HOME/tun0.log
    /// (NOT /tmp: a fixed /tmp path would let any local user pre-create a symlink
    /// and have the elevated process overwrite an arbitrary file).
    /// Always clears any previous tun0 process first so only one instance runs.
    static func startTun0() {
        guard let teamx = TeamxCore.teamxURL()?.path else {
            LogBuffer.shared.push("[tun0] teamx 二进制未找到")
            return
        }
        let envPrefix = Self.mTLSEnvPrefix()
        // Kill any prior instance, then start fresh (guarantees a single tun0).
        // teamx tun0 start sets system DNS itself (fake-ip gateway + fallback).
        let cmd = "pkill -f 'teamx tun0 start' 2>/dev/null; pkill -f 'tun0 start' 2>/dev/null; sleep 1; " +
                  "\(envPrefix)mkdir -p \"$TEAMX_HOME\" 2>/dev/null; '\(teamx)' tun0 start > \"$TEAMX_HOME/tun0.log\" 2>&1 </dev/null &"
        LogBuffer.shared.push("[tun0] 请求系统授权以启动 tun0 …")
        runDetached(cmd)
    }

    /// Stop tun0 as root. Runs `teamx tun0 stop` (restores system DNS + removes
    /// the route), then kills any lingering start process.
    static func stopTun0() {
        guard let teamx = TeamxCore.teamxURL()?.path else {
            LogBuffer.shared.push("[tun0] teamx 二进制未找到")
            return
        }
        let envPrefix = Self.mTLSEnvPrefix()
        let cmd = "\(envPrefix)'\(teamx)' tun0 stop 2>/dev/null; " +
                  "pkill -f 'teamx tun0 start' 2>/dev/null; pkill -f 'tun0 start' 2>/dev/null"
        LogBuffer.shared.push("[tun0] 请求系统授权以停止 tun0（恢复系统 DNS）…")
        runDetached(cmd)
    }

    /// Restart tun0 as root: stop, wait, then start again (so route/port
    /// changes take effect). One elevated prompt for the whole sequence.
    static func restartTun0() {
        guard let teamx = TeamxCore.teamxURL()?.path else {
            LogBuffer.shared.push("[tun0] teamx 二进制未找到")
            return
        }
        let envPrefix = Self.mTLSEnvPrefix()
        let cmd = "\(envPrefix)'\(teamx)' tun0 stop 2>/dev/null; " +
                  "pkill -f 'teamx tun0 start' 2>/dev/null; pkill -f 'tun0 start' 2>/dev/null; " +
                  "sleep 1; " +
                  "\(envPrefix)mkdir -p \"$TEAMX_HOME\" 2>/dev/null; '\(teamx)' tun0 start > \"$TEAMX_HOME/tun0.log\" 2>&1 </dev/null &"
        LogBuffer.shared.push("[tun0] 请求系统授权以重启 tun0 …")
        runDetached(cmd)
    }

    /// Build the shell `export` prefix for the mTLS env the teamx binary needs.
    private static func mTLSEnvPrefix() -> String {
        var envPrefix = ""
        let keys = ["TEAMX_HOME", "TEAMX_DB", "TEAMX_SERVER_URL",
                    "TEAMX_MTLS_CERT", "TEAMX_MTLS_KEY", "TEAMX_MTLS_CA"]
        let env = ProcessInfo.processInfo.environment
        for k in keys {
            if let v = env[k] {
                let escaped = v.replacingOccurrences(of: "'", with: "'\\''")
                envPrefix += "export \(k)='\(escaped)'; "
            }
        }
        return envPrefix
    }

    /// Run a shell command elevated via AppleScript, detached (non-blocking).
    private static func runDetached(_ cmd: String) {
        let script = "do shell script \"\(cmd.replacingOccurrences(of: "\"", with: "\\\""))\" with administrator privileges"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        p.arguments = ["-e", script]
        p.standardInput = FileHandle.nullDevice
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do {
            try p.run()
        } catch {
            LogBuffer.shared.push("[tun0] 无法弹出授权: \(error.localizedDescription)")
        }
    }
}
