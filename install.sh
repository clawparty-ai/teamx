#!/usr/bin/env bash
# teamx installer / uninstaller.
#
# Install:
#   1. Builds the Rust CLI (cargo build --release) → ~/.local/bin/teamx
#   2. Builds the opencode plugin bundle and installs the three pieces into the
#      opencode config dir (override: $OPENCODE_CONFIG):
#        - plugins/teamx.js
#        - agent/teamx.md
#        - command/Team.md
#   3. Pins @opencode-ai/plugin to the running opencode version.
#   4. Hardens permissions on the teamx state dir (~/.teamx → 0700, db → 0600).
#
# Uninstall:  ./install.sh --uninstall
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
CONFIG_DIR="${OPENCODE_CONFIG:-$HOME/.config/opencode}"
BIN_DIR="${TEAMX_BIN_DIR:-$HOME/.local/bin}"
PLUGIN_DIR="$ROOT/opencode-plugin"
TEAMX_HOME_DIR="${TEAMX_HOME:-$HOME/.teamx}"

step() { printf '\n== %s ==\n' "$*"; }

uninstall() {
  step "uninstalling teamx"
  rm -f "$BIN_DIR/teamx"
  rm -f "$CONFIG_DIR/plugins/teamx.js" \
        "$CONFIG_DIR/agent/teamx.md" \
        "$CONFIG_DIR/agent/teamx.en.md" \
        "$CONFIG_DIR/command/Team.md" \
        "$CONFIG_DIR/command/Team.en.md" \
        "$CONFIG_DIR/commands/team.md" \
        "$CONFIG_DIR/commands/team-create.md" \
        "$CONFIG_DIR/commands/team-create.en.md" \
        "$CONFIG_DIR/commands/team-join.md" \
        "$CONFIG_DIR/commands/team-join.en.md" \
        "$CONFIG_DIR/commands/team-status.md" \
        "$CONFIG_DIR/commands/team-status.en.md" \
        "$CONFIG_DIR/commands/team-sync.md" \
        "$CONFIG_DIR/commands/team-sync.en.md" \
        "$CONFIG_DIR/commands/team-goal-set.md" \
        "$CONFIG_DIR/commands/team-goal-set.en.md" \
        "$CONFIG_DIR/commands/team-goal-share.md" \
        "$CONFIG_DIR/commands/team-goal-share.en.md" \
        "$CONFIG_DIR/commands/team-goal-close.md" \
        "$CONFIG_DIR/commands/team-goal-close.en.md" \
        "$CONFIG_DIR/commands/team-approve.md" \
        "$CONFIG_DIR/commands/team-approve.en.md" \
        "$CONFIG_DIR/commands/team-deny.md" \
        "$CONFIG_DIR/commands/team-deny.en.md" \
        "$CONFIG_DIR/commands/team-role.md" \
        "$CONFIG_DIR/commands/team-role.en.md" \
        "$CONFIG_DIR/commands/team-role-propose.md" \
        "$CONFIG_DIR/commands/team-role-propose.en.md" \
        "$CONFIG_DIR/commands/team-role-approve.md" \
        "$CONFIG_DIR/commands/team-role-approve.en.md" \
        "$CONFIG_DIR/commands/team-role-deny.md" \
        "$CONFIG_DIR/commands/team-role-deny.en.md" \
        "$CONFIG_DIR/commands/team-role-update.md" \
        "$CONFIG_DIR/commands/team-role-update.en.md" \
        "$CONFIG_DIR/commands/team-invite.md" \
        "$CONFIG_DIR/commands/team-invite.en.md" \
        "$CONFIG_DIR/commands/team-import.md" \
        "$CONFIG_DIR/commands/team-import.en.md" \
        "$CONFIG_DIR/commands/team-invite-list.md" \
        "$CONFIG_DIR/commands/team-invite-list.en.md" \
        "$CONFIG_DIR/commands/team-invite-revoke.md" \
        "$CONFIG_DIR/commands/team-invite-revoke.en.md" \
        "$CONFIG_DIR/commands/team-state.md" \
        "$CONFIG_DIR/commands/team-state.en.md" \
        "$CONFIG_DIR/commands/team-ask.md" \
        "$CONFIG_DIR/commands/team-ask.en.md" \
        "$CONFIG_DIR/commands/team-respond.md" \
        "$CONFIG_DIR/commands/team-respond.en.md" \
        "$CONFIG_DIR/commands/team-publish.md" \
        "$CONFIG_DIR/commands/team-publish.en.md" \
        "$CONFIG_DIR/commands/team-archive.md" \
        "$CONFIG_DIR/commands/team-archive.en.md" \
        "$CONFIG_DIR/commands/team-destroy.md" \
        "$CONFIG_DIR/commands/team-destroy.en.md" \
        "$CONFIG_DIR/commands/team-serve.md" \
        "$CONFIG_DIR/commands/team-serve.en.md" \
        "$CONFIG_DIR/commands/team-serve-start.md" \
        "$CONFIG_DIR/commands/team-serve-start.en.md" \
        "$CONFIG_DIR/commands/team-serve-status.md" \
        "$CONFIG_DIR/commands/team-serve-status.en.md" \
        "$CONFIG_DIR/commands/team-serve-stop.md" \
        "$CONFIG_DIR/commands/team-serve-stop.en.md" \
        "$CONFIG_DIR/commands/team-serve-token.md" \
        "$CONFIG_DIR/commands/team-serve-token.en.md" \
        "$CONFIG_DIR/commands/team-help.md" \
        "$CONFIG_DIR/commands/team-help.en.md"
  # NOTE: keep ~/.teamx (the SQLite data) — it is user data; removing the
  # binary/plugin does not destroy teams.
  echo "removed binary and opencode pieces."
  echo "team data is preserved under $TEAMX_HOME_DIR (delete it manually if you want to wipe teams)."
  exit 0
}

