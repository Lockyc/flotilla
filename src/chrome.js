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

// ── Nav pill: back / forward / home, acting on the active tab ───────────────
const BACK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6"/></svg>`;
const FWD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>`;
const HOME_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/></svg>`;

let activeId = null; // mirror of the DTO's `active`, which the nav pill acts on
let navBtns = [];

function navPill() {
  const pill = document.createElement("div");
  pill.className = "nav-pill";
  const wire = (icon, cmd, name) => {
    const btn = document.createElement("button");
    btn.className = "nav-btn";
    btn.innerHTML = icon;
    btn.dataset.name = name;
    btn.addEventListener("click", () => {
      if (activeId) invoke(cmd, { id: activeId }).catch((err) => sb.setError(String(err)));
    });
    pill.appendChild(btn);
    return btn;
  };
  navBtns = [
    wire(BACK_SVG, "nav_back", "Back"),
    wire(FWD_SVG, "nav_forward", "Forward"),
    wire(HOME_SVG, "home_tab", "Home"),
  ];
  setActive(null);
  return pill;
}

function setActive(id) {
  activeId = id;
  for (const b of navBtns) {
    b.disabled = !id;
    b.title = id ? b.dataset.name : `${b.dataset.name} — no tab open`;
  }
}

// chrome-core's window DTO: the identity fields plus Rust's `{ tabs, active, collapsed }`.
function windowDto(ws) {
  setActive(ws.active ?? null);
  return {
    title: identity.title,
    colour: identity.colour ?? null,
    density: identity.density,
    windowDrag: identity.sidebar_drag !== false,
    ...ws,
  };
}

// The header slot: the nav pill, then the new-group field.
function bannerControls() {
  const row = document.createElement("div");
  row.id = "banner-controls";
  row.append(navPill(), newGroupField());
  return row;
}

// Enter opens the typed or pasted link as a new group.
function newGroupField() {
  const input = document.createElement("input");
  input.id = "new-group";
  input.type = "text";
  input.placeholder = "Open a folder or page as a new group";
  input.spellcheck = false;
  input.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    invoke("new_group", { url: input.value })
      .then(() => {
        input.value = "";
        sb.clearError();
      })
      .catch((err) => sb.setError(String(err)));
  });
  return input;
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
      onToggleGroup(group, collapsed) {
        invoke("toggle_group", { head: group, collapsed }).catch(() => {});
      },
      onResize(width) {
        setSidebarWidth(width);
        reportRect();
      },
    },
    {
      header: bannerControls(),
      appName: "flotilla",
      storageKey: "flotilla:sidebar-width",
      defaultWidth: identity.default_width,
      minWidth: MIN_W,
      maxWidth: MAX_W,
      maxFraction: MAX_FRACTION,
      autoUpdate: identity.auto_update !== false,
    }
  );
  sb.update(windowDto(await invoke("get_workspace")));
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

listen("workspace", (e) => sb && identity && sb.update(windowDto(e.payload)));
// A reloaded config: re-read the identity (density) and drop an earlier config error.
listen("config-reloaded", async () => {
  if (!sb) return;
  identity = await invoke("window_identity");
  sb.clearError();
  sb.update(windowDto(await invoke("get_workspace")));
});
listen("notice",(e) => sb && sb.setError(e.payload));
// Cycling walks loaded tabs only (the live dot); a cold row loads when clicked. From a popped-out
// window, `from` is that tab, which never holds the highlight.
listen("nav-tab", (e) =>
  sb && sb.selectByOffset(e.payload.dir, { liveOnly: true, from: e.payload.from ?? undefined }));
listen("jump-tab", (e) => sb && sb.selectByIndex(e.payload));
listen("find-in-sidebar", () => sb && sb.focusSearch());
// Group ▸ New Group… (⌘N).
listen("focus-new-group", () => {
  const input = document.getElementById("new-group");
  if (input) {
    input.focus();
    input.select();
  }
});
listen("pointer-away", () => sb && sb.pointerAway());
listen("check-update", () => sb && sb.checkForUpdateNow());

mountChrome();
