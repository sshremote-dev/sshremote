#!/bin/sh
# ci-package.sh KIND BINARY FILE ARCH: the Linux program BINARY as a package FILE of KIND (deb or
# rpm) for ARCH (x86_64 or aarch64): the program in /usr/bin, its desktop entry (as
# src/desktop_entry.rs writes it, but for /usr/bin) and its icon, the license and notices.
# KIND msi: the Windows program BINARY (x86_64) as an MSI, by release/sshremote.wxs and wixl.
set -eu
# What it installs: for its owner to change, for everyone to read.
umask 022
KIND=$1 BIN=$2 FILE=$3 ARCH=$4
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$KIND" = msi ]; then
  [ "$ARCH" = x86_64 ] || { echo "ci-package.sh: an MSI is made for x86_64 only" >&2; exit 1; }
  wixl -a x64 -D Version="$VERSION" -D Exe="$BIN" -o "$FILE" release/sshremote.wxs
  sha256sum "$FILE"
  exit 0
fi
WORK=$(mktemp -d)
ROOT="$WORK/root"
install -Dm755 "$BIN" "$ROOT/usr/bin/sshremote"
install -Dm644 assets/sshremote.png "$ROOT/usr/share/icons/hicolor/256x256/apps/sshremote.png"
install -Dm644 LICENSE "$ROOT/usr/share/doc/sshremote/copyright"
install -Dm644 THIRD-PARTY-NOTICES.txt "$ROOT/usr/share/doc/sshremote/THIRD-PARTY-NOTICES.txt"
mkdir -p "$ROOT/usr/share/applications"
cat > "$ROOT/usr/share/applications/sshremote.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SSH Remote
GenericName=Remote desktop and SSH
Comment=Split terminals, screens and files of your servers and computers
Exec=/usr/bin/sshremote %u
Icon=sshremote
Terminal=false
Categories=Network;RemoteAccess;System;
Keywords=ssh;remote;desktop;terminal;support;
StartupWMClass=sshremote
MimeType=x-scheme-handler/sshremote;
EOF
SUMMARY="Remote desktop and SSH workspace"
ABOUT="Split terminals, screens and files of your servers and computers, over SSH or by a computer's remote access ID."
case "$KIND" in
deb)
  case "$ARCH" in x86_64) DEBARCH=amd64 ;; aarch64) DEBARCH=arm64 ;; *) echo "no Debian name for $ARCH" >&2; exit 1 ;; esac
  mkdir -p "$ROOT/DEBIAN"
  cat > "$ROOT/DEBIAN/control" <<EOF
Package: sshremote
Version: $VERSION
Architecture: $DEBARCH
Maintainer: SSH Remote <registry@sshremote.dev>
Installed-Size: $(du -sk "$ROOT/usr" | cut -f1)
Depends: libc6 (>= 2.39), libstdc++6, libgcc-s1, libglib2.0-0t64 | libglib2.0-0, libasound2t64 | libasound2, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0
Recommends: curl, xdg-utils, openssh-client
Section: net
Priority: optional
Homepage: https://sshremote.dev
Description: $SUMMARY
 $ABOUT
EOF
  dpkg-deb --root-owner-group -Zxz --build "$ROOT" "$FILE" > /dev/null
  ;;
rpm)
  mkdir -p "$WORK/top/SPECS"
  cat > "$WORK/top/SPECS/sshremote.spec" <<EOF
Name: sshremote
Version: $VERSION
Release: 1
Summary: $SUMMARY
License: MIT
URL: https://sshremote.dev
BuildArch: $ARCH
Requires: glibc >= 2.39
Recommends: curl, xdg-utils, openssh-clients
%global debug_package %{nil}
%global __os_install_post %{nil}
%global __strip /bin/true
%description
$ABOUT
%install
cp -a "$ROOT/." %{buildroot}/
%files
/usr/bin/sshremote
/usr/share/applications/sshremote.desktop
/usr/share/icons/hicolor/256x256/apps/sshremote.png
%license /usr/share/doc/sshremote/copyright
%doc /usr/share/doc/sshremote/THIRD-PARTY-NOTICES.txt
EOF
  rpmbuild --define "_topdir $WORK/top" --target "$ARCH" -bb "$WORK/top/SPECS/sshremote.spec" > /dev/null
  cp "$WORK/top/RPMS/$ARCH/sshremote-$VERSION-1.$ARCH.rpm" "$FILE"
  ;;
*)
  echo "ci-package.sh: deb, rpm or msi, not $KIND" >&2
  exit 1
  ;;
esac
rm -rf "$WORK"
sha256sum "$FILE"
