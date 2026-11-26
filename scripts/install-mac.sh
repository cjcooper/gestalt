#!/usr/bin/env bash
# Installs a built Gestalt.app into /Applications so Finder, Launchpad and the
# Dock show its new icon, without clearing the system icon caches.
#
#   scripts/install-mac.sh                     newest src-tauri/target/release/bundle/macos/*.app
#   scripts/install-mac.sh path/to/Gestalt.app
#   scripts/install-mac.sh --dest ~/Applications
#   scripts/install-mac.sh --no-dock           don't restart the Dock
#
# Why copying over the old app isn't enough: `ditto` merges into the existing
# bundle and keeps the build's modification time, so the bundle at
# /Applications/Gestalt.app has the same path, inode and (between releases)
# version as before. LaunchServices and the icon cache see nothing new and
# keep the old icon. So this script:
#   1. quits Gestalt if it is running from the destination (and reopens it
#      afterwards), since a running app's files are being replaced;
#   2. copies the new bundle next to the old one, then swaps it in, so the
#      destination is a new directory with no stale files from the old build;
#   3. touches the bundle, so its modification time is now;
#   4. re-registers it with LaunchServices (`lsregister -f`), which refreshes
#      its icon in Finder and Launchpad;
#   5. restarts the Dock, only if Gestalt is kept in it: the Dock keeps its
#      own copy of a pinned app's icon. Restarting it takes a second and
#      loses nothing.
# It uses sudo only when the destination folder isn't writable.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

die() { echo "error: $*" >&2; exit 1; }

[[ "$(uname -s)" == Darwin ]] || die "macOS only"

src=""
dest_dir=/Applications
restart_dock=1
while (( $# )); do
  case "$1" in
    --dest)
      [[ $# -ge 2 ]] || die "--dest needs a folder"
      dest_dir="$2"; shift ;;
    --dest=*) dest_dir="${1#--dest=}" ;;
    --no-dock) restart_dock=0 ;;
    -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
    -*) die "unknown option $1" ;;
    *) [[ -z "$src" ]] || die "only one app to install"; src="$1" ;;
  esac
  shift
done

if [[ -z "$src" ]]; then
  src="$(ls -dt "$root"/src-tauri/target/release/bundle/macos/*.app 2>/dev/null | head -1 || true)"
  [[ -n "$src" ]] || die "no app in src-tauri/target/release/bundle/macos; build one, or pass its path"
fi
src="${src%/}"
[[ -d "$src" && -f "$src/Contents/Info.plist" ]] || die "$src is not an app bundle"
[[ -d "$dest_dir" ]] || die "$dest_dir is not a folder"

name="$(basename "$src")"
dest="${dest_dir%/}/$name"
stage="${dest_dir%/}/.$name.installing"
bundle_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$src/Contents/Info.plist")"
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$src/Contents/Info.plist" 2>/dev/null || echo '?')"

[[ "$(cd "$src" && pwd -P)" != "$(cd "$dest_dir" && pwd -P)/$name" ]] || die "$src is already installed there"

use_sudo=0
if [[ ! -w "$dest_dir" || ( -e "$dest" && ! -w "$dest" ) ]]; then
  use_sudo=1
  echo "$dest_dir isn't writable by you; using sudo"
fi
as_admin() { if (( use_sudo )); then sudo "$@"; else "$@"; fi; }

# --- quit the installed copy if it's running --------------------------------
running() { pgrep -f "^$dest/Contents/MacOS/" >/dev/null 2>&1; }
was_running=0
if [[ -d "$dest" ]] && running; then
  was_running=1
  echo "Quitting ${name}…"
  osascript -e "tell application id \"$bundle_id\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 50); do running || break; sleep 0.2; done
  ! running || die "$name is still running; quit it and try again"
fi

# --- copy, then swap --------------------------------------------------------
echo "Installing $name $version into ${dest_dir}…"
as_admin rm -rf "$stage"
as_admin ditto "$src" "$stage"
as_admin rm -rf "$dest"
as_admin mv "$stage" "$dest"
as_admin touch "$dest"

# --- tell macOS about the new icon ------------------------------------------
"$lsregister" -f -R -trusted "$dest"

if (( restart_dock )) \
  && defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "\"$bundle_id\""; then
  echo "Restarting the Dock to refresh its icon…"
  killall Dock 2>/dev/null || true
fi

if (( was_running )); then
  open "$dest"
fi
echo "Installed $dest"
