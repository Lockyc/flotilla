// flotilla's chrome controller: binds the shared chrome-core ChromeSidebar (the view) to flotilla's
// Tauri backend. Rust owns the workspace and selection; it pushes the tab list as `workspace`
// events and this file merges it with the window identity.

const { invoke } = window.__TAURI__.core;
// A bare `listen()` is app-wide, not window-scoped; binding to this webview scopes it to this window.
const chromeWebview = window.__TAURI__.webview.getCurrentWebview();
const listen = (event, handler) => chromeWebview.listen(event, handler);

const MIN_W = 160, MAX_W = 520, MAX_FRACTION = 0.4;

let sb = null;
let identity = null;

function setSidebarWidth(w) {
  document.getElementById("sidebar").style.width = Math.round(w) + "px";
}

// Report the #content-hole's CSS rect so Rust positions document webviews to match.
function reportRect() {
  const r = document.getElementById("content-hole").getBoundingClientRect();
  invoke("set_hole_rect", { rect: { x: r.x, y: r.y, width: r.width, height: r.height } }).catch(() => {});
}

// The header's back / forward / home act on the active tab.
const NAV_COMMANDS = { back: "nav_back", forward: "nav_forward", home: "home_tab" };
let activeId = null; // mirror of the DTO's `active`

// chrome-core's window DTO: the identity fields plus Rust's `{ tabs, active, collapsed }`.
function windowDto(ws) {
  activeId = ws.active ?? null;
  sb.setNavEnabled(!!activeId);
  return {
    title: identity.title,
    colour: identity.colour ?? null,
    density: identity.density,
    windowDrag: identity.sidebar_drag !== false,
    openSection: identity.open_section === true,
    rail: identity.rail,
    ...ws,
  };
}

