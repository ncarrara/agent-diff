# agent-diff

A desktop app (Rust + egui) to watch a remote workspace where Claude is working, review what it
changed, and sync whole files or single hunks between it and your local git workspace, in both
directions.

```
agent-diff ~/work/myproject user@devbox:/home/user/myproject
agent-diff                       # or fill in the setup screen
agent-diff --interval 5 LOCAL REMOTE
```

The remote is `host:path` (rsync over ssh, key auth, one shared ControlMaster connection) or a
plain directory. Each local + remote pair is a workspace tab; every tab keeps syncing and its
terminals keep running in the background. A tab shows its branch, how many files differ, and a
spinner / green **✔** while its Claude works / once it answered.

## How it works

- **Scope**: the local workspace must be, or contain, git repositories (nested ones and
  submodules included). Synced: tracked files, new files Claude created on the remote, and
  files already synced. Untracked files are listed (**?**); **gitignored files are never
  synced** (git's own rules, on both sides). Pulled, created or deleted files are
  `git add`ed / `git rm --cached` in the closest repository.
- **Mirror**: the remote is rsynced every `--interval` seconds into
  `~/.agent-diff/workspaces/<id>/mirror` (without `.git` and gitignored files). The local
  workspace is watched with inotify.
- **Baseline**: the last state both sides agreed on decides who changed what: *Changed on
  remote (Claude)*, *Changed locally (you)*, *Changed on both sides*. **📌** makes the current
  local files the baseline again. Similar deletion + addition pairs show as renames (**R**).
- **Safety**: before changing the remote it is re-fetched, and an action on a file changed
  since you looked at it is refused. Overwritten local files are backed up in
  `…/<id>/backups/`. Interrupted pushes are finished before the next fetch.

## Reviewing and syncing

- **Files** (left) lists every file as a tree or flat list; **Changes** (right) only the
  differing ones, grouped by origin. Each has a filter, search in files and **⊞** expand all.
  Gitignored folders open one level at a time.
- **⬇ Take remote** / **⬆ Push local** per hunk or file; **⬇ Pull (n)**, **⬆ Push (n)** and
  **⚠ Make remote identical** in bulk. **F7** accepts the current change and moves on.
- Right-click a file or folder: Sync, Push only, Rename, Delete, New file / folder, Ignore /
  Unignore, bulk actions on a folder. Drag files onto a folder to move them.
- **?** on a change sends it to a Claude terminal, asking to explain it.
- **✏** edits your local file in place (margin marks show differences with the remote); the
  orange **✏** edits Claude's version on the remote.
- **✔ Commit…** commits (and optionally pushes) in each repository with changes.
- **git log** panel at the bottom of Changes: `git fetch --all --prune` and `git log --graph`
  every 5 s (configurable), only while shown; worktrees of one repository share one fetch.
- **Previews** (👁 Preview | </> Source): images, SVG, HTML, Markdown, wav / mp3, glb / gltf,
  hex view for other binary files. Private keys are hidden behind a spoiler.
- **Run bar** for scripts: Bash (`getopts`, positionals, `${VAR:-default}`), Python and Typer
  CLIs get a form, then run locally or on the remote in a shell tab.

## Worktrees

**🌿 → New worktree…** runs `git worktree add` (existing branch, tracked remote branch, or new
branch from a base), creates the remote folder, copies the tracked files into it, and opens the
pair in a new tab with its own Claude. Folders are siblings: `~/work/proj` ⇄ `host:/srv/proj`
gives `~/work/proj.wt/feat-x` ⇄ `host:/srv/proj.wt/feat-x`. Git stays local: Claude edits plain
files, you review and commit in the local worktree.

**🌿 → Remove this worktree…** closes the tab and runs `git worktree remove` (the branch is
kept), optionally deleting the remote folder (only inside a `.wt` folder). It warns about
uncommitted and unpulled changes first.

## Terminals

**⌨** opens the terminal panel: **✳ Claude** (runs in the remote workspace), **🖧 Remote shell**,
**🖥 Local shell**. Each Claude tab keeps its own conversation ID and resumes it on restart or
**⟲ Reconnect**. **⑂ Fork** copies a conversation into a new tab, **🌳 Forks** shows the fork
graph (exportable as dot / SVG), **🗋 Clear** starts a new conversation (use it instead of
`/clear`). Shell tabs keep their output, history and folder across restarts. Long commands and
Claude answers raise a **✔** and, when the window is unfocused, a desktop notification.

## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+1 / Ctrl+2 / Ctrl+0 / Ctrl+4 | Files / Changes / file view / terminals |
| Shift+Esc | enlarge the current panel, or restore |
| Ctrl+O | go to a file or a line |
| Ctrl+F, Enter / F3 | find in the open file |
| Ctrl+Shift+F | search in files |
| Ctrl+B, Ctrl+click / Alt+← | go to definition / back |
| Ctrl+R / Ctrl+N | rename / new file |
| F7, ⏶ / ⏷ | accept the change and go on / previous, next change |
| Ctrl+Shift+T, Ctrl+PageUp / PageDown | new terminal, previous / next terminal |
| Ctrl+Enter / Ctrl+S / Esc | in the editor: save and close / save / cancel |

**⧉** on a panel moves it to its own window. The layout is saved for the whole app.

## App data

Everything lives in `~/.agent-diff/`: `settings`, `workspaces/<id>/` (mirror, baseline,
backups, pending pushes), `terminals/<tab id>/`, `fork-graph/`, `ssh/`. A workspace's **⚙** →
**🗑 Reset this workspace…** deletes its data; **⚙ Options → 🗑 Factory reset…** deletes it all.
Local and remote files and Claude's conversations are never touched.

## Build

```
cargo build --release
cargo build --release --no-default-features --features images,sounds   # fewer previews
```

Features (all on by default): `images`, `svg`, `html`, `markdown`, `sounds`, `models`, `bash`,
`python`. `.cargo/config.toml` limits the build to one job for low-memory machines.

Desktop entry:

```
install -Dm755 target/release/agent-diff ~/.local/bin/agent-diff
install -Dm644 assets/agent-diff.png ~/.local/share/icons/hicolor/256x256/apps/agent-diff.png
install -Dm644 assets/agent-diff.desktop ~/.local/share/applications/agent-diff.desktop
```

The icon is drawn in code (`src/icon.rs`); regenerate the PNG with
`cargo test --release export_icon -- --ignored`.
