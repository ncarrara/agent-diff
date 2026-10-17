# agent-diff

Watch a remote workspace where Claude is working, review what it changed, and sync whole files or
single hunks with your local git workspace, both ways.

```
agent-diff ~/work/myproject user@devbox:/home/user/myproject
agent-diff                       # or fill in the setup screen
```

- The remote is `host:path` (rsync over ssh, key auth) or a plain directory. Each pair is a tab,
  syncing in the background, with Claude and shell terminals.
- Only git-tracked files and files Claude creates are synced; gitignored files never are.
- A baseline tells who changed what: Claude, you, or both. Pull or push per hunk, file or in bulk;
  backups are kept and stale actions refused.
- **🌿 Worktrees**: a branch in its own local worktree, remote folder, tab and Claude, with your
  `.env` files and dependency setup (`npm ci`, `uv sync`…) chosen in the dialog.
- Editor, previews (images, SVG, HTML, Markdown, audio, 3D), script runner, commit dialog, git log.

App data lives in `~/.agent-diff/` (⚙ Options → Factory reset deletes it).

## Build

```
cargo build --release
cargo build --release --no-default-features --features images,sounds   # fewer previews
```

Features: `images`, `svg`, `html`, `markdown`, `sounds`, `models`, `bash`, `python`.

```
install -Dm755 target/release/agent-diff ~/.local/bin/agent-diff
install -Dm644 assets/agent-diff.png ~/.local/share/icons/hicolor/256x256/apps/agent-diff.png
install -Dm644 assets/agent-diff.desktop ~/.local/share/applications/agent-diff.desktop
```
