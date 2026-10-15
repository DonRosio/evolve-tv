#!/usr/bin/env bash
# Installs Evolve TV for the current user (without sudo):
#   ~/.local/bin/evolve-tv                     the program
#   ~/.local/share/evolve-tv/icon.png          the icon (kept out of the git repository: the author's rights)
#   ~/.local/share/applications/evolve-tv.desktop   the Cinnamon menu launcher
#
# Usage: ./scripts/installer.sh   (from the project root, after cargo build --release)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
binary="$root/target/release/evolve-tv"
icon_source="$root/assets/icon.png"
bin_dir="$HOME/.local/bin"
data_dir="$HOME/.local/share/evolve-tv"
apps_dir="$HOME/.local/share/applications"

[ -x "$binary" ] || { echo "Binaire absent : lance d'abord cargo build --release"; exit 1; }

mkdir -p "$bin_dir" "$data_dir" "$apps_dir"

# Atomic replacement: no effect on an instance that is already running.
cp "$binary" "$bin_dir/.evolve-tv.new"
chmod 755 "$bin_dir/.evolve-tv.new"
mv -f "$bin_dir/.evolve-tv.new" "$bin_dir/evolve-tv"

if [ -f "$icon_source" ]; then
    cp "$icon_source" "$data_dir/icon.png"
    chmod 644 "$data_dir/icon.png"
else
    echo "Icône absente (assets/icon.png) : l'app s'affichera avec une icône par défaut."
fi

cat > "$apps_dir/evolve-tv.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Evolve TV
GenericName=Client Twitch léger
Comment=Regarder un stream Twitch avec le chat (emotes 7TV, BTTV, FFZ)
Exec=$bin_dir/evolve-tv
Icon=$data_dir/icon.png
Terminal=false
Categories=AudioVideo;
Keywords=twitch;stream;chat;
StartupNotify=true
StartupWMClass=evolve-tv
EOF

if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$apps_dir/evolve-tv.desktop"
fi
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$apps_dir" || true
fi

echo "Evolve TV installé dans $bin_dir/evolve-tv"
echo "Il apparaît dans le menu Cinnamon (catégorie Multimédia)."
