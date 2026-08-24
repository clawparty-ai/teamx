// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "TeamxApp",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "TeamxApp",
            path: "Sources/TeamxApp",
            resources: [
                // tray/app icons are copied by the build script (build-teamx-app.sh)
            ]
        ),
    ]
)