async function mountChrome() {
  identity = await invoke("window_identity");
  sb = window.ChromeSidebar.mount(
    document.getElementById("sidebar"),
    {
      // Clicking the active row again returns it to the page it opened with; clicking a popped-out
      // row raises its window.
      onSelect(id, { wasActive } = {}) {
        invoke(wasActive ? "home_tab" : "select_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      // The live dot: close the tab's webview, keep its row.
      onUnload(id) {
        invoke("unload_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      // A head row's unload-group button (`group` is the head's id): every loaded tab of the group.
      onUnloadGroup(group) {
        invoke("unload_group", { head: group }).catch((err) => sb.setError(String(err)));
      },
      // The bin: close the tab (a head closes its group into Recent).
      onRemove(id) {
        invoke("remove_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      // The icon tile's overlay: into its own window, and back into its group.
      onPopOut(id) {
        invoke("pop_out_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      onPopIn(id) {
        invoke("pop_in_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      // Right-click: the Group menu, acting on this row.
      onContextMenu(id, { x, y }) {
        invoke("row_menu", { id, x, y }).catch((err) => sb.setError(String(err)));
      },
      // Right-click on the window's title row: Edit Window… / Delete Window….
      onBannerMenu({ x, y }) {
        invoke("banner_menu", { x, y }).catch((err) => sb.setError(String(err)));
      },
      // An Incoming row's button: Add to the active group, Add to group… (a menu at the button),
      // New group, or Dismiss.
      onIncomingAction(id, action, { x, y }) {
        invoke("incoming_action", { id, action, x, y }).catch((err) => sb.setError(String(err)));
      },
      onMoveTab(id, group) {
        invoke("move_tab", { id, group }).catch((err) => sb.setError(String(err)));
      },
      onToggleGroup(group, collapsed) {
        invoke("toggle_group", { head: group, collapsed }).catch(() => {});
      },
      onResize(width) {
        setSidebarWidth(width);
        reportRect();
      },
      // Collapse and peek move the hole; a peek only by a transform, which the ResizeObserver misses.
      onLayout: reportRect,
      onNav(action) {
        if (activeId) invoke(NAV_COMMANDS[action], { id: activeId }).catch((err) => sb.setError(String(err)));
      },
      // Enter in the URL field opens the typed or pasted link as a new group.
      onUrlSubmit(url) {
        return invoke("new_group", { url });
      },
      onDownloads(rect) {
        invoke("downloads_toggle", { rect }).catch((err) => sb.setError(String(err)));
      },
    },
    {
      nav: ["back", "forward", "home"],
      urlField: { placeholder: "Open a folder or page as a new group" },
      downloads: true,
      appName: "flotilla",
      emptyState: document.getElementById("empty-state"),
      hole: document.getElementById("content-hole"),
      storageKey: "flotilla:sidebar-width",
      defaultWidth: identity.default_width,
      minWidth: MIN_W,
      maxWidth: MAX_W,
      maxFraction: MAX_FRACTION,
      autoUpdate: identity.auto_update !== false,
    }
  );
  sb.update(windowDto(await invoke("get_workspace")));
  sb.setDownloads(await invoke("downloads_state"));
  reportRect();
  if (!(parseFloat(localStorage.getItem("flotilla:sidebar-width")) > 0)) {
    setSidebarWidth(identity.default_width);
  }
  const notice = await invoke("startup_notice");
  if (notice) sb.setError(notice);
}

window.addEventListener("resize", () => {
  const el = document.getElementById("sidebar");
  const cur = parseInt(el.style.width, 10) || parseInt(getComputedStyle(el).width, 10);
  if (Number.isFinite(cur)) {
    setSidebarWidth(Math.max(MIN_W, Math.min(cur, Math.min(MAX_W, window.innerWidth * MAX_FRACTION))));
  }
  reportRect();
});
new ResizeObserver(() => reportRect()).observe(document.getElementById("content-hole"));

// ── Rename… : the row's name becomes a field in place ──────────────────────
// While a name is being edited, workspace pushes are held (a repaint would drop the field) and the
// latest one applied when the edit ends. Enter or leaving the field saves the name, as in Finder;
// Escape keeps the old one.
let renaming = null; // { id, name, input, title } while editing
let heldDto = null;

function update(dto) {
  if (renaming) heldDto = dto;
  else sb.update(windowDto(dto));
}

function endRename(save) {
  // Cleared first: the blur below calls back in here. The blur is explicit because removing a
  // focused field fires no focusout, and the sidebar's focusout is what lets a peek it held close.
  const r = renaming;
  if (!r) return;
  renaming = null;
  r.input.blur();
  r.input.remove();
  r.title.style.display = "";
  if (heldDto) {
    const dto = heldDto;
    heldDto = null;
    sb.update(windowDto(dto));
  }
  if (save && r.input.value !== r.name) {
    invoke("rename_tab", { id: r.id, name: r.input.value }).catch((err) => sb.setError(String(err)));
  }
}

function beginRename(id, name) {
  endRename(true);
  // A row hidden by a search is shown again (its group was already expanded by Rust).
  const row = sb.revealRow(id);
  const title = row && row.querySelector(".cc-title");
  if (!title) return;
  const input = document.createElement("input");
  input.className = "row-rename";
  input.type = "text";
  input.value = name;
  input.spellcheck = false;
  // Keystrokes and clicks in the field stay in it, not the row's selection.
  for (const ev of ["mousedown", "click", "dblclick", "contextmenu"]) {
    input.addEventListener(ev, (e) => e.stopPropagation());
  }
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") endRename(false);
    if (e.key === "Enter") endRename(true);
  });
  input.addEventListener("blur", () => endRename(true));
  title.style.display = "none";
  title.after(input);
  renaming = { id, name, input, title };
  input.focus();
  input.select();
}

listen("rename-row", (e) => sb && beginRename(e.payload.id, e.payload.title));
listen("workspace", (e) => sb && identity && update(e.payload));
// A reloaded config: re-read the identity (density, the Open section, the rail) and drop an earlier config error.
listen("config-reloaded", async () => {
  if (!sb) return;
  identity = await invoke("window_identity");
  sb.clearError();
  update(await invoke("get_workspace"));
});
listen("notice",(e) => sb && sb.setError(e.payload));
// Cycling walks loaded tabs only (the live dot); a cold row loads when clicked. From a popped-out
// window, `from` is that tab, which never holds the highlight.
listen("nav-tab", (e) =>
  sb && sb.selectByOffset(e.payload.dir, { liveOnly: true, from: e.payload.from ?? undefined }));
listen("jump-tab", (e) => sb && sb.selectByIndex(e.payload));
listen("find-in-sidebar", () => sb && sb.focusSearch());
listen("toggle-sidebar", () => sb && sb.toggleSidebar());
// Group ▸ New Group… (⌘N).
listen("focus-new-group", () => sb && sb.focusUrlField());
listen("downloads-state", (e) => sb && sb.setDownloads(e.payload));
// The pointer moved onto a content webview (shell-core's web::pointer): this page never sees its
// own exit, since the content composites above it, so chrome-core's `:hover` would freeze.
listen("pointer-away", () => sb && sb.pointerAway());
listen("check-update", () => sb && sb.checkForUpdateNow());

mountChrome();
