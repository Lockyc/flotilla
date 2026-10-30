# flotilla

![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey) ![Rust](https://img.shields.io/badge/rust-Tauri%20v2-orange) ![License: MIT](https://img.shields.io/badge/license-MIT-blue)

A macOS workspace where any tab can head a group of documents (usually a Google Drive folder). Documents opened from a tab list beneath it, each document is open once, any tab can pop out into its own window (⌘⇧O; close the window to put it back), and the session, popped-out windows included, is restored on relaunch.

**Status:** early, in development. Paste a folder or page link into the field at the top of the sidebar to open it as a group.

## Build and run

```sh
just run      # run the app
just gate     # format check, clippy, tests
just build    # release .app bundle (needs `cargo install tauri-cli --version ^2`)
```
