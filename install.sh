#!/bin/sh
# agentop installer
#
#   curl -fsSL https://raw.githubusercontent.com/sacx/agentop/main/install.sh | sh
#   curl -fsSL …/install.sh | sh -s -- --i3 --polybar
#
# Options:
#   --i3            float the dashboard, autostart the tray from i3, add the i3bar block
#   --polybar       add the polybar module (and put it in modules-right)
#   --from-source   build with cargo instead of downloading a release binary
#   --no-start      don't start the tray now
#   --uninstall     remove agentop and every integration it added
#
# Env: AGENTOP_REPO (owner/repo), AGENTOP_VERSION (tag, default latest),
#      AGENTOP_BIN_DIR (default ~/.local/bin)
set -eu

REPO="${AGENTOP_REPO:-sacx/agentop}"
VERSION="${AGENTOP_VERSION:-latest}"
BIN_DIR="${AGENTOP_BIN_DIR:-$HOME/.local/bin}"
BIN="$BIN_DIR/agentop"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}"
APP_FILE="$DATA/applications/agentop.desktop"
ICON_FILE="$DATA/icons/hicolor/scalable/apps/agentop.svg"
AUTOSTART_FILE="$CONF/autostart/agentop-tray.desktop"
BEGIN="# >>> agentop >>>"
END="# <<< agentop <<<"

I3=0 POLYBAR=0 SOURCE=0 START=1 UNINSTALL=0
for a in "$@"; do
  case "$a" in
    --i3) I3=1 ;;
    --polybar) POLYBAR=1 ;;
    --from-source) SOURCE=1 ;;
    --no-start) START=0 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,19p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

backup() { [ -f "$1" ] && cp "$1" "$1.agentop-bak" && say "backup: $1.agentop-bak"; return 0; }

# remove a marker block from a file
strip_block() {
  [ -f "$1" ] || return 0
  grep -qF "$BEGIN" "$1" || return 0
  awk -v b="$BEGIN" -v e="$END" '$0==b{skip=1;next} $0==e{skip=0;next} !skip' "$1" >"$1.tmp" && mv "$1.tmp" "$1"
}

stop_tray() {
  for p in $(pgrep -x agentop 2>/dev/null || true); do
    cmd=$( (tr '\0' ' ' <"/proc/$p/cmdline") 2>/dev/null || true)
    case "$cmd" in *" tray"*) kill "$p" 2>/dev/null || true ;; esac
  done
}

find_i3_config() {
  for f in "$CONF/i3/config" "$HOME/.i3/config"; do [ -f "$f" ] && { echo "$f"; return; }; done
}
find_polybar_config() {
  for f in "$CONF/polybar/config.ini" "$CONF/polybar/config"; do [ -f "$f" ] && { echo "$f"; return; }; done
}

# ---------------------------------------------------------------- uninstall
if [ "$UNINSTALL" = 1 ]; then
  say "stopping agentop"
  stop_tray
  rm -f "$BIN" "$APP_FILE" "$ICON_FILE" "$AUTOSTART_FILE"
  if f=$(find_i3_config) && [ -n "$f" ] && grep -q agentop "$f"; then
    backup "$f"; strip_block "$f"
    sed -i 's#[[:space:]]*|[[:space:]]*[^|]*agentop i3bar##' "$f"
    have i3-msg && i3-msg -q reload 2>/dev/null || true
    say "removed from $f"
  fi
  if f=$(find_polybar_config) && [ -n "$f" ] && grep -q agentop "$f"; then
    backup "$f"; strip_block "$f"
    sed -i -E '/^modules-(left|center|right)/ s/ ?\bagentop\b//' "$f"
    have polybar-msg && polybar-msg cmd restart >/dev/null 2>&1 || true
    say "removed from $f"
  fi
  say "agentop uninstalled"
  exit 0
fi

# ---------------------------------------------------------------- binary
[ "$(uname -s)" = Linux ] || die "agentop supports Linux only"
mkdir -p "$BIN_DIR"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

in_tree=0
[ -f Cargo.toml ] && grep -q '^name = "agentop"' Cargo.toml && in_tree=1

if [ "$SOURCE" = 0 ] && [ "$in_tree" = 0 ]; then
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) arch=""; warn "no prebuilt binary for $(uname -m), building from source" ;;
  esac
  if [ -n "$arch" ]; then
    if [ "$VERSION" = latest ]; then
      url="https://github.com/$REPO/releases/latest/download/agentop-$arch-linux"
    else
      url="https://github.com/$REPO/releases/download/$VERSION/agentop-$arch-linux"
    fi
    say "downloading $url"
    if have curl; then curl -fsSL "$url" -o "$tmp/agentop" || rm -f "$tmp/agentop"
    elif have wget; then wget -qO "$tmp/agentop" "$url" || rm -f "$tmp/agentop"
    else die "need curl or wget"; fi
    [ -s "$tmp/agentop" ] || { warn "download failed, building from source"; SOURCE=1; }
  else
    SOURCE=1
  fi
