#!/usr/bin/env bash
# build-teamx-app.sh — build the Swift/AppKit Teamx desktop app.
#
#   ./scripts/build-teamx-app.sh          # -> dist/Teamx.app (debug swift build)
#   ./scripts/build-teamx-app.sh --release  # release swift build
#   ./scripts/build-teamx-app.sh --install-agent  # + login LaunchAgent
#
# Requires: swift (Xcode CLT), cargo (Rust), sips, iconutil (macOS).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="Teamx"
DIST_DIR="$ROOT/dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
APP_SRC="$ROOT/app"
LOGO_SRC="${TEAMX_LOGO_SRC:-$ROOT/docs/logo/out/teamx-logo-1024.png}"
ICONSET_DIR="$DIST_DIR/AppIcon.iconset"
RES_DIR="$APP_DIR/Contents/Resources"

step() { printf '\n== %s ==\n' "$*"; }

step "1/6 build Rust teamx CLI (release)"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

step "2/6 build Swift app"
cd "$APP_SRC"
if [[ "${1:-}" == "--release" || "${1:-}" == "--install-agent" ]]; then
    swift build -c release
    SWIFT_BIN="$APP_SRC/.build/release/TeamxApp"
else
    swift build
    SWIFT_BIN="$APP_SRC/.build/debug/TeamxApp"
fi
cd "$ROOT"

step "3/6 prepare .app layout"
rm -rf "$APP_DIR" "$ICONSET_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$RES_DIR" "$ICONSET_DIR"

step "4/6 generate AppIcon.icns from logo ($LOGO_SRC)"
sips -z 16 16   "$LOGO_SRC" --out "$ICONSET_DIR/icon_16x16.png"      >/dev/null
sips -z 32 32   "$LOGO_SRC" --out "$ICONSET_DIR/icon_16x16@2x.png"   >/dev/null
sips -z 32 32   "$LOGO_SRC" --out "$ICONSET_DIR/icon_32x32.png"      >/dev/null
sips -z 64 64   "$LOGO_SRC" --out "$ICONSET_DIR/icon_32x32@2x.png"   >/dev/null
sips -z 128 128 "$LOGO_SRC" --out "$ICONSET_DIR/icon_128x128.png"    >/dev/null
sips -z 256 256 "$LOGO_SRC" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$LOGO_SRC" --out "$ICONSET_DIR/icon_256x256.png"    >/dev/null
sips -z 512 512 "$LOGO_SRC" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$LOGO_SRC" --out "$ICONSET_DIR/icon_512x512.png"    >/dev/null
cp "$LOGO_SRC" "$ICONSET_DIR/icon_512x512@2x.png"
iconutil -c icns "$ICONSET_DIR" -o "$RES_DIR/AppIcon.icns"
# tray icon (small, template-friendly)
sips -z 32 32 "$ROOT/docs/logo/out/teamx-logo-512.png" --out "$RES_DIR/tray.png" >/dev/null

step "5/6 copy Swift binary + Rust teamx CLI + Info.plist"
cp "$SWIFT_BIN" "$APP_DIR/Contents/MacOS/TeamxApp"
chmod 0755 "$APP_DIR/Contents/MacOS/TeamxApp"
# bundled Rust CLI — the app looks it up in Resources/teamx
cp "$ROOT/target/release/teamx" "$RES_DIR/teamx"
chmod 0755 "$RES_DIR/teamx"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>     <string>Teamx</string>
    <key>CFBundleIdentifier</key>      <string>io.flomesh.teamx</string>
    <key>CFBundleVersion</key>         <string>0.4.0</string>
    <key>CFBundleShortVersionString</key><string>0.4.0</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleExecutable</key>      <string>TeamxApp</string>
    <key>CFBundleIconFile</key>        <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>  <string>13.0</string>
    <key>LSUIElement</key>             <true/>
    <key>NSPrincipalClass</key>        <string>NSApplication</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSHumanReadableCopyright</key><string>© 2026 teamx</string>
</dict>
</plist>
PLIST

step "6/6 sign (ad-hoc)"
codesign --force --deep --sign - "$APP_DIR" 2>/dev/null || echo "(sign skipped)"
rm -rf "$ICONSET_DIR"

echo
echo "Done: $APP_DIR"
echo
echo "Launch options:"
echo "  1) Double-click Teamx.app in Finder (LaunchServices may refuse on"
echo "     unsigned builds — use right-click -> Open the first time)."
echo "  2) Reliable launch:"
echo "       launchctl submit -l teamx -- \"$APP_DIR/Contents/MacOS/TeamxApp\""
echo "  3) Login auto-start (LaunchAgent):"
echo "       $0 --install-agent"
echo "Quit via the tray menu."

if [[ "${1:-}" == "--install-agent" ]]; then
    AGENT_DIR="$HOME/Library/LaunchAgents"
    AGENT_PLIST="$AGENT_DIR/io.flomesh.teamx.plist"
    mkdir -p "$AGENT_DIR"
    cat > "$AGENT_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>              <string>io.flomesh.teamx</string>
    <key>ProgramArguments</key>
    <array>
        <string>${APP_DIR}/Contents/MacOS/TeamxApp</string>
    </array>
    <key>RunAtLoad</key>          <true/>
    <key>KeepAlive</key>          <true/>
    <key>ProcessType</key>        <string>Interactive</string>
</dict>
</plist>
PLIST
    launchctl unload "$AGENT_PLIST" 2>/dev/null || true
    launchctl load "$AGENT_PLIST"
    echo
    echo "LaunchAgent installed: $AGENT_PLIST (auto-starts at login, restarts if killed)"
fi
