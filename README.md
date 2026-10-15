# Evolve TV

A lightweight Twitch client for Linux (X11): the video (mpv, embedded and detachable) and the chat
(read without an account, write once logged in) with Twitch, 7TV, BTTV and FFZ emotes, animated and
zero-width.

The application itself is in French (its buttons and messages); the code and this documentation are
in English.

## Requirements

- Rust stable (the version is pinned in `rust-toolchain.toml`);
- `mpv` and `streamlink` (in the PATH or in `~/.local/bin`);
- an **X11** session (the embedded video uses `mpv --wid`).

## Install (Cinnamon menu)

```bash
cargo build --release && ./scripts/installer.sh
```

Installs the program in `~/.local/bin`, the icon and the launcher in `~/.local/share` (no
administrator rights). After updating the code, run both commands again.

## Build and run

```bash
cargo build --release
./target/release/evolve-tv
```

Measure the RAM (PSS, mpv and streamlink included) while the application is running:

```bash
./scripts/mesure-ram.sh
```

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space | Pause / resume **at live** (resuming reconnects mpv to the live stream) |
| F11 | Fullscreen window, layout kept (with T: video + chat immersion) |
| F | Fullscreen for the video alone (Esc to leave) |
| T | Theatre mode: hides the top and bottom bars (Esc to leave) |
| M | Mute / unmute |
| ↑ / ↓ | Volume ±5 |

The bar under the video groups: start / stop, pause (resume at live), sound and volume, quality,
theatre mode, detach the video, fullscreen.

Closing the detached video window attaches it back to the main window.

In the chat input:

| Key | Action |
|---|---|
| Tab / Shift+Tab | Complete the last word (emotes, then nicknames), next / previous suggestion |
| `@` + Tab | Complete a nickname among the people active in the chat |
| Enter | Send |

Hovering a message shows **Copy** and **Reply** (a real Twitch reply, linked to the original
message), in a reserved margin that never hides the text. Right click on a message: copy the message,
copy the nickname, reply. Links open after a confirmation (the real domain is displayed).

### Shortcuts (bar under the input)

- **Saved messages**: a click sends the message. Right click: insert it in the input or delete the
  button. "+" saves the input's text as a button.
- **Favourite emotes**: click = send; right click = insert in the input.
- **★ Emotes**: browse all the emotes (search); a click adds or removes a favourite, a right click
  inserts it.
- A message identical to the previous one (less than 30 s apart) gets an invisible character one
  time out of two, otherwise Twitch would refuse it; slow mode and sending limits are respected.

The channel's 7TV, BTTV and FFZ emotes are reloaded every time the chat connects, every 30 minutes,
or with the "Actualiser les emotes" (refresh emotes) button.

## Twitch account login

Without an account, the chat is **read-only**. To write:

1. Register an application at <https://dev.twitch.tv/console> (2FA required): redirect URL
   `http://localhost`, category *Application Integration*, client type **Public**.
2. Copy its **client ID** into `twitch_client_id` (see the configuration below).
3. Click "Se connecter à Twitch" (log in to Twitch): a code is displayed and the twitch.tv/activate
   page opens; enter the code there.

The token is stored in the GNOME keyring (entry "evolve-tv"), checked every hour and renewed
automatically. "Se déconnecter" (log out) revokes it at Twitch and removes it from the keyring.

## Configuration

File `~/.config/evolve-tv/config.json`, created at first launch with the default values (`0600`
permissions). JSON does not allow comments: here is the meaning of each field.

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

