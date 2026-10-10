# Evolve TV

Client Twitch léger pour Linux (X11) : la vidéo (mpv, intégrée et détachable) et,
à partir de l'étape 2, le chat avec les emotes 7TV / BTTV / FFZ.

## Prérequis

- Rust stable (la version est figée dans `rust-toolchain.toml`) ;
- `mpv` et `streamlink` (dans le PATH ou dans `~/.local/bin`) ;
- une session **X11** (la vidéo intégrée utilise `mpv --wid`).

## Compiler et lancer

```bash
cargo build --release
./target/release/evolve-tv
```

Mesurer la RAM (PSS, mpv et streamlink compris) pendant que l'application tourne :

```bash
./scripts/mesure-ram.sh
```

## Raccourcis clavier

| Touche | Action |
|---|---|
| Espace | Pause / reprise **au direct** (la reprise reconnecte mpv au direct) |
| F ou F11 | Plein écran (Échap pour sortir) |
| M | Couper / rétablir le son |
| ↑ / ↓ | Volume ±5 |

Fermer la fenêtre vidéo détachée la rattache à la fenêtre principale.

## Configuration

Fichier `~/.config/evolve-tv/config.json`, créé au premier lancement avec les valeurs
par défaut (droits `0600`). Le JSON n'accepte pas de commentaires : voici le sens de
chaque champ.

```json
{
  "channel": "some_channel",
  "autostart": true,
  "default_profile": "5G",
  "profiles": [
    { "name": "5G", "max_quality": "480p", "wifi_networks": [] },
    { "name": "Wi-Fi", "max_quality": "1080p60", "wifi_networks": ["HomeWifi"] }
  ],
  "player": {
    "volume": 70,
    "video_output": "auto",
    "hardware_decoding": true,
    "streamlink_path": null,
    "mpv_path": null
  }
}
```

| Champ | Rôle |
|---|---|
| `channel` | Chaîne Twitch (lettres, chiffres, `_`). |
| `autostart` | Lance le stream à l'ouverture ; s'il est hors ligne, attend le début du live. |
| `default_profile` | Profil utilisé quand le Wi-Fi actuel n'est associé à aucun profil. |
| `profiles[].max_quality` | Qualité maximale : la meilleure qualité disponible **sans la dépasser** est choisie (`480p` → 480p30, sinon 360p…). Exemples : `480p`, `720p60`, `1080p60`. |
| `profiles[].wifi_networks` | Noms (SSID) des réseaux Wi-Fi qui activent ce profil. Le bouton « Associer ce réseau » en bas de la fenêtre les remplit. |
| `player.volume` | Volume 0–100, mémorisé automatiquement. |
| `player.video_output` | `auto` (Xv jusqu'à 480p, GPU au-dessus), `xv` (le plus léger en RAM, mais décodage par le processeur) ou `gpu` (le moins de CPU). |
| `player.hardware_decoding` | En sortie GPU : décodage par la carte graphique. |
| `player.streamlink_path`, `player.mpv_path` | Chemins imposés des programmes (`null` : recherche automatique). |

Un champ inconnu ou mal orthographié est signalé en bas de la fenêtre ; l'application
utilise alors la configuration par défaut **sans réécrire** le fichier.

## Sécurité

- Aucun code `unsafe` (`#![forbid(unsafe_code)]`) ; programmes externes lancés sans shell,
  arguments validés (nom de chaîne, qualités).
- streamlink sert le flux uniquement sur `127.0.0.1` ; mpv ne lit que cette adresse.
- streamlink et mpv sont lancés **sans** leur configuration utilisateur (`--no-config`),
  sans scripts mpv ni youtube-dl.
- Socket IPC de mpv dans `$XDG_RUNTIME_DIR/evolve-tv` (droits `0700`).
- mpv décode le flux dans un processus séparé, qui n'aura jamais accès au jeton Twitch.

## Limites connues

- Pendant une coupure pub, streamlink filtre la pub : l'image reste figée.
- Mettre en pause avec le bouton de mpv (et non la touche Espace) fait prendre du retard
  sur le direct.
- Si l'application est tuée brutalement (`kill -9`), streamlink reste en attente en
  arrière-plan : il ne télécharge rien mais occupe ~47 Mo. Pour le fermer :
  `pkill -f 'streamlink --no-config'`. (Une fermeture normale arrête tout.)
