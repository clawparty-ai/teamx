//
//  main.swift — app entry.
//

import AppKit
import Darwin

// Single-instance guard: take an exclusive flock on a lock file. If another
// TeamxApp already holds it, exit immediately so only one tray app runs.
// FD_CLOEXEC prevents child processes (teamx CLI) from inheriting the lock,
// so the lock is released when this app exits even if a worker lingers.
let lockPath = "/tmp/io.flomesh.teamx.lock"
let lockFD = open(lockPath, O_CREAT | O_RDWR, 0o600)
let isSelfTest = CommandLine.arguments.contains("--self-test-tun0") || CommandLine.arguments.contains("--self-test-proxy")
if lockFD >= 0 && !isSelfTest {
    _ = fcntl(lockFD, F_SETFD, FD_CLOEXEC)
    if flock(lockFD, LOCK_EX | LOCK_NB) != 0 {
        fputs("teamx: another instance is already running\n", stderr)
        exit(0)
    }
} else {
    // Could not create the lock file (unusual); allow the app to continue.
}

// Self-test hook: `TeamxApp --self-test-tun0` triggers Privileged.startTun0
// (as if the panel button was clicked) so crashes can be reproduced from a
// terminal.
if CommandLine.arguments.contains("--self-test-tun0") {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    Privileged.startTun0()
    // keep running a bit so any async spawn/exit is observable
    DispatchQueue.main.asyncAfter(deadline: .now() + 8) {
        print("SELF-TEST: still alive after tun0 start request")
        exit(0)
    }
    app.run()
}

// Self-test: start proxy via the same code path as the panel button.
if CommandLine.arguments.contains("--self-test-proxy") {
    func trace(_ s: String) { FileHandle.standardError.write((s + "\n").data(using: .utf8)!) }
    trace("SELF-TEST: enter")
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    trace("SELF-TEST: listing members")
    let members = TeamxCore.shared.listLocalMembers()
    trace("SELF-TEST: members=\(members.count)")
    TeamxCore.shared.currentMember = members.first
    TeamxCore.shared.startProxy()
    trace("SELF-TEST: proxy requested")
    DispatchQueue.main.asyncAfter(deadline: .now() + 6) {
        trace("SELF-TEST: proxyRunning=\(TeamxCore.shared.proxyRunning)")
        exit(0)
    }
    app.run()
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)   // menu bar app (no Dock icon)
app.run()
