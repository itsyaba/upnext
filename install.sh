#!/bin/sh
# Build and install Up Next for the current user (~/.local).
set -e
cd "$(dirname "$0")"
cargo build --release
install -Dm755 target/release/upnext "$HOME/.local/bin/upnext"
install -Dm644 data/io.github.itsyaba.UpNext.desktop "$HOME/.local/share/applications/io.github.itsyaba.UpNext.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
echo "Installed. Run 'upnext' or find 'Up Next' in your app launcher."
echo "Folders are set in ${XDG_CONFIG_HOME:-$HOME/.config}/upnext/config.json"
