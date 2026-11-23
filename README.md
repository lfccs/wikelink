# Wikelink

Wikelink (`wikelink.exe`) is the Windows program that runs on the PC where you play Star Citizen and keeps your account on [Wikelist](https://wikelist.net) (a fan-made tracker for **Wikelo**'s contracts) up to date. It was called *Wikelist Watcher* (`wikelo-watcher.exe`) before.

It does three things:

- **Reads the game's `Game.log`** while you play and sends your mission events to your account (Wikelo contracts accepted, delivered, completed, the end of other missions that pay items the site tracks, and where you are). The site updates your inventory and completions by itself.
- **Shows an overlay** over the game (optional): your pinned items, what is still missing, what is stored where you are, recent records, a search, and the channel's blueprints (which ones you have; opening one asks the site for its craft, which the overlay works out at the qualities you choose, and for the missions that give it).
- **Sends the game data** when a new game version comes out (optional): it extracts contracts, items and icons from the installed `Data.p4k` and uploads a bundle.

This repository is public so that anyone can read what the program does and check that the published executable was built from this code. Wikelink is in English, or in Portuguese when Windows is; the settings window lets you choose.

## What it reads and what it sends

On your PC it reads:

- the game's `Game.log` (and, for the optional game-data upload, the game's `Data.p4k` and `build_manifest.id`), found in the usual install folders;
- the game's old logs in the `logbackups` folder next to `Game.log`, only for the blueprints you already received (the "Received Blueprint" lines): when it starts and then once an hour, up to 200 files at a time, the newest first, at most 256 MiB of each. It only reads them; nothing in the game folder is written, renamed or deleted;
- its own files in `%LOCALAPPDATA%\wikelo-sc\data\` (`watcher.json` with your session and settings, `watcher.status.json`, `watcher.log`, `logbackups-read.json`, the list of old logs already read (name and size; a file whose size changed is read again), and `overlay.log`, where the overlay notes what Windows answered each time it was asked to open; it stays on your PC).

It never opens, injects into or hooks the game process. The overlay is a separate transparent window, like any always-on-top app, and finds the game by its window title; the game must run in **borderless window** mode. The overlay's shortcuts (`Shift+Tab`, `Alt+Shift+F9`) are read by polling the key state, not with a hook. It never synthesizes a key press, a click or a mouse movement. Only after Windows refuses to bring the overlay to the front on `Shift+Tab` does it send one empty input event with `SendInput` (a synthetic mouse event with no movement, no button and no wheel, which goes through the system's input stream like any input and is marked as injected for programs that watch input; it is not sent to the game's process, and nothing is written to or read from that process) and ask again; in the tests done, that is what makes Windows accept the request.

It sends to Wikelist (`https://wikelist.net`), and nowhere else:

- mission events from the `Game.log`, including the character name the log shows, plus a heartbeat with this PC's name, the channel (LIVE, PTU…) and Wikelink's version;
- the blueprints found in the old logs (each one's name and when it was received), in the same request as the mission events;
- if you turn it on, the game data extracted from your install;
- the overlay's changes you make yourself (quantities, pins, moves between locations).

Wikelink never asks for your account's password. To sign in it asks Wikelist for a short code (sending this PC's name, so the site can show which PC is asking), shows it in its window and opens `https://wikelist.net/link` in your browser, where you, signed in on the site, type that code and confirm, and meanwhile asks Wikelist every few seconds whether you did. Then it receives a session token; only the token is kept, in `watcher.json` on your PC. The page it opens is always built from Wikelink's own address, and a code that is not two groups of four letters or digits is refused. Signing out asks Wikelist to end that session too (if it cannot be reached, the session is forgotten on your PC anyway). Once a day Wikelink asks Wikelist which release is the newest and tells you when there is a newer one; it never downloads or installs anything by itself (see *Updating*).

## Download and check

