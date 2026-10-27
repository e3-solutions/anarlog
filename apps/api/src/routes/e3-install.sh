#!/bin/bash
set -euo pipefail

RELEASES="https://rwefimqhinohocgmtwjj.supabase.co/storage/v1/object/public/e3-scribe-releases"
GUIDE="https://e3-scribe-setup.up.railway.app/download"
APP="E3 Scribe.app"
IDENTIFIER="ai.e3group.meeting-capture-shared-preview"

fail() { printf 'Error: %s\n' "$*" >&2; exit 1; }
finish() {
  printf 'Ready: %s\nSetup guide: %s\n' "$1" "$GUIDE"
  printf 'Sign in with E3, allow Mac permissions, then connect both Google Meet and Slack.\n'
  if [ "${E3_SCRIBE_NO_OPEN:-0}" != 1 ]; then
    /usr/bin/open "$GUIDE"
    /usr/bin/open "$1"
  fi
}

[ "$(uname -s)" = Darwin ] || fail "E3 Scribe is only available for macOS."
[ "$(/usr/sbin/sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ] || fail "E3 Scribe requires an Apple Silicon (M1 or newer) Mac."
macos=$(/usr/bin/sw_vers -productVersion)
[ "${macos%%.*}" -ge 15 ] || fail "E3 Scribe requires macOS 15 or newer."

# Existing installs use their signed updater; never interrupt an active recording.
if [ -n "${E3_SCRIBE_INSTALL_DIR:-}" ]; then
  destinations=("$E3_SCRIBE_INSTALL_DIR")
else
  destinations=("/Applications" "$HOME/Applications")
fi
for directory in "${destinations[@]}"; do
  if [ -e "$directory/$APP" ]; then
    installed=$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$directory/$APP/Contents/Info.plist")
    [ "$installed" = "$IDENTIFIER" ] || fail "A different app already occupies $directory/$APP. Nothing was replaced."
    printf 'E3 Scribe is already installed. Future updates arrive through the app.\n'
    finish "$directory/$APP"
    exit 0
  fi
done

DEST="${E3_SCRIBE_INSTALL_DIR:-/Applications}"
if [ -z "${E3_SCRIBE_INSTALL_DIR:-}" ] && [ ! -w "$DEST" ]; then
  DEST="$HOME/Applications"
fi
mkdir -p "$DEST"
[ -w "$DEST" ] || fail "Cannot write to $DEST. Install in your own Applications folder instead."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
  "$RELEASES/update/darwin-aarch64.json" -o "$tmp/release.json"
version=$(/usr/bin/plutil -extract version raw -o - "$tmp/release.json")
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Invalid release version."
url=$(/usr/bin/plutil -extract url raw -o - "$tmp/release.json")
[ "$url" = "$RELEASES/releases/$version/E3-Scribe.app.tar.gz" ] || fail "Unexpected release download URL."

printf 'Downloading E3 Scribe %s…\n' "$version"
curl --fail --show-error --location --progress-bar --proto '=https' --proto-redir '=https' \
  "$url" -o "$tmp/app.tar.gz"
tar -tzf "$tmp/app.tar.gz" > "$tmp/files"
/usr/bin/awk '
  $0 != "E3 Scribe.app" && index($0, "E3 Scribe.app/") != 1 { exit 1 }
  /(^|\/)\.\.(\/|$)/ { exit 1 }
' "$tmp/files" || fail "Unexpected archive contents."
tar -xzf "$tmp/app.tar.gz" -C "$tmp"

printf 'Verifying Apple signature and notarization…\n'
/usr/bin/codesign --verify --deep --strict \
  -R '=anchor apple generic and certificate leaf[subject.OU] = "4RA2H59VML" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists' \
  "$tmp/$APP" || fail "App signature verification failed. Nothing was installed."
/usr/sbin/spctl --assess --type execute "$tmp/$APP" || fail "Apple notarization check failed. Nothing was installed."
[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$tmp/$APP/Contents/Info.plist")" = "$IDENTIFIER" ] || fail "Unexpected app identity."
[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$tmp/$APP/Contents/Info.plist")" = "$version" ] || fail "App version does not match the release."

# Do not overwrite another install if two installers ran at the same time.
[ ! -e "$DEST/$APP" ] || fail "E3 Scribe was installed while downloading. Run this command again to open it."
mv -n "$tmp/$APP" "$DEST/"
[ ! -e "$tmp/$APP" ] || fail "Another installation appeared. Nothing was replaced; run the command again."
finish "$DEST/$APP"
