#!/bin/sh
# The macOS app, built on a Mac for Intel and Apple silicon in one program, into OUT:
#   scripts/macos.sh OUT
# OUT gets "SSH Remote.app" and sshremote-macos.zip (what scripts/deploy.sh takes as $MAC_ZIP and
# the program's update downloads). The app is signed with the certificate and key in $SIGN_DIR
# (cert.pem, key.pem; by default ~/dev/sign) through rcodesign, so that macOS keeps what the
# person allowed it (recording the screen, control) from one version to the next; without them
# it is signed ad hoc, and each version is asked about again.
# Needs Xcode's command line tools, rustup, CMake (Opus and libaom are built with it) and NASM
# (libaom's x86 code) on PATH.
set -eu
OUT=$(mkdir -p "$1" && cd "$1" && pwd)
cd "$(dirname "$0")/.."
SIGN_DIR=${SIGN_DIR:-$HOME/dev/sign}
export MACOSX_DEPLOYMENT_TARGET=12.3
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)

for t in x86_64-apple-darwin aarch64-apple-darwin; do
  rustup target add "$t" > /dev/null 2>&1 || true
  if ! cargo build --profile dist --target "$t" > "$OUT/build-$t.log" 2>&1; then
    echo "FAILED $t: see $OUT/build-$t.log"
    exit 1
  fi
done

app="$OUT/SSH Remote.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/sshremote" target/x86_64-apple-darwin/dist/sshremote target/aarch64-apple-darwin/dist/sshremote

# The icon is drawn once and kept until its drawing changes.
icons=target/macos-icon-$(shasum assets/make_icon.py | cut -c1-12)
if [ ! -f "$icons/sshremote.icns" ]; then
  rm -rf "$icons" && mkdir -p "$icons"
  python3 assets/make_icon.py iconset "$icons/sshremote.iconset"
  iconutil -c icns "$icons/sshremote.iconset" -o "$icons/sshremote.icns"
fi
cp "$icons/sshremote.icns" "$app/Contents/Resources/sshremote.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>SSH Remote</string>
<key>CFBundleDisplayName</key><string>SSH Remote</string>
<key>CFBundleIdentifier</key><string>dev.sshremote.app</string>
<key>CFBundleExecutable</key><string>sshremote</string>
<key>CFBundleIconFile</key><string>sshremote</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>12.3</string>
<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSMicrophoneUsageDescription</key><string>Calls with the person at the other computer, when you start one.</string>
<key>NSCameraUsageDescription</key><string>Showing this Mac's camera to a viewer, when remote access allows it.</string>
<key>NSAppleEventsUsageDescription</key><string>Restarting this Mac when a viewer you let in asks for it.</string>
</dict></plist>
PLIST

if [ -f "$SIGN_DIR/cert.pem" ] && [ -f "$SIGN_DIR/key.pem" ] && command -v rcodesign > /dev/null; then
  rcodesign sign --pem-file "$SIGN_DIR/cert.pem" --pem-file "$SIGN_DIR/key.pem" "$app" > "$OUT/sign.log" 2>&1 || { echo "FAILED signing: see $OUT/sign.log"; exit 1; }
else
  codesign --force --deep -s - "$app"
fi

rm -f "$OUT/sshremote-macos.zip"
(cd "$OUT" && ditto -c -k --norsrc --keepParent "SSH Remote.app" sshremote-macos.zip)
echo "built macOS $version in $OUT"
