#!/usr/bin/env bash
# Guarded retirement/decommission: stop/uninstall one exact service and
# deregister its exact runner directory. The workspace is deliberately preserved.
# The controller prepends RF_* values and run_as_root (run-as-root.sh).
set -euo pipefail

: "${RF_ROOT:?}"
: "${RF_NAME:?}"
: "${RF_TOKEN:?}"
: "${RF_SERVICE_MODE:?}"

case "$RF_ROOT" in
  "$HOME"/*|/*) ;;
  *) echo "refusing unresolved retirement path: $RF_ROOT" >&2; exit 2 ;;
esac
test -f "$RF_ROOT/.runner" || { echo "no configured runner at exact path $RF_ROOT" >&2; exit 3; }
tr -d '[:space:]' < "$RF_ROOT/.runner" | grep -F "\"agentName\":\"$RF_NAME\"" >/dev/null || {
  echo "runner name at exact path does not match $RF_NAME" >&2
  exit 3
}

cd "$RF_ROOT"
nixos_unit="runner-fleet-$RF_NAME.service"
if [ -f /etc/NIXOS ] && [ "$(cat .service 2>/dev/null || true)" = "$nixos_unit" ]; then
  # Generated NixOS module: stop and remove it while .runner still proves the
  # exact identity, then deregister.
  : "${RF_NIXOS_SERVICE_HELPER:?}"
  run_as_root systemctl stop "$nixos_unit"
  run_as_root python3 -c "$RF_NIXOS_SERVICE_HELPER" uninstall "$RF_ROOT" "$RF_NAME" "$(id -un)"
  ./config.sh remove --token "$RF_TOKEN"
  unset RF_TOKEN
  unset RF_SUDO_PASSWORD
  echo "retired $RF_NAME; preserved exact directory $RF_ROOT"
  exit 0
elif [ "$RF_SERVICE_MODE" = systemd ]; then
  run_as_root ./svc.sh stop
  run_as_root ./svc.sh uninstall
else
  ./svc.sh stop
  ./svc.sh uninstall
fi
./config.sh remove --token "$RF_TOKEN"
unset RF_TOKEN
unset RF_SUDO_PASSWORD
echo "retired $RF_NAME; preserved exact directory $RF_ROOT"
