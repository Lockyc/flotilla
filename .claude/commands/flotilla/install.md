You are installing or updating **flotilla** — a macOS workspace where each piece of work is a
group: a head tab (usually a Google Drive folder) with the documents opened from it listed
beneath, each document open in exactly one place.

GitHub: `https://github.com/Lockyc/flotilla`

Build it from source and install `flotilla.app` to `/Applications`. Source lives in a
persistent clone at `~/.flotilla`; updates are `git pull` + rebuild.

---

## Steps

### 1. Detect context

Check whether the current working directory is a flotilla checkout:

```bash
[ -f install.sh ] && [ -f src-tauri/tauri.conf.json ] && echo "IN_REPO" || echo "NOT_IN_REPO"
```

**If IN_REPO:** you will run the local `install.sh` in step 4 (it builds this checkout).
**If NOT_IN_REPO:** you will run the published installer over curl in step 4 (it manages a `~/.flotilla` clone).

### 2. Check prerequisites and offer to install

Probe each build prerequisite:

```bash
command -v git         >/dev/null 2>&1 && echo "git: ok"       || echo "git: MISSING"
command -v cargo       >/dev/null 2>&1 && echo "cargo: ok"     || echo "cargo: MISSING"
command -v cargo-tauri >/dev/null 2>&1 && echo "tauri-cli: ok" || echo "tauri-cli: MISSING"
xcode-select -p        >/dev/null 2>&1 && echo "clt: ok"       || echo "clt: MISSING"
```

For each MISSING prerequisite, use AskUserQuestion to offer to install it. Only run an install
command on confirmation:

- **Xcode Command Line Tools** (also provides `git`): `xcode-select --install`
  (opens a GUI installer — tell the user to finish it, then continue).
- **Rust** (`cargo`): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`,
  then advise sourcing `~/.cargo/env` or restarting the shell.
- **Tauri CLI** (`cargo-tauri`): `cargo install tauri-cli --version '^2' --locked`
  (compiles the CLI — takes a few minutes). `install.sh` backstops this too, so it
  is safe to skip here and let the core install handle it.

flotilla has **no Node/npm dependency** — do not probe for it. If a prerequisite the user
declined to install is still missing (other than the Tauri CLI, which `install.sh` installs
anyway), warn that `install.sh` will refuse to build until it is present, and ask whether to
continue anyway.

### 3. Probe current state

For smarter messaging:

```bash
[ -d /Applications/flotilla.app ] && echo "app: installed" || echo "app: absent"
if [ ! -e ~/.flotilla ]; then echo "src: fresh";
elif [ -d ~/.flotilla/.git ]; then echo "src: clone";
else echo "src: not-a-clone"; fi
pgrep -f "/Applications/flotilla.app/" >/dev/null && echo "running: yes" || echo "running: no"
```

If `src: not-a-clone`, tell the user `~/.flotilla` exists but is not a git clone; `install.sh`
will refuse to touch it. They must move it aside before continuing (NOT_IN_REPO path only).

### 4. Run the core install

**If IN_REPO:**

```bash
PATH="$HOME/.cargo/bin:$PATH" bash install.sh
```

**If NOT_IN_REPO:**

```bash
curl -fsSL https://raw.githubusercontent.com/Lockyc/flotilla/main/install.sh | PATH="$HOME/.cargo/bin:$PATH" bash
```

(The `PATH="$HOME/.cargo/bin:$PATH"` prefix ensures a Rust toolchain / Tauri CLI you may
have just installed via rustup/cargo in step 2 is found — a fresh shell won't have picked
up rustup's profile changes yet.)

Both back the Tauri CLI install if absent, run `cargo tauri build` and install the app to
`/Applications/flotilla.app`. The build takes a few minutes. **If it fails, show the full
output and stop** — do not run later steps.

### 5. Offer to launch

Use AskUserQuestion: **"Launch flotilla now?"**

- **Launch** → `open /Applications/flotilla.app`
- **Not now** — skip.

### 6. Self-install this command

So `/flotilla:install` is available in future Claude Code sessions:

```bash
mkdir -p ~/.claude/commands/flotilla
```

Copy `install.md` verbatim to `~/.claude/commands/flotilla/install.md`. Source it from the
cwd checkout if IN_REPO (`.claude/commands/flotilla/install.md`), otherwise from
`~/.flotilla/.claude/commands/flotilla/install.md` (present after step 4 cloned it).

### 7. Summary

Read the version from `src-tauri/Cargo.toml` (the `version` field) — use the cwd checkout if
IN_REPO, else `~/.flotilla/src-tauri/Cargo.toml`.

Print:

**Installed**
- flotilla vX.Y.Z → `/Applications/flotilla.app` ✓
- Source clone → `~/.flotilla` (NOT_IN_REPO) or "built from checkout" (IN_REPO)

**Next steps**
- Paste a Drive folder or page link into the sidebar (**⌘N**) to open it as a group.
- **Config ▸ Edit Config** opens `~/.config/flotilla/config.toml`, writing a starter first.
- Update any time by re-running `/flotilla:install` (or `curl … | bash`) — it git-pulls and
  rebuilds. The app also updates itself from GitHub releases.
