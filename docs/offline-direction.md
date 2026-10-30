---
type: decision
links:
  - rel: part-of
    to: CLAUDE.md
    note: CLAUDE.md points here for offline editing
research:
  bears-on: >-
    Offline editing of Docs, Sheets and Slides inside flotilla. News is anything that changes
    which browsers Google supports offline in, moves tauri-runtime-cef toward a stable release,
    or changes whether a Chrome-style CEF browser can load and run Google's Docs Offline extension.
  sources:
    - name: tauri-runtime-cef releases
      check: gh release list -R tauri-apps/tauri -L 40 | grep -i cef
      note: >-
        crates.io (https://crates.io/api/v1/crates/tauri-runtime-cef, needs a user-agent) lists
        the same versions. The crate's CHANGELOG.md and src/webview.rs (grep "extension") show
        what it does with extension attributes
    - name: Tauri CEF issues
      check: gh search issues --repo tauri-apps/tauri cef --sort updated -L 15 --json number,title,updatedAt,state
    - name: CEF extension issues
      check: gh search issues --repo chromiumembedded/cef extension --sort updated -L 10 --json number,title,updatedAt,state
      note: CEF announcements (cef-announce Google Group) carry runtime-wide removals such as Alloy's
    - name: Google's Docs offline help page
      check: https://support.google.com/docs/answer/6388102?hl=en&co=GENIE.Platform%3DDesktop
      note: the supported-browser list is the line to compare
---

# Offline editing — blocked on the engine

Google supports offline editing only in Chrome and Edge, and only through its Docs Offline extension from the Chrome Web Store. flotilla's content runs in WKWebView, and no page script can stand in for the extension, so offline editing is out of reach on the current engine.

## What the unlock needs

A Chromium engine under Tauri that loads Docs Offline, and Docs accepting it:

- **Engine:** `tauri-runtime-cef`, Tauri's CEF runtime. It hosts child webviews (`set_as_child`), the shape `tabs` uses.
- **Extension loading:** CEF dropped its own extension API along with the Alloy runtime (M128). A Chrome-style browser still runs Chrome's extension system, and it loads unpacked extensions given with `--load-extension=<dir>`. `tauri-runtime-cef` ignores the `browser_extensions_enabled`/`extensions_path` webview attributes, but passes browser-process switches through `Cef::command_line_arg`, which is where `--load-extension` goes.
- **Docs accepting it:** untested. An unpacked load gets an extension ID from the manifest's `key` (or the path, if there's no key), while the Web Store copy's ID comes from its signing key. If Docs looks for the extension by its Web Store ID, an unpacked copy needs a matching `key` to be recognised.

**Next action, once `tauri-runtime-cef` has a stable release:** in a throwaway Tauri app on the CEF runtime, unpack the Docs Offline CRX, load it with `--load-extension`, sign in, and check whether Docs offers offline mode. If it does, the real work is porting flotilla to CEF. shell-core's web layer, `tabs::intercept` and `src/inject/` all assume WKWebView, so the engine is a shared-core decision across the constellation, not a flotilla-only change.

## Evidence

- **Google:** "You must use the Google Chrome or Microsoft Edge browser." Requirements: install and turn on the Google Docs Offline Chrome extension (Edge users are sent to the Chrome Web Store for it), no private browsing, an internet connection to turn offline on, enough free disk space, and an up-to-date browser. Offline turns on automatically "if you have Drive for desktop installed on your device", in those browsers only.
- **CEF, Alloy removal** ([chromiumembedded/cef#3685](https://github.com/chromiumembedded/cef/issues/3685)): the Alloy extension API "has been removed in M128". The Chrome extension API is supported in Chrome-style browsers only. Windowless (off-screen) rendering is always Alloy style, so it can't host extensions. `chrome://extensions` in Alloy style was closed as not planned ([#3859](https://github.com/chromiumembedded/cef/issues/3859)).
- **CEF, MV3 extensions in Chrome style:** [#4309](https://github.com/chromiumembedded/cef/issues/4309) (closed 2026-10-23) loads an MV3 extension with a background service worker through `cefclient --load-extension=<dir>` on CEF 148 (macOS arm64). It found that CEF sent `Origin: null` on the service worker's CORS fetches, which broke 1Password's extension. A fix was tested on CEF 154. [#4327](https://github.com/chromiumembedded/cef/issues/4327) (open): `web_accessible_resources` with `use_dynamic_url` fail to load in Chrome-style browsers; static resources load.
- **tauri-runtime-cef 3.0.0-alpha.6** (source read): `webview.rs` documents `browser_extensions_enabled` and `extensions_path` as ignored because "CEF removed its extension loading API along with the Alloy runtime", and warns when either is set. `RuntimeStyle::{Alloy, Chrome}` is selectable per browser. `Cef::command_line_arg` and `Cef::enable_features` set process-wide switches.

## Status

- **2026-10-30** — `tauri-runtime-cef` is at 3.0.0-alpha.6 (2026-10-14), after alpha.0 on 09-13. No stable release. Open Tauri CEF issues are Linux input and CPU bugs. Google's supported browsers are still Chrome and Edge.
