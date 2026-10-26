#!/bin/sh
# Build a release tarball: dist/vivobook-fancontrol-<version>-x86_64-linux.tar.gz
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
name=vivobook-fancontrol-v$version-x86_64-linux
cargo build --release --locked
rm -rf "dist/$name" && mkdir -p "dist/$name/bin"
for b in vfc vfcd vfc-gui fan-control; do cp "target/release/$b" "dist/$name/bin/"; done
cp -r install.sh uninstall.sh packaging extras README.md NOTES.md CHANGELOG.md LICENSE "dist/$name/"
tar -C dist -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
