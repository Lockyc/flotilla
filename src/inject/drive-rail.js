// Injected into every content webview; acts only on Google Drive's top frame. Narrows Drive's left
// navigation column to an icon rail, which opens over the file list while hovered or focused.
// Drive's class names are generated, so the column is found by shape: the widest ancestor of the
// New button that is still a narrow, near-full-height strip at the window's left edge. Anything
// else leaves the page untouched.
(function () {
  if (location.hostname !== "drive.google.com" || window.top !== window) return;

  var RAIL = 64;
  var ATTR = "data-flotilla-rail";
  var CSS =
    "[" + ATTR + "]{width:" + RAIL + "px!important;min-width:" + RAIL + "px!important;" +
    "max-width:" + RAIL + "px!important;flex:0 0 " + RAIL + "px!important;" +
    "overflow:hidden!important;position:relative!important;z-index:20!important;" +
    "-webkit-mask-image:linear-gradient(to right,#000 " + (RAIL - 16) + "px,transparent " + RAIL + "px);" +
    "mask-image:linear-gradient(to right,#000 " + (RAIL - 16) + "px,transparent " + RAIL + "px)}" +
    "[" + ATTR + "]>*{width:var(--flotilla-rail-w)!important;min-width:var(--flotilla-rail-w)!important}" +
    "[" + ATTR + "]:hover,[" + ATTR + "]:focus-within{overflow:visible!important;" +
    "-webkit-mask-image:none;mask-image:none}" +
    "[" + ATTR + "]:hover>*,[" + ATTR + "]:focus-within>*{background:var(--flotilla-rail-bg)!important;" +
    "box-shadow:4px 0 12px rgba(0,0,0,.25)}";

  var column = null;

  function newButton() {
    var b = document.querySelector('[guidedhelpid="new_menu_button"]');
    if (b) return b;
    var buttons = document.querySelectorAll("button,[role=button]");
    for (var i = 0; i < buttons.length; i++) {
      if (buttons[i].textContent.trim() === "New") return buttons[i];
    }
    return null;
  }

  function find() {
    var el = newButton();
    if (!el) return null;
    var limit = window.innerWidth * 0.4;
    while (el.parentElement && el.parentElement !== document.body &&
           el.parentElement.getBoundingClientRect().width < limit) {
      el = el.parentElement;
    }
    var r = el.getBoundingClientRect();
    var strip = r.left < 40 && r.width >= 150 && r.width < limit &&
                r.height > window.innerHeight * 0.6;
    return strip ? el : null;
  }

  function background(el) {
    for (; el; el = el.parentElement) {
      var c = getComputedStyle(el).backgroundColor;
      if (c && c !== "transparent" && !/rgba\(.*,\s*0\)$/.test(c)) return c;
    }
    return getComputedStyle(document.body).backgroundColor;
  }

  function apply() {
    if (column && column.isConnected) return;
    var el = find();
    if (!el) return;
    el.style.setProperty("--flotilla-rail-w", el.getBoundingClientRect().width + "px");
    el.style.setProperty("--flotilla-rail-bg", background(el));
    el.setAttribute(ATTR, "");
    column = el;
  }

  function start() {
    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);
    var pending = false;
    new MutationObserver(function () {
      if (pending) return;
      pending = true;
      requestAnimationFrame(function () {
        pending = false;
        apply();
      });
    }).observe(document.body, { childList: true, subtree: true });
    apply();
  }

  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
