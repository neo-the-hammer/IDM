#!/bin/sh
# Generates the Firefox build from the Chromium one.
#
# The two differ only in the manifest: Firefox needs an explicit add-on id for
# native messaging to work, and runs MV3 background code as scripts rather than
# a service worker. Everything else is shared, so chromium/ is the source of
# truth and firefox/ is derived.
set -eu
here=$(dirname "$0")
cd "$here"

rm -rf firefox
mkdir -p firefox/icons
cp chromium/*.js chromium/*.html chromium/*.css firefox/
cp chromium/icons/*.png firefox/icons/
cp firefox-manifest.json firefox/manifest.json

echo "Built extensions/firefox from extensions/chromium"
