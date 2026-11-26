#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Build ERPForAll.app for this checkout with osacompile (macOS only; no Xcode needed).
#   scripts/macos/build-mac-app.sh [--dest DIR]    (default DIR: ~/Applications)
# The app calls erpforall-ctl in this checkout by absolute path, so run this again after moving the checkout.
# Building on your own Mac keeps the app free of the download quarantine, so Gatekeeper does not block it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CTL="$HERE/erpforall-ctl"
SRC="$HERE/ERPForAll.applescript"
DEST_DIR="$HOME/Applications"

die() { printf 'build-mac-app: %s\n' "$*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dest) if [ "$#" -lt 2 ] || [ -z "$2" ]; then die "--dest needs a directory"; fi; DEST_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '3,6p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown flag $1 (try --help)" ;;
  esac
done

[ "$(uname -s)" = Darwin ] || die "this builds a macOS app; run it on a Mac"
command -v osacompile >/dev/null 2>&1 || die "osacompile was not found (it ships with macOS in /usr/bin)"
if [ ! -f "$SRC" ] || [ ! -f "$CTL" ]; then die "run this from a full checkout ($SRC or $CTL is missing)"; fi
# The path is pasted into an AppleScript string and a sed replacement; scripts/install.sh refuses spaces too.
case "$CTL" in *'"'*|*\\*|*'|'*|*'&'*|*[[:space:]]*) die "the checkout path must not contain spaces, quotes, backslashes, | or &: $CTL" ;; esac

APP="$DEST_DIR/ERPForAll.app"
if [ -e "$APP" ]; then
  [ -f "$APP/Contents/Resources/Scripts/main.scpt" ] || die "$APP exists and is not an app this script built; move it away first"
  rm -rf "$APP"
fi
mkdir -p "$DEST_DIR"
chmod +x "$CTL"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
sed "s|@@ERPFORALL_CTL@@|$CTL|" "$SRC" > "$tmp/ERPForAll.applescript"
grep -q '@@ERPFORALL_CTL@@' "$tmp/ERPForAll.applescript" && die "the placeholder was not replaced"
osacompile -o "$APP" "$tmp/ERPForAll.applescript"

printf 'Built %s\n  It controls the checkout at %s\n  Open it from Finder (Applications in your home folder) or with: open "%s"\n' \
  "$APP" "$(cd "$HERE/../.." && pwd)" "$APP"
