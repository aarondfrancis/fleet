#!/bin/sh
# Builds Runner Dashboard and installs it as /Applications/Runner Dashboard.app.
#
#   sh scripts/install-app.sh
#
# Env overrides: INSTALL_DIR (default /Applications), SKIP_BUILD=1 to install
# the last build without rebuilding.
#
# The new copy is staged next to the destination, then swapped in with two
# renames, so a failed build or copy leaves the installed app untouched.
# Pasted GitHub tokens and settings live in the Keychain and
# ~/Library/Application Support, not in the app, so they survive reinstalls.
set -eu

cd "$(dirname "$0")/.."
APP="Runner Dashboard.app"
DEST_DIR="${INSTALL_DIR:-/Applications}"
BUILT="target/release/bundle/macos/$APP"

if [ "${SKIP_BUILD:-0}" != 1 ]; then
  pnpm install --frozen-lockfile
  pnpm tauri build
fi
[ -d "$BUILT" ] || { echo "error: $BUILT wasn't built" >&2; exit 1; }
codesign --verify --deep --strict "$BUILT"

if [ ! -w "$DEST_DIR" ]; then
  echo "error: $DEST_DIR isn't writable by $(id -un)" >&2
  exit 1
fi

# Quit a running copy so the swap doesn't pull files out from under it.
if osascript -e 'application id "dev.runnerdashboard.app" is running' 2>/dev/null | grep -q true; then
  echo "Quitting Runner Dashboard…"
  osascript -e 'tell application id "dev.runnerdashboard.app" to quit' || true
  i=0
  while osascript -e 'application id "dev.runnerdashboard.app" is running' 2>/dev/null | grep -q true; do
    i=$((i + 1))
    [ $i -gt 20 ] && { echo "error: Runner Dashboard didn't quit" >&2; exit 1; }
    sleep 0.5
  done
fi

staged="$DEST_DIR/.$APP.installing"
old="$DEST_DIR/.$APP.previous"
rm -rf "$staged" "$old"
ditto "$BUILT" "$staged"
codesign --verify --deep --strict "$staged"
if [ -d "$DEST_DIR/$APP" ]; then
  mv "$DEST_DIR/$APP" "$old"
fi
mv "$staged" "$DEST_DIR/$APP"
rm -rf "$old"

version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$DEST_DIR/$APP/Contents/Info.plist")
echo "Installed Runner Dashboard $version at $DEST_DIR/$APP"
echo "Note: it's ad-hoc signed (no Developer ID), so after an update macOS may ask once to let it read the GitHub tokens you pasted into Settings."
