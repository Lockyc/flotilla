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

- **Routing a handed document into the group whose folder holds it — needs a location source.** A bare document link lands in Unsorted, because nothing in flotilla knows a file's folder: a Docs, Sheets or Slides page carries no parent id (the title bar's Move button fetches it only when clicked). The shape: each provider's `Recogniser` also yields a head's container id; a per-provider `Locator` returns a file's ancestors nearest-first, or unknown; one provider-neutral function in `flotilla-core` picks the group with the nearest ancestor head, else Unsorted. Locations are looked up, never stored. Sources are the provider's own API, signed in per store, never a local sync client's index (Drive for desktop, Mountain Duck): those are other software, not reliably running. The unlock: a Google sign-in per store for the Drive API (`files.get?fields=parents`).

- **Offline editing of Docs, Sheets and Slides — blocked on the engine.** Google supports it only in Chrome and Edge, through an extension WKWebView can't run. The unlock (a stable Tauri CEF runtime that loads Docs Offline), the experiment to run and the research ledger are in [`offline-direction.md`](offline-direction.md); `/research` checks it.
