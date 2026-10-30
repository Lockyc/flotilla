#!/usr/bin/env bash
# install.sh — build flotilla from source and install it to /Applications.
# Usage:  bash install.sh
#    or:  curl -fsSL https://raw.githubusercontent.com/Lockyc/flotilla/main/install.sh | bash
#
# The curl URL and the git clone below require the GitHub repo to be public.
#
# Two modes, auto-detected:
#   • IN_REPO     — run from a flotilla checkout: builds from the current working
#                   tree (so local changes are picked up). No clone/pull.
#   • NOT_IN_REPO — otherwise: manages a persistent source clone at ~/.flotilla
#                   (clone if absent, git pull if present) and builds from it.
#
# Never relaunches the app (the caller decides) and never depends on `just`. For
# guided setup with prerequisite installation, use /flotilla:install in Claude Code.
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "flotilla is a macOS-only app; install.sh only runs on macOS." >&2
  exit 1
fi

REPO_URL="https://github.com/Lockyc/flotilla"
INSTALL_DIR="$HOME/.flotilla"

# 1. Hard prerequisites. /flotilla:install offers to install these; the bare
#    script only refuses with a hint (except the Tauri CLI, which it backstops).
missing=0
for c in git cargo; do
  if ! command -v "$c" >/dev/null 2>&1; then
    echo "flotilla: '$c' is required but not found on PATH" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  echo "flotilla: install Rust (https://rustup.rs) and Xcode Command Line Tools" >&2
  echo "          (xcode-select --install), then re-run." >&2
  exit 1
fi

# 2. Resolve the source dir (IN_REPO vs clone at ~/.flotilla).
if [ -f install.sh ] && [ -f src-tauri/tauri.conf.json ]; then
  SRC="$(pwd)"
  echo "→ building from the current flotilla checkout: $SRC"
else
  if [ ! -e "$INSTALL_DIR" ]; then
    echo "→ cloning flotilla into $INSTALL_DIR"
    git clone "$REPO_URL" "$INSTALL_DIR"
  elif [ -d "$INSTALL_DIR/.git" ]; then
    echo "→ updating flotilla clone in $INSTALL_DIR"
    git -C "$INSTALL_DIR" pull --ff-only
  else
    echo "flotilla: $INSTALL_DIR exists but is not a git clone — move it aside and re-run." >&2
    exit 1
  fi
  SRC="$INSTALL_DIR"
fi

# 3. Tauri CLI backstop — a source build needs `cargo tauri`; ship it as a cargo global.
if ! command -v cargo-tauri >/dev/null 2>&1; then
  echo "→ installing the Tauri CLI (cargo install tauri-cli — this takes a while)"
  cargo install tauri-cli --version '^2' --locked
fi

# 4. Build the release bundle.
cd "$SRC"
echo "→ building release bundle (this takes a few minutes)"
( cd src-tauri && cargo tauri build )

# 5. Install the built app into /Applications. flotilla is a Cargo workspace, so the
#    bundle lands under the workspace-root target/, not src-tauri/target/.
bash scripts/install-app.sh "target/release/bundle/macos/flotilla.app"

echo ""
echo "✓ flotilla installed to /Applications/flotilla.app"
echo "  Launch it and paste a Drive folder or page link into the sidebar (⌘N) to start a group."
echo "  Update any time by re-running this installer (it git-pulls + rebuilds)."
