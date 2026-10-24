#!/usr/bin/env sh
# Install Luma for the current user on Linux, as Zed's script/install.sh does:
#
#   curl -fsSL https://github.com/julianallchin/luma/releases/latest/download/install.sh | sh
#
# Unpacks the release tarball into ~/.local/luma.app, links ~/.local/bin/luma,
# and adds a desktop entry. The app updates itself in place from then on.
#
# LUMA_CHANNEL=nightly installs the rolling nightly instead of the latest
# release. On macOS, download the DMG from the releases page.
set -eu

repo=https://github.com/julianallchin/luma
channel=${LUMA_CHANNEL:-stable}

case "$(uname -s)" in
    Linux) ;;
    *) echo "install.sh is for Linux. Download Luma for other systems from $repo/releases" >&2; exit 1 ;;
esac
arch=$(uname -m)
case "$arch" in
    x86_64) ;;
    *) echo "Luma does not publish a Linux build for $arch yet." >&2; exit 1 ;;
esac
case "$channel" in
    stable) url="$repo/releases/latest/download/luma-linux-$arch.tar.gz" ;;
    nightly) url="$repo/releases/download/nightly/luma-linux-$arch.tar.gz" ;;
    *) echo "Unknown LUMA_CHANNEL '$channel' (stable or nightly)" >&2; exit 1 ;;
esac

prefix="$HOME/.local"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading Luma ($channel)…"
if command -v curl >/dev/null 2>&1; then
    curl -fL --progress-bar "$url" -o "$tmp/luma.tar.gz"
else
    wget -q --show-progress "$url" -O "$tmp/luma.tar.gz"
fi
tar -xzf "$tmp/luma.tar.gz" -C "$tmp"

mkdir -p "$prefix/bin" "$prefix/share/applications"
rm -rf "$prefix/luma.app"
mv "$tmp/luma.app" "$prefix/luma.app"
ln -sf "$prefix/luma.app/bin/luma" "$prefix/bin/luma"

desktop="$prefix/share/applications/luma.desktop"
sed -e "s|^Exec=.*|Exec=$prefix/luma.app/bin/luma|" \
    -e "s|^Icon=.*|Icon=$prefix/luma.app/share/icons/hicolor/512x512/apps/luma.png|" \
    "$prefix/luma.app/share/applications/luma.desktop" > "$desktop"

echo "Installed Luma to $prefix/luma.app"
case ":$PATH:" in
    *":$prefix/bin:"*) echo "Run it with: luma" ;;
    *) echo "Run it with: $prefix/bin/luma  (add $prefix/bin to PATH to type just 'luma')" ;;
esac
