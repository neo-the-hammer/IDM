#!/bin/sh
# Builds a .deb from an already-built release.
#
#   cargo build --release
#   packaging/linux/build-deb.sh
#
# Uses dpkg-deb when it is available and falls back to assembling the archive
# with ar and tar, so the package can be built on a machine that has no Debian
# tooling at all.
set -eu

version=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
arch=$(uname -m)
case "$arch" in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
esac

root=$(pwd)
[ -x "$root/target/release/hdmd" ] || { echo "Build first: cargo build --release" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
pkg="$stage/hydra"

mkdir -p "$pkg/DEBIAN" \
         "$pkg/usr/bin" \
         "$pkg/usr/share/hydra" \
         "$pkg/usr/share/applications" \
         "$pkg/usr/share/doc/hydra"

install -m 755 target/release/hdmd "$pkg/usr/bin/hdmd"
install -m 755 target/release/hdm "$pkg/usr/bin/hdm"
install -m 755 target/release/hdm-host "$pkg/usr/bin/hdm-host"
install -m 755 packaging/linux/hydra-open "$pkg/usr/bin/hydra-open"
cp -r ui "$pkg/usr/share/hydra/ui"
cp -r python "$pkg/usr/share/hydra/python"
# Bytecode caches are build artefacts of whoever ran the tests, not content.
find "$pkg/usr/share/hydra" -name '__pycache__' -type d -exec rm -rf {} + 2>/dev/null || true
find "$pkg/usr/share/hydra" -name '*.pyc' -delete 2>/dev/null || true
rm -rf "$pkg/usr/share/hydra/python/tests"
install -m 644 packaging/linux/hydra.desktop "$pkg/usr/share/applications/hydra.desktop"
install -m 644 README.md "$pkg/usr/share/doc/hydra/README.md"

cat > "$pkg/DEBIAN/control" <<CONTROL
Package: hydra-download-manager
Version: $version
Section: net
Priority: optional
Architecture: $arch
Maintainer: Hydra Download Manager contributors <noreply@example.invalid>
Depends: libc6
Recommends: libssl3 | libssl1.1, python3
Suggests: yt-dlp
Homepage: https://github.com/neo-the-hammer/IDM
Description: Segmented download manager with queues and scheduling
 Hydra downloads files over many connections at once, resumes interrupted
 transfers, and organises them into queues that can run on a schedule.
 .
 It has no third-party dependencies: TLS comes from the OpenSSL the system
 already provides, loaded at run time, so the package works whether that is
 libssl3 or libssl1.1 and needs neither at build time.
 .
 python3 is recommended: it powers the site grabber and media extraction.
 Everything else works without it.
CONTROL

# Sizes are in kilobytes, and dpkg wants the installed size, not the archive's.
echo "Installed-Size: $(du -sk "$pkg" | cut -f1)" >> "$pkg/DEBIAN/control"

output="$root/hydra-download-manager_${version}_${arch}.deb"

if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --build --root-owner-group "$pkg" "$output" >/dev/null
else
  # A .deb is an ar archive of three members, in this order.
  echo "dpkg-deb not found; assembling with ar and tar"
  ( cd "$pkg" && echo "2.0" > "$stage/debian-binary" \
    && tar czf "$stage/control.tar.gz" -C "$pkg/DEBIAN" . \
    && tar czf "$stage/data.tar.gz" --exclude=./DEBIAN -C "$pkg" . )
  ( cd "$stage" && ar rc "$output" debian-binary control.tar.gz data.tar.gz )
fi

echo "Built $output"
echo "Install with: sudo apt install ./$(basename "$output")"
