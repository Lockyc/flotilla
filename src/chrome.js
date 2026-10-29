// flotilla's chrome controller: binds the shared chrome-core ChromeSidebar (the view) to flotilla's
// Tauri backend. Currently mounts an empty sidebar; the model arrives with the DTO.

const { invoke } = window.__TAURI__.core;
// A bare `listen()` is app-wide, not window-scoped; binding to this webview scopes it to this window.
const chromeWebview = window.__TAURI__.webview.getCurrentWebview();
const listen = (event, handler) => chromeWebview.listen(event, handler);

const MIN_W = 160, MAX_W = 520, MAX_FRACTION = 0.4;

let sb = null;

function setSidebarWidth(w) {
  document.getElementById("sidebar").style.width = Math.round(w) + "px";
}

// Report the #content-hole's CSS rect so Rust positions document webviews to match.
function reportRect() {
  const r = document.getElementById("content-hole").getBoundingClientRect();
  invoke("set_hole_rect", { rect: { x: r.x, y: r.y, width: r.width, height: r.height } }).catch(() => {});
}

async function buildDto() {
  const id = await invoke("window_identity");
  const tabs = await invoke("get_tabs");
  return {
    title: id.title,
    colour: id.colour ?? null,
    density: id.density,
    windowDrag: id.sidebar_drag !== false,
    active: null,
    tabs,
  };
}

async function refresh() {
  sb.update(await buildDto());
  reportRect();
}

async function mountChrome() {
  const id = await invoke("window_identity");
  sb = window.ChromeSidebar.mount(
    document.getElementById("sidebar"),
    {
      onSelect() {},
      onResize(width) {
        setSidebarWidth(width);
        reportRect();
      },
    },
    {
      appName: "flotilla",
      storageKey: "flotilla:sidebar-width",
      defaultWidth: id.default_width,
      minWidth: MIN_W,
      maxWidth: MAX_W,
      maxFraction: MAX_FRACTION,
      autoUpdate: id.auto_update !== false,
    }
  );
  await refresh();
  if (!(parseFloat(localStorage.getItem("flotilla:sidebar-width")) > 0)) {
    setSidebarWidth(id.default_width);
  }
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

listen("nav-tab", (e) => sb && sb.selectByOffset(e.payload, { liveOnly: true }));
listen("jump-tab", (e) => sb && sb.selectByIndex(e.payload));
listen("find-in-sidebar", () => sb && sb.focusSearch());
listen("pointer-away", () => sb && sb.pointerAway());
listen("check-update", () => sb && sb.checkForUpdateNow());

mountChrome();
