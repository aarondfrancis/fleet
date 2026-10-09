# Read-only host configuration. Run in a subshell so runner probing is unaffected.
export LC_ALL=C
fact() { printf 'RFM\tdiag:%s\t%s\n' "$1" "$(printf '%s' "$2" | tr '\t\r\n' '   ' | cut -c 1-1000)"; }
fact identity "$(id -un 2>/dev/null)"
fact architecture "$(uname -m 2>/dev/null)"
os="$(uname -s)"
ts="$(command -v tailscale 2>/dev/null)"
if [ -z "$ts" ]; then
  for candidate in /opt/homebrew/bin/tailscale /usr/local/bin/tailscale /usr/bin/tailscale /Applications/Tailscale.app/Contents/MacOS/Tailscale; do
    if [ -x "$candidate" ]; then ts="$candidate"; break; fi
  done
fi
fact tailscale_cli "$ts"
if [ -n "$ts" ]; then
  # Read only selected fields. Never emit the complete preferences or state.
  prefs="$(TAILSCALE_BE_CLI=1 "$ts" debug prefs 2>/dev/null)"
  fact tailscale_ssh "$(printf '%s\n' "$prefs" | sed -En 's/^[[:space:]]*"RunSSH":[[:space:]]*(true|false).*/\1/p')"
  status="$(TAILSCALE_BE_CLI=1 "$ts" status --json 2>/dev/null)"
  fact tailscale_version "$(printf '%s\n' "$status" | sed -n 's/^  "Version": "\([^"]*\)".*/\1/p')"
  fact tailscale_backend "$(printf '%s\n' "$status" | sed -n 's/^  "BackendState": "\([^"]*\)".*/\1/p')"
fi
case "$os" in
  Darwin)
    # An installed CLI may be a symlink into the GUI. Check the running daemon too.
    if ps -axo comm= 2>/dev/null | awk '/(^|\/)tailscaled$/ {found=1} END {exit !found}'; then
      fact tailscale_install cli-daemon
    elif [ -d /Applications/Tailscale.app ]; then
      if [ -f /Applications/Tailscale.app/Contents/_MASReceipt/receipt ]; then
        fact tailscale_install mac-app-store
      else
        fact tailscale_install mac-standalone
      fi
    elif [ -n "$ts" ]; then
      fact tailscale_install cli
    fi
    power="$(pmset -g custom 2>/dev/null)"
    for setting in sleep displaysleep autorestart womp; do
      fact "ac_$setting" "$(printf '%s\n' "$power" | awk -v key="$setting" '/^[^[:space:]]/ {ac=($0 ~ /^AC Power:/)} ac && $1==key {print $2; exit}')"
    done
    fact battery_sleep "$(printf '%s\n' "$power" | awk '/^[^[:space:]]/ {battery=($0 ~ /^Battery Power:/)} battery && $1=="sleep" {print $2; exit}')"
    fact console_user "$(stat -f '%Su' /dev/console 2>/dev/null)"
    ;;
  Linux)
    if [ -n "$ts" ]; then fact tailscale_install cli-daemon; fi
    if command -v systemctl >/dev/null 2>&1; then
      fact tailscale_startup "$(systemctl is-enabled tailscaled.service 2>/dev/null)"
      for target in sleep suspend hibernate hybrid-sleep suspend-then-hibernate; do
        fact "target_$target" "$(systemctl is-enabled "$target.target" 2>/dev/null)"
      done
    fi
    # cat-config includes drop-ins in precedence order. Ignore is logind's default.
    if logind="$(systemd-analyze cat-config systemd/logind.conf 2>/dev/null)"; then
      fact idle_action "$(printf '%s\n' "$logind" | awk -F= 'BEGIN {action="ignore"} /^[[:space:]]*IdleAction[[:space:]]*=/ {action=$2; gsub(/[[:space:]]/,"",action)} END {print action}')"
    fi
    ;;
esac
fact complete 1
