#!/bin/sh
# Assembles Hydra.app from an already-built release.
#
#   cargo build --release
#   packaging/macos/build-app.sh
set -eu

version=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
app="Hydra.app"
[ -x target/release/hdmd ] || { echo "Build first: cargo build --release" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

for binary in hdmd hdm hdm-host; do
  install -m 755 "target/release/$binary" "$app/Contents/MacOS/$binary"
done
cp -r ui "$app/Contents/Resources/ui"
cp -r python "$app/Contents/Resources/python"
find "$app/Contents/Resources" -name '__pycache__' -type d -exec rm -rf {} + 2>/dev/null || true
rm -rf "$app/Contents/Resources/python/tests"

# The bundle's entry point: start the daemon if it is not already running, then
# open the interface. Double-clicking should do the obvious thing.
cat > "$app/Contents/MacOS/Hydra" <<'LAUNCHER'
#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
resources="$here/../Resources"
data="${HYDRA_DATA_DIR:-$HOME/Library/Application Support/Hydra}"

# hdmd looks for ui/ and python/ next to itself; in a bundle they are one
# directory across, so point at them explicitly.
if [ ! -f "$data/daemon.json" ]; then
  HYDRA_PLUGIN_DIR="$resources/python" "$here/hdmd" --ui "$resources/ui" >/dev/null 2>&1 &
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -f "$data/daemon.json" ] && break
    sleep 0.3
  done
fi

url=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["url"])' "$data/daemon.json" 2>/dev/null || echo "")
if [ -z "$url" ]; then
  osascript -e 'display alert "Hydra could not start" message "Run hdmd in a terminal to see why."' || true
  exit 1
fi
exec open "$url"
LAUNCHER
chmod +x "$app/Contents/MacOS/Hydra"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Hydra</string>
  <key>CFBundleDisplayName</key><string>Hydra Download Manager</string>
  <key>CFBundleIdentifier</key><string>org.hydradm.app</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>Hydra</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <!-- The interface lives in the browser; the bundle has no window of its own. -->
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "Built $app"
echo "It is unsigned, so the first launch needs: right-click, Open, confirm."
