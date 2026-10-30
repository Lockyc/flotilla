<p align="center">
  <img src="src-tauri/icons/icon.png" alt="flotilla app icon" width="128" height="128">
</p>

<h1 align="center">flotilla</h1>

<p align="center">
  <a href="https://github.com/Lockyc/flotilla/releases/latest"><img src="https://img.shields.io/github/v/release/Lockyc/flotilla?sort=semver&label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/platform-macOS-000000?logo=apple&logoColor=white" alt="Platform: macOS">
  <img src="https://img.shields.io/badge/built%20with-Tauri%20v2-24C8DB?logo=tauri&logoColor=white" alt="Built with Tauri v2">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Lockyc/flotilla" alt="License: MIT"></a>
</p>

A macOS workspace for work that spreads across many documents. Each piece of work is a **group**: a head tab (usually a Google Drive folder) with the documents opened from it listed beneath. Every document is open in exactly one place, so you stop losing track of which folder is open and which docs already are.

**Status:** early (0.x). Google Docs, Sheets, Slides, Forms and Drive files are recognised as documents; other sites open as ordinary tabs.

## How it works

- **Groups form as you work.** Paste a folder or page link into the field at the top of the sidebar (**⌘N**) to open it as a group. Any tab can head a group. A link to a document, clicked in any tab of a group, opens as a member of that group, with no setup.
- **Each document is open once.** Opening a document that is already open, in any group or popped-out window, brings up the existing tab instead of a second copy.
- **Pop out.** Any tab can move into its own window (**⌘⇧O**); closing that window (**⌘W**) puts the tab back in its group. A document opened from a popped-out window joins its group in the main window, which comes to the front.
- **Restored on relaunch.** Every group comes back with its head, its documents and its popped-out windows. Only the active tab loads straight away.
- **Close and reopen.** **⌘W** closes the active tab and **⌘⇧⌫** its whole group, popped-out windows included. A closed group is kept: choosing it under **Group ▸ Recent** reopens it with its documents, docked in the main window; **Recent ▸ Clear Recent** empties the list.
- **Find things in the sidebar.** Groups list their documents beneath them, and search covers every group. Right-click a row, or use the **Group** menu, for Home, Make Own Group, Move to Group, Close Tab and Close Group.

Links to other sites leave flotilla for your default browser. Content runs in WKWebView (Safari's engine), so Chrome extensions and Docs offline mode are not available.

## Install

Download `flotilla-<version>-macos.zip` from the [latest release](https://github.com/Lockyc/flotilla/releases/latest), unzip, and move `flotilla.app` to `/Applications`. macOS only.

To build from source (needs Rust and the Tauri CLI, `cargo install tauri-cli --version ^2`):

```sh
git clone https://github.com/Lockyc/flotilla && cd flotilla
just install      # builds a release .app, installs it to /Applications, relaunches
```

## Updates

flotilla updates itself. On launch, every 6 hours while open, and via **flotilla ▸ Check for Updates…**, it checks GitHub for a newer release; when one exists the sidebar shows an *Update available* bar with a one-click **Update & Relaunch**. Nothing installs without your approval, and each update's signature is verified first.

## Config

Settings are app-wide, in `~/.config/flotilla/config.toml` (or the file `FLOTILLA_CONFIG` names). **Config ▸ Edit Config** opens it, writing a starter file first if there is none. Saving hot-reloads; a file that doesn't parse keeps the last good config and shows the error in the sidebar. Groups never go in the config: they live in the session.

| Key | Default | What it does |
|---|---|---|
| `title` | `""` | The window's name, shown in the sidebar banner, the window title and **Window** menu. Empty for none. Hot-reloads. |
| `colour` | `""` | `#rgb` / `#rrggbb` accent for the sidebar, the **Window** menu swatch and popped-out windows. Empty for neutral. Hot-reloads. |
| `density` | `"comfortable"` | Chrome sizing: `comfortable` or `compact` (type and spacing scaled down). Hot-reloads. |
| `dark_mode` | `false` | Force a dark appearance so pages honouring `prefers-color-scheme` render dark. Hot-reloads. |
| `auto_update` | `true` | Check for a new release on launch and every 6 hours. `false` stops the automatic checks; **Check for Updates…** still works. Read at launch (restart to change). |
| `allow_insecure` | `[]` | Hosts whose self-signed or invalid TLS certificate is accepted. Read at launch (restart to change). |
| `format_on_save` | `false` | Reformat the config in house style on each clean reload. A file that fails to parse is left untouched. |

## Build

```sh
just run      # run the app
just gate     # format check, clippy, tests
just build    # release .app bundle
```

## License

MIT — see [LICENSE](LICENSE).