[Releases](https://github.com/lfccs/wikelink/releases) has `wikelink.exe` and `SHA256SUMS.txt`, both made by GitHub Actions from the tag ([`.github/workflows/release.yml`](.github/workflows/release.yml)). After downloading:

```powershell
Get-FileHash .\wikelink.exe -Algorithm SHA256                         # must match SHA256SUMS.txt
gh attestation verify .\wikelink.exe --repo lfccs/wikelink   # GitHub signs every build (needs the GitHub CLI)
.\wikelink.exe hash      # prints the commit built into the binary
.\wikelink.exe status    # prints the version and says when a newer one exists
```

The exe has no code signature (SmartScreen may warn on the first run: *More info → Run anyway*); the attestation above is the proof of origin.

**Coming from `wikelo-watcher.exe`:** open `wikelink.exe` once. It uses the same session and settings, moves "Start with Windows" over to itself (the old entry is removed, so the two never start together) and offers to restart the background copy if the old one is still running. The old file is not touched; delete it when you like.

## Using it

Double-click opens the **settings window**: **Sign in** (the browser opens; type there the code the window shows), start / stop, the channel to follow, **Start with Windows**, the game-data upload, the overlay and the language. Once signed in, Wikelink runs in the background and the window can be closed. In the background it keeps an icon in the notification area: hover for its state, double-click for the settings, right-click for the site, the overlay and **Quit**.

The same is available from a terminal:

| Command | What it does |
|---|---|
| `wikelink` | Opens the settings window. |
| `wikelink start [--channel X --share-data true\|false --quiet]` | Sets up what is missing (signing in opens the site in the browser) and leaves Wikelink running in the background. |
| `wikelink stop` | Stops it (and the overlay). |
| `wikelink status` | Version, running or not, server / account / channel, last contact, last version check. |
| `wikelink autostart on` / `off` | Start with Windows (an entry under `HKCU\…\Run`). |
| `wikelink watch` | Runs in this console (Ctrl+C to quit), to see what happens. |
| `wikelink logout` | Stops, asks Wikelist to end the session and forgets it. |
| `wikelink update [--yes]` | Updates to the newest release after asking (`--yes` answers for a script); says so when there is nothing newer. See *Updating*. |
| `wikelink overlay [--preview]` | Opens the overlay (it shows with the game; `--preview` shows it now). |
| `wikelink bundle` | Writes a game-data bundle by hand, next to the exe. |
| `wikelink hash` | Extracts and prints the reproducible content hash of each installed channel, to compare with what the site shows. |
| `wikelink installs` / `extract` / `dump` / `p4k-ls` | Extraction tools for looking at the game data. |

In a terminal, `start` and `watch` sign in the same way as the window: they print the address and the code, open the browser and wait until you confirm or refuse on the site, the code expires, or you press Ctrl+C. There is no option for an account name or a password.

Wikelink talks only to `https://wikelist.net`; there is no setting for another address. For development only, the environment variable `WIKELINK_DEV_SERVER` points it at a server on this PC (`localhost`, `127.0.0.1` or `[::1]`, any port, e.g. `http://localhost:8787`); any other value is ignored, with a line in `watcher.log`. A session made on one of the two is never sent to the other: switching means signing in again.

The extraction is deterministic: the same `Data.p4k` always gives the same dataset and the same `contentHash`, so anyone with the game can reproduce what was uploaded with `wikelink hash`.

## Updating

When a newer release exists, the settings window says so, with **Open download** (the release page) and **Update**. **Update** asks first: it names the installed version, the new one and where the file comes from, and nothing is downloaded or changed until you choose **Update now**; **Cancel** leaves everything as it was. Wikelink never updates by itself, and no setting makes it. In a terminal, `wikelink update` asks the same question (`--yes` is the answer given in advance, for scripts). The tray icon has no update action.

What it does after you confirm:

- downloads `wikelink.exe` and `SHA256SUMS.txt` of that release from `https://github.com/lfccs/wikelink/releases/download/v<version>/`. Wikelist only tells it the version number; the address is built into Wikelink, the version must be newer than the installed one, and redirects are followed only to GitHub's download hosts. These downloads carry no session token and no cookie;
- checks that the SHA-256 of the file is the one `SHA256SUMS.txt` lists for `wikelink.exe`, and that it is a Windows program; otherwise it deletes the download and changes nothing;
- stops Wikelink in the background and the overlay, renames the running file to `wikelink.exe.old`, puts the new one in its place (if that fails, the old name is put back) and starts it again. The next start deletes the `.old`.

What the check proves: the file is the one published in that release of this repository, downloaded over HTTPS. It is not a code signature; for proof of which commit built it, see `gh attestation verify` in *Download and check*. If Wikelink's folder is not writable (say, under `Program Files`), it stops before downloading and says so: run Wikelink once as administrator, or download the file by hand.

## Build it yourself

```powershell
rustup show                                                  # installs the version pinned in rust-toolchain.toml
cargo build --release --locked -p wikelo-watcher --bin wikelink
# target\release\wikelink.exe
```

`Data.p4k` / DataForge are read with [StarBreaker](https://github.com/diogotr7/StarBreaker) (MIT), pinned to a commit in `Cargo.toml`.

| Crate | What it does |
|---|---|
| `crates/wikelo-watcher` | The program (`wikelink.exe`): window, background watcher, overlay, extraction, bundles. |
| `crates/wikelo-core` | Dataset types, calculations, canonical hash and validation rules. |
| `crates/wikelo-install` | Finds the game installs (`LIVE`, `PTU`, …) and reads `build_manifest.id`. |
| `crates/wikelo-log` | `Game.log` parser, per-mission state machine, log tailer, reader of the blueprints in old logs. |

The crate and folder names keep the old `wikelo-*` names.

## Licence

MIT, see [LICENSE](LICENSE). Star Citizen® is a trademark of Cloud Imperium Games; this is a personal fan project, not affiliated with it.

## Português

O Wikelink (antes *Wikelist Watcher*, `wikelo-watcher.exe`) é o programa para Windows que lê o `Game.log` enquanto você joga e mantém sua conta no [Wikelist](https://wikelist.net) em dia, com um overlay opcional por cima do jogo. Ele só se comunica com `https://wikelist.net`. Ele aparece em português quando o Windows está em português (dá para escolher na janela de opções). Como baixar, usar e conferir o arquivo: veja o site, em **Dados → Watcher**, e a seção *Download and check* acima.

**Logs antigos:** ele também lê os logs antigos da pasta `logbackups`, ao lado do `Game.log`, só para achar os blueprints que você já recebeu, e envia o nome e a data de cada um. Só lê: nada na pasta do jogo é gravado, renomeado ou apagado. A lista dos arquivos já lidos (nome e tamanho) fica em `logbackups-read.json`, na pasta de dados do Wikelink.

**Entrar:** o Wikelink nunca pede a senha da conta. Em **Entrar**, ele pede um código curto ao Wikelist (enviando o nome deste PC), mostra esse código na janela e abre `https://wikelist.net/link` no navegador, onde você, já conectado no site, digita o código e confirma, e recebe então o token da sessão, que fica só no `watcher.json` do seu PC. No terminal, `start` e `watch` fazem o mesmo (mostram o endereço e o código; Ctrl+C cancela). Ao sair da conta, ele pede ao Wikelist que encerre essa sessão também.

**Atualizar:** quando sai uma versão nova, a janela de opções mostra **Atualizar**. Ele pergunta antes, dizendo a versão instalada, a nova e de onde o arquivo vem; nada é baixado nem trocado até você escolher **Atualizar agora**, e **Cancelar** deixa tudo como estava. O Wikelink nunca se atualiza sozinho. No terminal: `wikelink update` (`--yes` responde pela pergunta, para scripts). Ele baixa `wikelink.exe` e `SHA256SUMS.txt` da release deste repositório no GitHub (o Wikelist só informa o número da versão), confere o SHA-256 e se é um programa do Windows, e só então troca o arquivo e abre de novo; se algo falhar, o arquivo instalado fica como estava. Isso prova que o arquivo é o publicado nessa release, baixado por HTTPS; não é uma assinatura de código. Se a pasta do Wikelink não aceitar gravação, ele avisa antes de baixar: abra-o uma vez como administrador ou baixe à mão.
