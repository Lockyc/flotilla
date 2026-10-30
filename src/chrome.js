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

// chrome-core's window DTO: the identity fields plus Rust's `{ tabs, active, collapsed }`.
function windowDto(ws) {
  return {
    title: identity.title,
    colour: identity.colour ?? null,
    density: identity.density,
    windowDrag: identity.sidebar_drag !== false,
    ...ws,
  };
}

// The header slot's field: Enter opens the typed or pasted link as a new group.
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
      // Clicking the active row again returns it to the page it opened with.
      onSelect(id, { wasActive } = {}) {
        invoke(wasActive ? "home_tab" : "select_tab", { id }).catch((err) => sb.setError(String(err)));
      },
      // The live dot: close the tab's webview, keep its row.
      onUnload(id) {
        invoke("unload_tab", { id }).catch((err) => sb.setError(String(err)));
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
      header: newGroupField(),
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
listen("notice", (e) => sb && sb.setError(e.payload));
// Cycling walks loaded tabs only (the live dot); a cold row loads when clicked.
listen("nav-tab", (e) => sb && sb.selectByOffset(e.payload, { liveOnly: true }));
listen("jump-tab", (e) => sb && sb.selectByIndex(e.payload));
listen("find-in-sidebar", () => sb && sb.focusSearch());
listen("pointer-away", () => sb && sb.pointerAway());
listen("check-update", () => sb && sb.checkForUpdateNow());

mountChrome();
