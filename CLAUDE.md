# agent-diff

Desktop app (Rust 2024 + egui/eframe) to watch a remote workspace where Claude
is working, review what changed, and sync whole files or single hunks between
it and a local git workspace, in both directions. Each local/remote pair is a
workspace tab, with an embedded terminal running `claude` on the remote.

See `README.md` for user-facing behavior; keep it up to date when behavior changes.

## Rules

- **Never write comments.** No `//`, `///` or `//!` comments in code you add or
  change. Use clear names and small functions instead.
- **Don't run the tests, don't build** (no `cargo test`, `cargo build`, `cargo check`): the
  pre-commit hook (`.pre-commit-config.yaml`, installed with `uvx pre-commit install`)
  runs the tests.

## Layout

- `src/main.rs` — CLI args (clap), starts the eframe app.
- `src/engine.rs` — core sync logic: file scope (index of every git repo in
  the workspace, git add/rm in the closest one + remote-only +
  already synced; gitignored folders and the opened ones), baseline snapshots, change origin (remote / local / both),
  rename detection, applying actions with backups and staleness checks. Scans reuse a file's previous
  result while the stamps (size, mtime, ctime, inode) of its local, mirror and base copies are unchanged
  (`scan_path_reusing`); files over 4 MB are compared in chunks, and diff text is read when shown (`Texts`).
  Gitignored files are never synced: listed (as local-only unchanged entries, `gitignored_entry`) but out of
  the scan (also every path under a gitignored folder, even one the baseline still holds: `in_any_folder`), refused by apply / sync / rename / remote save (`refuse_gitignored`), and excluded from the fetch
  by the remote's own git and the local one (`gitignored_rules`, `Remote::gitignored_paths`, an rsync exclude
  list with `--delete-excluded`; `.gitignore` filter as fallback). The first local listing (`scan_local`)
  versions files by their stamps, never reading them. Remote-only mirror files go through
  `git check-ignore` (`without_gitignored`), and the repository walk (`walk_repos`) follows every git
  ignore source (`.gitignore`, `info/exclude`, global excludes). Opening or closing a gitignored folder
  updates the shared lists right away from the UI (`show_gitignored_folder_now` in `session.rs`, one
  `read_dir` per level, `gitignored_folder_children`), then the worker records it (`open_gitignored`
  re-expands git's last ignored listing, `gitignored_listed`, without a rescan).
  The watcher (folder by folder, `watch_tree` / `WatchScope` in `session.rs`) skips them.
- `src/diff.rs` — hunks computation (`similar`) and taking a hunk from either side.
- `src/remote.rs` — remote as `host:path` (rsync over ssh, ControlMaster) or a
  plain directory; mirroring and pushing.
- `src/highlight.rs` — syntax highlighting (syntect, with bat's extra grammars from `two-face`: TypeScript, TSX, TOML…).
- `src/icon.rs` — the app icon, drawn in code (window icon; `assets/agent-diff.png` is exported
  from it, with `assets/agent-diff.desktop`).
- `src/app/` — GUI:
  - `mod.rs` — `AgentDiffApp`, workspace tabs.
  - `workspace.rs` — a workspace pair and its tab state, bulk actions, delete/rename/new file dialogs, and the
    file tabs (`FileTabs`: the main tab follows `selected`, pinned tabs keep their file).
  - `tab_view.rs` — the frame of a running workspace tab: `tab_ui` lays it out from small
    functions (top bar, status bar, terminal area, file list panel (every file: tracked, untracked and gitignored, with gitignored
    folders opened one level at a time), the Changes panel on the right (changed files only), main panel with the editor,
    diff or preview, dialogs), reading the session once into a `Frame` that also collects the
    `Cmd`s to send. `tab_view/explorer.rs` is the file explorer shared by the Files and Changes
    panels (`ExplorerKind`; each has its own `Explorer` in `Tab`: filter, collapsed groups, search;
    `searched_in` is the one whose matches the file view highlights).
  - `session.rs` — background worker per workspace (`Cmd` channel: refresh, apply, reset base…).
  - `file_list.rs`, `file_view.rs` — file list/tree and diff view.
  - `editor.rs` — in-place editing of a local file with margin diff marks.
  - `definitions.rs` — go to definition (Ctrl+B, Ctrl+click, Alt+← back): definitions found by
    name with per-language keywords (`definitions_in`), in the background over the search files;
    the word under the cursor (editor) or the mouse (diff view, `HOVERED_WORD_ID`), and the
    window listing several matches.
  - `quick_open.rs` — the Ctrl+O window: files by name and lines by text (background), arrow
    keys and Enter.
  - `find_in_file.rs` — the Ctrl+F bar: matches in the open file, Enter / F3 between them.
  - `search.rs` — content search in all files (background, case-insensitive) and the match
    highlighting shared by the diff view and the editor (query in egui temp data `QUERY_ID`,
    jump line in `JUMP_ID`).
  - `previews/` — viewer plugins, each behind a Cargo feature (all on by default), shown instead
    of the diff with a 👁 Preview | </> Source switch (`source_switch` in `workspace.rs`).
    `mod.rs` holds the `Preview` trait, `FilePlace` (local root, mirror, remote) and the
    registry (`plugins()`, `preview_for`); the rest of the app only uses those. To add one: a
    module implementing `Preview`, a feature, and an entry in `plugins()`. `hex_view.rs` (always
    built, `hex_for`) is the source view of binary files.
    - `image_view.rs` (feature `images`) — png, jpeg, gif, webp, bmp, ico side by side.
    - `audio_view.rs` (feature `sounds`) — wav (`hound`) and mp3 (`symphonia`): waveform side
      by side, playback with the system's player (`pw-play` / `paplay` / `aplay`, `mpv` /
      `ffplay` / `mpg123`).
    - `model_view.rs` (feature `models`) — glb, gltf (`gltf`): a small CPU rasterizer, side by
      side with a shared orbit camera and mesh statistics.
    - `svg_view.rs` (feature `svg`) — rendered with `resvg` at the panel size, side by side.
    - `rich_text.rs` (features `html` or `markdown`) — the shared document model (`Span`,
      `Block`, `Document`), its rendering, and `DocumentPair` (both versions side by side).
    - `html_view.rs` (feature `html`) — a small built-in HTML reader (no dependency) on an
      element stack, applying the page's CSS from `css.rs` (stylesheets, `style=""`,
      selectors by specificity, colors, fonts, `display: none`), and "open in browser".
    - `markdown_view.rs` (feature `markdown`) — a small CommonMark-ish parser to the same blocks.
    - `script_runner.rs` (features `bash` or `python`) — builds a script's command line from a
      form implementing its `Form` trait (interpreter, arguments, environment, ui) and hands it,
      as a `RunRequest`, to a local or remote shell tab (`run_in_shell` in `terminal.rs`); its own
      process spawning is kept for the forms' tests. Not a preview: its own registry (`script_runners`,
      `runner_for`) draws it as a run bar above the diff or editor (`run_bar` in `tab_view.rs`,
      kept in `Tab::runner`). Each language brings its own parser:
      - `bash_form.rs` (feature `bash`) — getopts, positionals, `${VAR:-default}`, usage.
      - `python_form.rs` (feature `python`) — any other Python script: interpreter, arguments,
        environment (source shown first).
      - `typer_form.rs` (feature `python`) — Typer CLIs: commands (`@app.command`,
        `app.command("x")(f)`, nested `add_typer` sub-apps, `typer.run`), following imports
        into other workspace modules (`Source.read`), callback options, parameters in both `Annotated` and
        `= typer.Option(...)` styles; editable interpreter (`python3` by default).
  - `terminal.rs` — pty + vt100 terminal tabs (Claude, remote shell, local shell), word-editing
    keys, Ctrl+PageUp/Down between its tabs, done notifications (`CommandCapture`: the next
    OSC 7 prompt for shells, quiet output for Claude; `poll_finished`, `notify_desktop`), a new tab of a chosen type (`TerminalPicker`, the app-wide Ctrl+Shift+T modal; `open_new`), and forking a Claude conversation (`fork_tab`; the
    `Fork` edges are saved per workspace in `workspace.rs`).
  - `fork_graph.rs` — the window drawing the fork graph of Claude conversations, and its
    Graphviz export (dot, `dot -Tsvg`).
  - `panes.rs` — the panels (`Pane`: Files, Changes, Main, Terminal), the app shortcuts (NetBeans keymap, `SHORTCUTS`: go to a panel, search, enlarge,
    rename, new file, new terminal, go to definition, back, quick open, find in file; applied in `apply_shortcut` in `tab_view.rs`), and the panels' rects, zoom
    and outline flash.
  - `tabs.rs` — the tab widget shared by workspace and terminal tabs (label + × on one background),
    and dragging a tab onto another to reorder (`dragged_onto`, `move_tab`).
  - `window.rs` — the app's own title bar (no system decorations): move, double-click to
    maximize, window buttons, resizing from the edges.
  - `options.rs` — global options (theme, syntax colors, full view, and the `Layout` defined in
    `workspace.rs`: terminals shown / enlarged, zoomed panel, panels detached into their own
    windows (egui immediate viewports, `pane_windows` in `tab_view.rs`), file list and Changes widths, terminal
    height — one for the app, copied into the shown `Tab` each frame by `tab_ui` and back):
    `Options`, its load/save, and the ⚙ Options menu. Per workspace, in `workspace.rs` and saved
    with it: the file list view options (`ListView`: tree, expand, opened
    folders, opened gitignored folders).
  - `commit.rs` — the commit / commit & push dialog.
  - `git_log.rs` — the git log panel at the bottom of the Changes panel: a background thread runs
    `git fetch --all --prune` then `git log --oneline --all --graph` every 5 s (`GitLog`, kept in `Tab::git_log`).
  - `factory_reset.rs` — the ⚙ Options → Factory reset dialog: closes the shared ssh connections
    and deletes `~/.agent-diff/` (`app_dir` in `engine.rs`), then quits.
    Also the reset of a single workspace (its ⚙ dialog, `reset_workspace` in `mod.rs`): its state
    dirs (`workspace_state_dirs` in `engine.rs`) and terminal output, then a fresh `Tab`.
  - `settings.rs`, `theme.rs` — settings file (`~/.agent-diff/settings`) and themes.

All app data lives in `~/.agent-diff/` (`app_dir` in `engine.rs`), nothing in the workspaces: `settings`,
`workspaces/<id>/` (`state_dir_of`: `mirror/`, `base/`, `backups/`, `pending-push`, `remote-gitignored`),
`terminals/<tab id>/` (shell output and history), `fork-graph/` and `ssh/` (ControlPath sockets, created at start).

## Commands

- Build: `cargo build --release` (`.cargo/config.toml` limits to one job for low-memory machines)
- Without some previews: `cargo build --release --no-default-features --features images,sounds`
  (features: `images`, `svg`, `html`, `markdown`, `sounds`, `models`, `bash`, `python`); every combination must keep
  building when touching `previews/`.
- Test: `cargo test` (run by the pre-commit hook)
- Run: `cargo run --release -- LOCAL REMOTE [--interval SECS]`
