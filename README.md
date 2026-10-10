# Evolve TV

Client Twitch léger pour Linux (X11) : la vidéo (mpv, intégrée et détachable) et
le chat (lecture sans compte, écriture une fois connecté) avec les emotes Twitch,
7TV, BTTV et FFZ, animées et zero-width.

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

Dans la zone de saisie du chat :

| Touche | Action |
|---|---|
| Tab / Shift+Tab | Compléter le dernier mot (emotes puis pseudos), proposition suivante / précédente |
| `@` + Tab | Compléter un pseudo parmi les personnes actives dans le chat |
| Entrée | Envoyer |

Au survol d'un message : **Copier** et **Répondre** (vraie réponse Twitch, reliée au
message d'origine). Les liens s'ouvrent après confirmation (le domaine réel est affiché).

### Raccourcis (barre sous la saisie)

- **Messages enregistrés** : un clic envoie le message. Clic droit : l'insérer dans la
  saisie ou supprimer le bouton. « + » enregistre le texte de la saisie comme bouton.
- **Emotes favorites** : clic = insérer dans la saisie ; clic droit ou Ctrl+clic = envoyer.
- **★ Emotes** : parcourir toutes les emotes (recherche) ; un clic ajoute ou retire une
  favorite, un clic droit l'insère.
- Un message identique au précédent (moins de 30 s) reçoit un caractère invisible une
  fois sur deux, sinon Twitch le refuserait ; mode lent et limites d'envoi respectés.

Les emotes 7TV, BTTV et FFZ de la chaîne sont rechargées à chaque connexion au chat,
toutes les 30 minutes, ou avec le bouton « Actualiser les emotes ».

## Connexion au compte Twitch

Sans compte, le chat est en **lecture seule**. Pour écrire :

1. Enregistrer une application sur <https://dev.twitch.tv/console> (2FA requis) :
   URL de redirection `http://localhost`, catégorie *Application Integration*,
   type de client **Public**.
2. Copier son **ID client** dans `twitch_client_id` (voir la configuration ci-dessous).
3. Cliquer sur « Se connecter à Twitch » : un code s'affiche et la page
   twitch.tv/activate s'ouvre ; y saisir le code.

Le jeton est rangé dans le trousseau GNOME (entrée « evolve-tv »), vérifié toutes
les heures et renouvelé automatiquement. « Se déconnecter » le révoque chez Twitch
et le supprime du trousseau.

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
  "twitch_client_id": "abcdefghijklmnopqrstuvwxyz0123",
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
| `twitch_client_id` | ID client de ton application Twitch (type Public). Ce n'est pas un secret. `null` : chat en lecture seule. |
| `saved_messages` | Boutons de messages : `[{ "label": "Join", "text": "!join" }]` (`label` facultatif). |
| `favorite_emotes` | Noms des emotes favorites, dans l'ordre d'affichage. |
| `animated_emotes` | `true` : emotes animées ; `false` : images fixes (moins de CPU, utile sur batterie). |
| `player.volume` | Volume 0–100, mémorisé automatiquement. |
| `player.video_output` | `auto` (Xv jusqu'à 480p, GPU au-dessus), `xv` (le plus léger en RAM, mais décodage par le processeur) ou `gpu` (le moins de CPU). |
| `player.hardware_decoding` | En sortie GPU : décodage par la carte graphique. |
| `player.streamlink_path`, `player.mpv_path` | Chemins imposés des programmes (`null` : recherche automatique). |

L'application réécrit ce fichier quand tu ajoutes un bouton ou une favorite : ferme-la
avant de modifier le fichier à la main.

Un champ inconnu ou mal orthographié est signalé en bas de la fenêtre ; l'application
utilise alors la configuration par défaut **sans réécrire** le fichier.

## Sécurité

- Aucun code `unsafe` (`#![forbid(unsafe_code)]`) ; programmes externes lancés sans shell,
  arguments validés (nom de chaîne, qualités).
- streamlink sert le flux uniquement sur `127.0.0.1` ; mpv ne lit que cette adresse.
- streamlink et mpv sont lancés **sans** leur configuration utilisateur (`--no-config`),
  sans scripts mpv ni youtube-dl.
- Jeton Twitch dans le trousseau GNOME, jamais dans un fichier ; droits demandés au
  minimum (`chat:read`, `chat:edit`, `user:read:emotes`), sans secret client.
- Chat chiffré (TLS, bibliothèque rustls en pur Rust) ; messages sortants nettoyés
  (pas de retour à la ligne : impossible d'injecter une commande IRC).
- Limites d'envoi de Twitch respectées (20 messages / 30 s, mode lent).
- Images (emotes, badges, emojis) téléchargées uniquement depuis les CDN officiels de
  Twitch, 7TV, BTTV, FFZ et Twemoji (jsDelivr, version figée) (adresses reconstruites à partir des identifiants, jamais reprises
  telles quelles) ; formats PNG, GIF et WebP décodés en pur Rust ; taille, dimensions
  et nombre de frames limités ; cache dans `~/.cache/evolve-tv` (droits `0700`).
- Socket IPC de mpv dans `$XDG_RUNTIME_DIR/evolve-tv` (droits `0700`).
- mpv décode le flux dans un processus séparé, qui n'aura jamais accès au jeton Twitch.

## Limites connues

- Pendant une coupure pub, streamlink filtre la pub : l'image reste figée.
- Mettre en pause avec le bouton de mpv (et non la touche Espace) fait prendre du retard
  sur le direct.
- Si l'application est tuée brutalement (`kill -9`), streamlink reste en attente en
  arrière-plan : il ne télécharge rien mais occupe ~47 Mo. Pour le fermer :
  `pkill -f 'streamlink --no-config'`. (Une fermeture normale arrête tout.)

## Crédits

Emojis : [Twemoji](https://github.com/jdecked/twemoji), © Twitter, Inc et contributeurs,
licence [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/).