fi

if [ "$in_tree" = 1 ] || [ "$SOURCE" = 1 ]; then
  have cargo || die "cargo not found — install Rust from https://rustup.rs or use a release binary"
  if [ "$in_tree" = 1 ]; then
    say "building from $(pwd)"
    cargo build --release --locked
    cp target/release/agentop "$tmp/agentop"
  else
    say "building $REPO with cargo"
    cargo install --locked --git "https://github.com/$REPO" agentop --root "$tmp/root"
    cp "$tmp/root/bin/agentop" "$tmp/agentop"
  fi
fi

chmod 755 "$tmp/agentop"
"$tmp/agentop" --version >/dev/null 2>&1 || die "downloaded binary does not run"
stop_tray
install -m 755 "$tmp/agentop" "$BIN"
say "installed $("$BIN" --version) → $BIN"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR is not in your PATH" ;; esac

# ---------------------------------------------------------------- desktop integration
mkdir -p "$(dirname "$APP_FILE")" "$(dirname "$ICON_FILE")" "$(dirname "$AUTOSTART_FILE")"
cat >"$ICON_FILE" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="14" fill="#0F141B"/>
  <circle cx="32" cy="32" r="20" fill="none" stroke="#4ADE80" stroke-width="5"/>
  <circle cx="24" cy="32" r="3.5" fill="#E6E6E6"/><circle cx="32" cy="32" r="3.5" fill="#E6E6E6"/>
  <circle cx="40" cy="32" r="3.5" fill="#E6E6E6"/>
</svg>
SVG
cat >"$APP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=agentop
Comment=See what your AI coding agents are doing
Exec=$BIN open
Icon=agentop
Terminal=false
Categories=System;Monitor;Development;
Keywords=ai;agent;claude;codex;gemini;opencode;monitor;
EOF
cat >"$AUTOSTART_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=agentop tray
Comment=AI agent monitor tray icon
Exec=$BIN tray
Icon=agentop
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
EOF
say "tray autostart: $AUTOSTART_FILE"

# ---------------------------------------------------------------- i3 (optional)
if [ "$I3" = 1 ]; then
  f=$(find_i3_config || true)
  if [ -z "$f" ]; then
    warn "--i3: no i3 config found, skipped"
  else
    backup "$f"; strip_block "$f"
    cat >>"$f" <<EOF
$BEGIN
for_window [class="^agentop\$"] floating enable, resize set 1400 600, move position center
exec --no-startup-id $BIN tray
$END
EOF
    # wrap the existing i3bar status_command (i3status | agentop i3bar)
    if ! grep -q 'agentop i3bar' "$f"; then
      sed -i -E "s#^([[:space:]]*status_command[[:space:]]+.*)\$#\1 | $BIN i3bar#" "$f"
    fi
    have i3-msg && i3-msg -q reload 2>/dev/null || true
    say "i3 configured: $f"
  fi
fi

# ---------------------------------------------------------------- polybar (optional)
if [ "$POLYBAR" = 1 ]; then
  f=$(find_polybar_config || true)
  if [ -z "$f" ]; then
    warn "--polybar: no polybar config found, skipped"
  else
    backup "$f"; strip_block "$f"
    cat >>"$f" <<EOF
$BEGIN
[module/agentop]
type = custom/script
exec = $BIN polybar
tail = true
click-left = $BIN open
format-padding = 1
$END
EOF
    if ! grep -qE '^modules-(left|center|right).*\bagentop\b' "$f"; then
      # before the tray module if there is one, otherwise at the end
      if grep -qE '^modules-right.*\btray\b' "$f"; then
        sed -i -E '/^modules-right/ s/\btray\b/agentop tray/' "$f"
      else
        sed -i -E 's/^(modules-right[[:space:]]*=.*[^[:space:]])[[:space:]]*$/\1 agentop/' "$f"
      fi
    fi
    have polybar-msg && polybar-msg cmd restart >/dev/null 2>&1 || true
    say "polybar configured: $f"
  fi
fi

# ---------------------------------------------------------------- start
if [ "$START" = 1 ] && { [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; }; then
  nohup "$BIN" tray >/dev/null 2>&1 &
  say "tray started"
fi

say "done — run 'agentop' for the dashboard, 'agentop --help' for everything else"
