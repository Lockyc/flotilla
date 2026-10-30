---
type: reference
links:
  - rel: part-of
    to: CLAUDE.md
    note: CLAUDE.md links here for deferred work
---

# flotilla — deferred follow-ups

Known, intentionally deferred work. Remove an item when it's done.

## Offline editing of Docs, Sheets and Slides — blocked on the engine

**What:** documents keep opening and editing with no network, the way they do in Chrome.

**Blocker:** Google supports offline only in Chrome and Edge, and only through its Docs Offline extension from the Chrome Web Store ([Google's help page](https://support.google.com/docs/answer/6388102)). Having Drive for desktop installed turns offline on automatically, but only inside those browsers. flotilla's content runs in WKWebView, and no page script can stand in for the extension, so nothing can be done on the current engine.

**Unlock: a Chromium engine that can also run that extension.** Both halves are needed, and neither exists yet:

- **Tauri's CEF runtime** (`tauri-runtime-cef`) is in alpha (3.0.0-alpha.6, 2026-10-14). It supports child webviews, the shape `tabs` uses.
- **Extensions inside it:** CEF dropped its own extension-loading API along with the Alloy runtime (M128). The Chrome extension API remains only in Chrome-style browsers. `tauri-runtime-cef` ignores `browser_extensions_enabled`/`extensions_path` and logs a warning when they're set. It can create a Chrome-style browser (`RuntimeStyle::Chrome`), and it already drives Chromium over the DevTools protocol. The one loading route reported for CEF is the DevTools `Extensions.loadUnpacked` call with `--enable-unsafe-extension-debugging` (a CefSharp report, not CEF guidance). It loads an *unpacked* extension, not a Web Store install.

**Next action (when `tauri-runtime-cef` has a stable release):** in a throwaway Tauri app on the CEF runtime, create a Chrome-style child browser, load Docs Offline through `Extensions.loadUnpacked`, sign in, and check whether Docs offers offline mode. If it doesn't, the item stays blocked. If it does, the real work is porting flotilla to CEF: shell-core's web layer, `tabs::intercept` and the `src/inject/` scripts all assume WKWebView, so the engine would be a shared-core decision across the constellation, not a flotilla-only change.
