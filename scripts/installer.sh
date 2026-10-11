#!/usr/bin/env bash
# Installe Evolve TV pour l'utilisateur courant (sans sudo) :
#   ~/.local/bin/evolve-tv                     le programme
#   ~/.local/share/evolve-tv/icon.png          l'icône (hors dépôt git : droits de l'auteur)
#   ~/.local/share/applications/evolve-tv.desktop   le lanceur du menu Cinnamon
#
# Usage : ./scripts/installer.sh   (depuis la racine du projet, après cargo build --release)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
binary="$root/target/release/evolve-tv"
icon_source="$root/assets/icon.png"
bin_dir="$HOME/.local/bin"
data_dir="$HOME/.local/share/evolve-tv"
apps_dir="$HOME/.local/share/applications"

[ -x "$binary" ] || { echo "Binaire absent : lance d'abord cargo build --release"; exit 1; }

mkdir -p "$bin_dir" "$data_dir" "$apps_dir"

# Remplacement atomique : sans effet sur une instance déjà lancée.
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
