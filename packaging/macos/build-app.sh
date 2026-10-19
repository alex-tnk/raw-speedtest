#!/bin/sh
# Build "Raw Speed Test.app" in target/release/.
#
#   packaging/macos/build-app.sh            native architecture
#   packaging/macos/build-app.sh universal  arm64 + x86_64 (needs both
#                                           rustup targets installed)
set -eu

cd "$(dirname "$0")/../.."
name="Raw Speed Test"
bin="raw-speed-test"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
out="target/release/$name.app"

if [ "${1:-}" = "universal" ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    mkdir -p target/release
    lipo -create -output "target/release/$bin-universal" \
        "target/aarch64-apple-darwin/release/$bin" \
        "target/x86_64-apple-darwin/release/$bin"
    exe="target/release/$bin-universal"
else
    cargo build --release
    exe="target/release/$bin"
fi

rm -rf "$out"
mkdir -p "$out/Contents/MacOS" "$out/Contents/Resources"
# The executable carries the app name: macOS titles the app menu
# (About, Hide, Quit) with the process name.
cp "$exe" "$out/Contents/MacOS/$name"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$out/Contents/Info.plist"
cp packaging/macos/AppIcon.icns "$out/Contents/Resources/AppIcon.icns"
# Ad-hoc signature so the bundle (Info.plist included) is sealed.
codesign --force --sign - "$out" 2>/dev/null
echo "$out"