| Field | Role |
|---|---|
| `channel` | Twitch channel (letters, digits, `_`). |
| `autostart` | Starts the stream on opening; if it is offline, waits for the live to begin. |
| `default_profile` | Profile used when the current Wi-Fi is not associated with any profile. |
| `profiles[].max_quality` | Maximum quality: the best available quality **without exceeding it** is chosen (`480p` → 480p30, otherwise 360p…). Examples: `480p`, `720p60`, `1080p60`. |
| `profiles[].wifi_networks` | Names (SSID) of the Wi-Fi networks that activate this profile. The "Associer ce réseau" (associate this network) button at the bottom of the window fills them in. |
| `twitch_client_id` | Client ID of your Twitch application (Public type). It is not a secret. `null`: read-only chat. |
| `saved_messages` | Message buttons: `[{ "label": "Join", "text": "!join" }]` (`label` optional). |
| `favorite_emotes` | Names of the favourite emotes, in display order. |
| `animated_emotes` | `true`: animated emotes; `false`: still images (less CPU, useful on battery). |
| `player.volume` | Volume 0–100, remembered automatically. |
| `player.video_output` | `auto` (Xv up to 480p, GPU above), `xv` (lightest in RAM, but CPU decoding) or `gpu` (least CPU). |
| `player.hardware_decoding` | With GPU output: decoding by the graphics card. |
| `player.streamlink_path`, `player.mpv_path` | Forced paths of the programs (`null`: automatic lookup). |

The application rewrites this file when you add a button or a favourite: close it before editing
the file by hand.

An unknown or misspelled field is reported at the bottom of the window; the application then uses
the default configuration **without rewriting** the file.

## Security

- No `unsafe` code (`#![forbid(unsafe_code)]`); external programs are launched without a shell, with
  validated arguments (channel name, qualities).
- streamlink serves the stream on `127.0.0.1` only; mpv only reads that address.
- streamlink and mpv are launched **without** their user configuration (`--no-config`), without mpv
  scripts or youtube-dl.
- Twitch token in the GNOME keyring, never in a file; minimal scopes requested (`chat:read`,
  `chat:edit`, `user:read:emotes`), no client secret.
- Encrypted chat (TLS, pure Rust rustls library); outgoing messages are sanitised (no line breaks:
  an IRC command cannot be injected).
- Twitch's sending limits are respected (20 messages / 30 s, slow mode).
- Images (emotes, badges, emojis) are only downloaded from the official CDNs of Twitch, 7TV, BTTV,
  FFZ and Twemoji (jsDelivr, pinned version); addresses are rebuilt from identifiers, never taken as
  is; PNG, GIF and WebP formats are decoded in pure Rust; size, dimensions and number of frames are
  limited; cache in `~/.cache/evolve-tv` (`0700` permissions).
- mpv's IPC socket is in `$XDG_RUNTIME_DIR/evolve-tv` (`0700` permissions).
- mpv decodes the stream in a separate process, which will never have access to the Twitch token.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Known limits

- During an ad break, streamlink filters the ad: the picture stays frozen.
- Pausing with mpv's own button (rather than the Space key) makes you fall behind the live.
- If the application is killed abruptly (`kill -9`), streamlink stays waiting in the background: it
  downloads nothing but uses ~47 MB. To close it: `pkill -f 'streamlink --no-config'`. (A normal
  exit stops everything.)

## Approach

This project started from a concrete need: replace a browser tab (about 870 MB of RAM on my 8 GB
laptop) with a native application for watching Twitch streams and taking part in the chat, with a
target of 150 to 200 MB in total, video included.

I wrote the requirements and drove the project, and I developed it with the assistance of
**Claude** (Anthropic), used as a pair-programming partner. Every commit message ends with a line
saying it was developed with Claude, for that reason. I was new to Rust, so I asked for each step to
be explained, and I asked to be consulted before every technical choice (language, graphics library,
configuration format, crates), with the options, their trade-offs and an estimated RAM cost. I chose
Rust and egui, mpv driven over its JSON IPC rather than a bundled player, the Device Code Flow for
login, and the safety rules above (no shell, local-only stream, token in the keyring).

The result was measured rather than assumed: 191 MB in total at 480p and 296 MB at 1080p60, with
the `scripts/mesure-ram.sh` script, and the buffer sizes were tuned from those measurements.

## Credits

Emojis: [Twemoji](https://github.com/jdecked/twemoji), © Twitter, Inc and contributors, licence
[CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/).

Icons: [Phosphor](https://phosphoricons.com) (through the `egui-phosphor` crate), MIT licence.

Font: [Inter](https://rsms.me/inter/) 4.1, © The Inter Project Authors, SIL Open Font License 1.1
(see `assets/fonts/Inter-LICENSE.txt`).

## License

[MIT](LICENSE) for the code of Evolve TV; the third-party elements above keep their own licences.
