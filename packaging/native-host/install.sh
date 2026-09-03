#!/bin/sh
# Registers Hydra's native messaging host with the browsers on this machine.
#
# The host exists so the extension can find Hydra's port and access token by
# itself. Without it the extension still works, but the token has to be pasted
# into its options page by hand.
#
# Usage: install.sh [/path/to/hdm-host] [chrome-extension-id ...]
set -eu

host_binary=${1:-}
if [ -z "$host_binary" ]; then
  for candidate in \
    "$(dirname "$0")/../../target/release/hdm-host" \
    "$(dirname "$0")/../../target/debug/hdm-host" \
    "$(command -v hdm-host || true)"
  do
    if [ -x "$candidate" ]; then host_binary=$candidate; break; fi
  done
fi
if [ ! -x "$host_binary" ]; then
  echo "error: could not find hdm-host. Build it with 'cargo build --release'," >&2
  echo "       or pass its path: install.sh /path/to/hdm-host" >&2
  exit 1
fi
host_binary=$(cd "$(dirname "$host_binary")" && pwd)/$(basename "$host_binary")
shift 2>/dev/null || true

# Chromium identifies callers by extension id, so an unpacked extension's id
# has to be supplied. Firefox identifies them by the add-on id in its manifest.
chrome_ids="$*"
firefox_id="hydra@hydradm.org"

chrome_origins=""
for id in $chrome_ids; do
  chrome_origins="$chrome_origins\"chrome-extension://$id/\","
done
chrome_origins=${chrome_origins%,}

write_manifest() {
  target_dir=$1
  body=$2
  mkdir -p "$target_dir"
  printf '%s\n' "$body" > "$target_dir/com.hydradm.host.json"
  echo "  wrote $target_dir/com.hydradm.host.json"
}

chrome_manifest="{
  \"name\": \"com.hydradm.host\",
  \"description\": \"Hydra Download Manager native host\",
  \"path\": \"$host_binary\",
  \"type\": \"stdio\",
  \"allowed_origins\": [$chrome_origins]
}"

firefox_manifest="{
  \"name\": \"com.hydradm.host\",
  \"description\": \"Hydra Download Manager native host\",
  \"path\": \"$host_binary\",
  \"type\": \"stdio\",
  \"allowed_extensions\": [\"$firefox_id\"]
}"

echo "Registering $host_binary"

case "$(uname -s)" in
  Darwin)
    base="$HOME/Library/Application Support"
    chrome_dirs="$base/Google/Chrome/NativeMessagingHosts
$base/Chromium/NativeMessagingHosts
$base/Microsoft Edge/NativeMessagingHosts
$base/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    firefox_dirs="$base/Mozilla/NativeMessagingHosts"
    ;;
  *)
    chrome_dirs="$HOME/.config/google-chrome/NativeMessagingHosts
$HOME/.config/chromium/NativeMessagingHosts
$HOME/.config/microsoft-edge/NativeMessagingHosts
$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    firefox_dirs="$HOME/.mozilla/native-messaging-hosts"
    ;;
esac

if [ -n "$chrome_origins" ]; then
  echo "$chrome_dirs" | while IFS= read -r dir; do
    [ -n "$dir" ] && write_manifest "$dir" "$chrome_manifest"
  done
else
  echo "  (no Chromium extension ids given; skipping Chromium)"
  echo "  Load the extension, copy its id from the extensions page, then re-run:"
  echo "    $0 \"$host_binary\" <extension-id>"
fi

echo "$firefox_dirs" | while IFS= read -r dir; do
  [ -n "$dir" ] && write_manifest "$dir" "$firefox_manifest"
done

echo "Done. Restart the browser for it to notice."
