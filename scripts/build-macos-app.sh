#!/usr/bin/env bash
# build-macos-app.sh — build a double-clickable macOS .app for the teamx
# desktop tray (L1). Run on macOS from the repo root.
#
#   ./scripts/build-macos-app.sh          # -> dist/Teamx.app
#   ./scripts/build-macos-app.sh --sign   # ad-hoc codesign
#
# Requires: cargo, sips, iconutil (macOS built-ins).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="Teamx"
DIST_DIR="$ROOT/dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
LOGO_SRC="${TEAMX_LOGO_SRC:-$ROOT/docs/logo/out/teamx-logo-1024.png}"
ICONSET_DIR="$DIST_DIR/AppIcon.iconset"
RESOURCES_DIR="$APP_DIR/Contents/Resources"

step() { printf '\n== %s ==\n' "$*"; }

step "1/5 build release binary (gui feature)"
cargo build --release --features gui --manifest-path "$ROOT/Cargo.toml"

step "2/5 prepare app bundle layout"
rm -rf "$APP_DIR" "$ICONSET_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$RESOURCES_DIR" "$ICONSET_DIR"

step "3/5 generate AppIcon.icns from logo ($LOGO_SRC)"
# macOS iconutil expects an .iconset with specific sizes.
sips -z 16 16   "$LOGO_SRC" --out "$ICONSET_DIR/icon_16x16.png"      >/dev/null
sips -z 32 32   "$LOGO_SRC" --out "$ICONSET_DIR/icon_16x16@2x.png"   >/dev/null
sips -z 32 32   "$LOGO_SRC" --out "$ICONSET_DIR/icon_32x32.png"      >/dev/null
sips -z 64 64   "$LOGO_SRC" --out "$ICONSET_DIR/icon_32x32@2x.png"   >/dev/null
sips -z 128 128 "$LOGO_SRC" --out "$ICONSET_DIR/icon_128x128.png"    >/dev/null
sips -z 256 256 "$LOGO_SRC" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$LOGO_SRC" --out "$ICONSET_DIR/icon_256x256.png"    >/dev/null
sips -z 512 512 "$LOGO_SRC" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$LOGO_SRC" --out "$ICONSET_DIR/icon_512x512.png"    >/dev/null
cp "$LOGO_SRC" "$ICONSET_DIR/icon_512x512@2x.png"                     # 1024 -> @2x
iconutil -c icns "$ICONSET_DIR" -o "$RESOURCES_DIR/AppIcon.icns"

step "4/5 copy binary + tray icon + Info.plist"
cp "$ROOT/target/release/teamx" "$APP_DIR/Contents/MacOS/teamx"
# Small tray icon (16/32px friendly) — derive from the 512 logo.
sips -z 32 32 "$ROOT/docs/logo/out/teamx-logo-512.png" --out "$RESOURCES_DIR/tray.png" >/dev/null
chmod 0755 "$APP_DIR/Contents/MacOS/teamx"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>     <string>Teamx</string>
    <key>CFBundleIdentifier</key>      <string>io.flomesh.teamx</string>
    <key>CFBundleVersion</key>         <string>0.3.1</string>
    <key>CFBundleShortVersionString</key><string>0.3.1</string>
    <key>CFBundleExecutable</key>      <string>teamx</string>
    <key>CFBundleIconFile</key>        <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>   <string>11.0</string>
    <key>LSUIElement</key>             <true/>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSHumanReadableCopyright</key><string>© 2026 teamx</string>
</dict>
</plist>
PLIST

step "5/5 sign (ad-hoc)"
if [[ "${1:-}" == "--sign" ]]; then
    codesign --force --deep --sign - "$APP_DIR"
    echo "signed (ad-hoc): $APP_DIR"
else
    echo "not signed (pass --sign for ad-hoc codesign)"
fi

rm -rf "$ICONSET_DIR"
echo
echo "Done: $APP_DIR"
echo "Double-click Teamx.app to launch the tray. Quit via its menu."
