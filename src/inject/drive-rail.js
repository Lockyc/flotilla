// Injected into every content webview; acts only on Google Drive's top frame. Narrows Drive's left
// navigation to an icon rail, which opens over the file list while hovered or focused.
// Drive's shell is a grid whose first column holds two cells, the New button's and the nav list's,
// found by their `guidedhelpid`s (Drive's class names are generated). The cells shrink to the rail
// and the content cell beside them takes the freed width through a negative margin, so Drive's own
// grid template is never rewritten. Anything else leaves the page untouched.
(function () {
  if (location.hostname !== "drive.google.com" || window.top !== window) return;

  var RAIL = 64;
  var GRID = "data-flotilla-rail-grid";
  var CELL = "data-flotilla-rail";
  var CONTENT = "data-flotilla-rail-content";
  var OPEN = "[" + GRID + "]:has([" + CELL + "]:hover,[" + CELL + "]:focus-within) [" + CELL + "]";
  var CSS =
    "[" + CELL + "]{width:" + RAIL + "px!important;overflow:hidden!important;" +
    "position:relative!important;z-index:3!important;" +
    "-webkit-mask-image:linear-gradient(to right,#000 " + (RAIL - 16) + "px,transparent " + RAIL + "px);" +
    "mask-image:linear-gradient(to right,#000 " + (RAIL - 16) + "px,transparent " + RAIL + "px)}" +
    OPEN + "{width:auto!important;overflow:visible!important;-webkit-mask-image:none;mask-image:none;" +
    "background:var(--flotilla-rail-bg)!important}" +
    "[" + CONTENT + "]{margin-left:var(--flotilla-rail-margin)!important}";

  var marked = [];

  function clear() {
    marked.forEach(function (m) {
      m[0].removeAttribute(m[1]);
    });
    marked = [];
  }

  function mark(el, attr) {
    el.setAttribute(attr, "");
    marked.push([el, attr]);
  }

  function background(el) {
    for (; el; el = el.parentElement) {
      var c = getComputedStyle(el).backgroundColor;
      if (c && c !== "transparent" && !/rgba\(.*,\s*0\)$/.test(c)) return c;
    }
    return "Canvas";
  }

  function apply() {
    if (marked.length && marked.every(function (m) { return m[0].isConnected; })) return;
    clear();
    var newButton = document.querySelector('[guidedhelpid="new_menu_button"]');
    var nav = document.querySelector('[guidedhelpid="main_nav"]');
    if (!newButton || !nav) return;
    var grid = nav.parentElement;
    while (grid && !grid.contains(newButton)) grid = grid.parentElement;
    if (!grid || getComputedStyle(grid).display !== "grid") return;
    var cell = function (el) {
      while (el.parentElement !== grid) el = el.parentElement;
      return el;
    };
    var cells = [cell(newButton), cell(nav)];
    var g = grid.getBoundingClientRect();
    var edge = Math.max(cells[0].getBoundingClientRect().right, cells[1].getBoundingClientRect().right);
    if (edge - g.left < RAIL * 2) return;
    var content = null;
    var widest = 0;
    for (var i = 0; i < grid.children.length; i++) {
      var r = grid.children[i].getBoundingClientRect();
      if (Math.abs(r.left - edge) < 8 && r.width > widest) {
        content = grid.children[i];
        widest = r.width;
      }
    }
    if (!content) return;
    var margin = parseFloat(getComputedStyle(content).marginLeft) || 0;
    grid.style.setProperty("--flotilla-rail-bg", background(cells[1]));
    grid.style.setProperty("--flotilla-rail-margin",
      margin - (content.getBoundingClientRect().left - g.left - RAIL) + "px");
    mark(grid, GRID);
    mark(cells[0], CELL);
    mark(cells[1], CELL);
    mark(content, CONTENT);
  }

  function start() {
    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);
    var pending = false;
    var schedule = function (reset) {
      if (reset) clear();
      if (pending) return;
      pending = true;
      requestAnimationFrame(function () {
        pending = false;
        apply();
      });
    };
    new MutationObserver(function () {
      schedule(false);
    }).observe(document.body, { childList: true, subtree: true });
    // The freed width is measured; a resize can change Drive's column, so it is measured again.
    window.addEventListener("resize", function () {
      schedule(true);
    });
    apply();
  }

  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