case "${1:-}" in
  --uninstall|-u) uninstall ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo (Rust) is required to build teamx" >&2
  exit 1
fi

step "building teamx CLI (release)"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$BIN_DIR"
install -m 0755 "$ROOT/target/release/teamx" "$BIN_DIR/teamx"
echo "installed $BIN_DIR/teamx"

step "building opencode plugin bundle"
if ! command -v bun >/dev/null 2>&1; then
  echo "ERROR: bun is required to build the plugin bundle" >&2
  exit 1
fi
(cd "$PLUGIN_DIR" && bun install >/dev/null 2>&1 && bun run build)

step "installing opencode plugin pieces"
mkdir -p "$CONFIG_DIR/plugins" "$CONFIG_DIR/agent" "$CONFIG_DIR/commands"
# Primary location: the standard plural `commands/` directory (same as loopx).
cp "$PLUGIN_DIR/dist/teamx.js" "$CONFIG_DIR/plugins/teamx.js"
cp "$PLUGIN_DIR/assets/agent/teamx.md" "$CONFIG_DIR/agent/teamx.md"
cp "$PLUGIN_DIR/assets/agent/teamx.en.md" "$CONFIG_DIR/agent/teamx.en.md"
# Install every /team command file (main router + all flat aliases + English variants).
for cmd in Team team-create team-join team-status team-sync \
           team-goal-set team-goal-share team-goal-close \
           team-approve team-deny team-role team-role-propose \
           team-role-approve team-role-deny team-role-update \
           team-invite team-import team-invite-list team-invite-revoke \
           team-state team-ask team-respond team-publish team-archive \
           team-destroy \
           team-serve team-serve-start team-serve-status team-serve-stop team-serve-token \
           team-help; do
  cp "$PLUGIN_DIR/assets/command/$cmd.md" "$CONFIG_DIR/commands/$cmd.md"
  cp "$PLUGIN_DIR/assets/command/$cmd.en.md" "$CONFIG_DIR/commands/$cmd.en.md"
done
# Back-compat: also drop the legacy singular `command/` copy so upgrades do not
# leave a stale `/Team` behind on case-sensitive filesystems. On macOS APFS
# (case-insensitive) `Team.md` and `team.md` are the same file, so only one
# copy is written there.
if [ -d "$CONFIG_DIR/command" ]; then
  cp "$PLUGIN_DIR/assets/command/Team.md" "$CONFIG_DIR/command/Team.md"
fi
# NOTE: do not `rm` legacy lowercase names here — macOS default APFS is
# case-insensitive and `rm .../team.md` would also delete Team.md.
echo "installed to $CONFIG_DIR/{plugins,agent,commands}/teamx.* / team*.md"

step "ensuring @opencode-ai/plugin dependency"
VERSION="$(opencode --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
VERSION="${VERSION:-1.17.11}"
if [ -f "$CONFIG_DIR/package.json" ]; then
  if command -v bun >/dev/null 2>&1; then
    (cd "$CONFIG_DIR" && bun add "@opencode-ai/plugin@$VERSION" >/dev/null 2>&1 || true)
    echo "pinned @opencode-ai/plugin@$VERSION in $CONFIG_DIR/package.json (bun add)"
  else
    echo "NOTE: bun not found; opencode will install deps at startup, or run: (cd $CONFIG_DIR && bun add @opencode-ai/plugin@$VERSION)"
  fi
else
  cat > "$CONFIG_DIR/package.json" <<EOF
{
  "dependencies": {
    "@opencode-ai/plugin": "$VERSION"
  }
}
EOF
  echo "wrote $CONFIG_DIR/package.json with @opencode-ai/plugin@$VERSION"
fi

step "hardening permissions"
mkdir -p "$TEAMX_HOME_DIR"
chmod 0700 "$TEAMX_HOME_DIR"
chmod 0600 "$TEAMX_HOME_DIR/teamx.db" 2>/dev/null || true
chmod 0600 "$TEAMX_HOME_DIR/instance.json" 2>/dev/null || true
echo "set $TEAMX_HOME_DIR → 0700; db/instance.json → 0600"

step "done"
"$BIN_DIR/teamx" --version
echo
echo "Restart opencode (or reload) so the plugin, agent and /Team command are picked up."
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
  echo "NOTE: add $BIN_DIR to your PATH (e.g. export PATH=\"$BIN_DIR:\$PATH\")"
fi
