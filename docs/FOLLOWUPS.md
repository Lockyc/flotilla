---
type: reference
links:
  - rel: part-of
    to: CLAUDE.md
    note: CLAUDE.md links here for deferred work
  - rel: references
    to: docs/offline-direction.md
    note: the offline item's unlock, evidence and research ledger
---

# flotilla — deferred follow-ups

Known, intentionally deferred work. Remove an item when it's done.

- **Reordering by drag — needs a positional move in config-core.** A dragged document joins the end of the group it lands in, because config-core's `move_tab`/`add_tab` only append, and the config file is the order. The unlock: a config-core edit that moves a tab table to an index (then every app re-pins it), a `Dest` index in `Workspace::move_tab`, `write::plan` diffing member order, and a drop position from chrome-core's `onMoveTab`. Dragging a group (a head) to reorder groups is the same work one level up.

- **Offline editing of Docs, Sheets and Slides — blocked on the engine.** Google supports it only in Chrome and Edge, through an extension WKWebView can't run. The unlock (a stable Tauri CEF runtime that loads Docs Offline), the experiment to run and the research ledger are in [`offline-direction.md`](offline-direction.md); `/research` checks it.
