#!/usr/bin/env bash
# Compare this repo's Rust port of Runner Fleet with the original Python CLI,
# using read-only commands only (inventory, status, plan, doctor). Neither side
# mints a token or changes anything.
#
#   scripts/fleet-parity.sh [command ...]
#
# RUNNER_FLEET_DIR is the Runner Fleet checkout (default
# ~/Code/soloterm/runner-fleet). RUNNER_DASHBOARD_INVENTORY overrides the
# inventory, as it does for the app. Needs python3.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OLD="${RUNNER_FLEET_DIR:-$HOME/Code/soloterm/runner-fleet}"
INVENTORY="${RUNNER_DASHBOARD_INVENTORY:-$ROOT/inventory/fleet.toml}"
COMMANDS=("$@")
[ ${#COMMANDS[@]} -gt 0 ] || COMMANDS=(inventory status plan doctor)

fail() { echo "error: $*" >&2; exit 2; }
[ -x "$OLD/bin/fleet" ] || fail "no Runner Fleet CLI at $OLD/bin/fleet; set RUNNER_FLEET_DIR to its checkout"
command -v python3 >/dev/null || fail "python3 is required to compare the outputs"
[ -f "$INVENTORY" ] || fail "no inventory at $INVENTORY; create it from inventory/fleet.example.toml or set RUNNER_DASHBOARD_INVENTORY"
for command in "${COMMANDS[@]}"; do
  case "$command" in
    inventory | status | plan | doctor) ;;
    *) fail "unsupported command '$command'; use inventory, status, plan or doctor" ;;
  esac
done

cargo build -q -p fleet --example fleet --manifest-path "$ROOT/Cargo.toml"
NEW="$ROOT/target/debug/examples/fleet"
TMP="${TMPDIR:-/tmp}"
OUT="$(mktemp -d "${TMP%/}/fleet-parity.XXXXXX")"

status=0
for command in "${COMMANDS[@]}"; do
  # Exit 1 is a valid result (doctor found a failing check); anything else, or
  # no JSON, means the command itself failed.
  old_exit=0 new_exit=0
  "$OLD/bin/fleet" --inventory "$INVENTORY" "$command" --json >"$OUT/old-$command.json" 2>"$OUT/old-$command.err" || old_exit=$?
  "$NEW" --inventory "$INVENTORY" "$command" --json >"$OUT/new-$command.json" 2>"$OUT/new-$command.err" || new_exit=$?
  failed=0
  for side in old new; do
    code=$([ "$side" = old ] && echo "$old_exit" || echo "$new_exit")
    if [ "$code" -gt 1 ] || ! python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$OUT/$side-$command.json" 2>/dev/null; then
      echo "✗ $command: $side side failed (exit $code)"
      sed 's/^/    /' "$OUT/$side-$command.err"
      failed=1
    fi
  done
  if [ $failed = 1 ]; then
    status=1
    continue
  fi
  if [ "$old_exit" != "$new_exit" ]; then
    echo "  note: exit status differs: old $old_exit, new $new_exit"
  fi
  if python3 - "$OUT/old-$command.json" "$OUT/new-$command.json" <<'PY' && [ "$old_exit" = "$new_exit" ]; then
import json, sys
import re
old, new = (json.load(open(p)) for p in sys.argv[1:3])
# Free disk space is live data that moves between the two runs.
def steady(rows):
    if isinstance(rows, list):
        for row in rows:
            if isinstance(row, dict) and row.get("check") == "disk":
                row["detail"] = re.sub(r"[0-9.]+", "N", row["detail"])
    return rows
old, new = steady(old), steady(new)
# Runners outside the inventory (e.g. ephemeral hosted pools) come and go
# between the two runs; compare those only by name, and only report churn.
def unmanaged(rows):
    if not isinstance(rows, list):
        return rows, set()
    kept = [r for r in rows if not (isinstance(r, dict) and r.get("drift") == "unmanaged")]
    return kept, {r["runner"] for r in rows if isinstance(r, dict) and r.get("drift") == "unmanaged"}
(old, old_extra), (new, new_extra) = unmanaged(old), unmanaged(new)
if old_extra != new_extra:
    print(f"  note: runners outside the inventory changed between runs: "
          f"-{sorted(old_extra - new_extra)} +{sorted(new_extra - old_extra)}")
if old == new:
    sys.exit(0)
if isinstance(old, list) and isinstance(new, list):
    for i in range(max(len(old), len(new))):
        a = old[i] if i < len(old) else None
        b = new[i] if i < len(new) else None
        if a != b:
            print(f"  row {i}:\n    old {json.dumps(a)}\n    new {json.dumps(b)}")
else:
    print(f"  old {json.dumps(old)}\n  new {json.dumps(new)}")
sys.exit(1)
PY
    echo "✓ $command: identical"
  else
    echo "✗ $command: differs (outputs in $OUT)"
    status=1
  fi
done
exit $status
