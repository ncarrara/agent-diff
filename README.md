# agent-diff

A desktop app (Rust + egui) to watch a remote workspace where Claude is
working, review what it changed, and choose what to sync between it and your
local workspace — whole files or single hunks, in both directions.

```
agent-diff ~/work/myproject user@devbox:/home/user/myproject
agent-diff                       # or fill in the setup screen
agent-diff --interval 5 LOCAL REMOTE
```

The window has its own title bar: the tab bar. Drag its empty part to move
the window, double-click it to maximize or restore, use 🗕 🗖 🗙 on the
right, and resize from the window edges. **⚙ Options** in the tab bar
holds the global settings: the theme and syntax colors. Its **🗑 Factory
reset…** deletes everything the app keeps, after asking (and once no sync
action is running): the whole `~/.agent-diff/` folder (settings, every
workspace's mirror, baseline, backups and pending pushes, shell tab output and
history, the fork graph export, the ssh connection sockets), after closing the
shared ssh connections; then the app quits
and starts empty next time. Local workspaces, remote files and Claude's
conversations on the remote are left alone.

To reset a single workspace instead, open its **⚙** (edit the workspace) and
click **🗑 Reset this workspace…**: after asking (and once no sync action is
running there), it deletes that workspace's data in `~/.agent-diff/workspaces/<id>/`
(mirror, baseline, backups, pending pushes) and the saved output of its shell
tabs, puts its view options, terminal tabs, fork graph and commit choices back
to the defaults, and starts it again from a fresh baseline. Its paths and poll
interval stay; the other workspaces and the shared ssh connections are left
alone.

Each workspace (local + remote pair) opens in its own tab. Add more with
**➕ Workspace**; **×** closes one and resets it (it asks first: its mirror, baseline, backups and saved shell output are deleted, the local and remote files are not touched). Open workspaces, the
active tab, theme and view options are saved in
`~/.agent-diff/settings` and restored at the next start. All tabs keep
syncing in the background, and each tab shows how many files differ.

The remote can be `host:path` / `user@host:path` (rsync over ssh, key auth
required, connection reused via ControlMaster) or a plain directory.
A transfer silent for 60 s is abandoned, and after a failed fetch or push the
shared ssh connection is closed so the next one starts fresh. Files waiting to
be pushed are noted in `…/<id>/pending-push` before the mirror changes: if a
push is interrupted (network, app killed), the next sync first finishes it
(`rsync --update`, so a file Claude changed since is kept), and fetching waits
until it succeeds, so your change is not overwritten.

## How it works

- **Scope**: the local workspace must be, or contain, git repositories:
  every repository inside it is found (the workspace itself, nested ones,
  submodules, and a repository folder its parent ignores, like `lib/` in
  the parent's `.gitignore`). Only these are synced:
  - files in the index of one of these repositories (`git ls-files`, so
    anything you `git add`)
  - new files that exist only on the remote (created by Claude)
  - files you already synced through the tool

  Your local untracked files are listed too (marked **?**, and in **Changes** as your
  additions); **⇄ Sync** `git add`s and pushes one. **Gitignored files are never synced**:
  git's own ignore rules decide (`.gitignore` files, `.git/info/exclude`, your global
  excludes), on both sides. `git add` is detected right away. A file pulled,
  created or deleted locally is `git add`ed / `git rm --cached` in the
  closest repository containing it (none if it is in no repository).
- **git log panel**: at the bottom of the **Changes** panel (drag its top edge to resize), the
  output of `git log --oneline --all --graph --decorate` (last 400 commits) of your local
  workspace, refreshed every 5 seconds after a `git fetch --all --prune` (which also prunes the
  remote branches gone from `origin`, like `git remote prune origin`). Its **⚙** sets how often to
  fetch (0: never) and how often to refresh the log, saved for every workspace. With several repositories,
  a picker chooses which one; a failed fetch shows as **fetch failed** (hover for why) and the log
  still shows.
- **Two file panels**: **Files** on the left lists the workspace's files,
  **Changes** on the right lists only the files that differ, in three groups
  (changed on the remote by Claude, changed on both sides, changed locally by
  you). Both panels work the same way: each has its own **🌲**, **⊞**, **⧉**,
  filter field and search in files (the Changes one searches only the changed
  files); the bulk buttons sit at the top of Changes.
- **File list view** (per workspace, saved with it): **🌲** switches
  between the tree and a flat list (each panel has its own **🌲** and **⊞**;
  the Files panel starts as a tree). The **Files** panel always lists every file:
  tracked ones (changed or not), untracked ones, marked **?**, and gitignored ones,
  marked **🙈**. A gitignored file is never synced (never among your changes nor in
  the ⬆ Push count; Sync, Rename and Delete are disabled for it), but you can open and
  edit your local copy. Gitignored folders (like a build folder) are listed
  closed, marked 🙈: clicking one lists what it holds, one level at a time
  (its subfolders stay closed until clicked), and which ones are open is saved.
  **🗋** creates a new file (see
  **📄 New file…** below) in the folder of the selected file, or at the
  workspace root, and **⊞** (in the tree)
  expands every folder of its panel (and new ones by default); turning it
  off collapses them all again. Folders start collapsed. Which folders and
  groups you opened or closed is saved too, so the list looks the same
  after a restart.
- **🔎 Search in files**: the field under the filter (in each panel) searches the content
  of every listed file, changed or not, untracked and gitignored ones included,
  inside the gitignored folders you opened (case-insensitive, at least 2 characters,
  text files up to 2 MB, your local version when there is one). While it
  has text, the list shows the files that contain it, with their number of
  matches; opening one shows its text whole (its source, even for a file
  that has a preview), with every match highlighted in yellow and the
  current one in orange, and the view moved to it (in the diff, and in the
  editor for an unchanged file). **⏶ / ⏷** (or Enter in the field) go to the previous /
  next matching line, then on to the previous / next file; the field shows
  where you are (e.g. `3/12`).
- **🔍 Filter**: the field under the toolbar keeps only the files whose
  path contains all the words typed (case-insensitive), with the folders
  and groups leading to them opened (your saved folder state is left as it
  is and comes back when the filter is cleared); F7 moves through the
  filtered files only. **×** clears it.
- **An icon shows each file's type**: 🦀 Rust, 🐍 Python, 📜 JS/TS, 📃 other code,
  ⚙ config and data, 📝 text and docs, 🌐 HTML, 🎨 styles, ⌨ shell, 🗄 databases,
  🖼 images, 🔊 sounds, 🎲 3D models, 🔒 lock files, 📄 the rest. Gitignored files and
  folders are light grey and a bit transparent, untracked files light red.
- **Repositories in the file list**: in the tree, a repository's folder is
  shown as **📦 name 🌿 branch** (hover for its path and file count) and is
  never merged into its parent's folder chain. When the workspace itself is
  a repository and contains others, the whole tree sits under
  **📦 (workspace root)**, with the nested repositories inside it where they
  are, so the nesting is visible. In the list, the repository part of each
  path is colored. A file in no repository is marked **⚠**: taking it will
  not `git add` it.
- **App data**: everything the app keeps lives in `~/.agent-diff/`, nothing in
  your workspaces: `settings`, `workspaces/<id>/` per local + remote pair
  (mirror, baseline, backups, pending pushes), `terminals/<tab id>/` (shell
  output and history), `fork-graph/` (the Graphviz export) and `ssh/` (the shared
  ssh connection sockets).
- **Remote mirror**: the remote is mirrored with `rsync` every `--interval`
  seconds into `~/.agent-diff/workspaces/<id>/mirror`. `.git` is excluded, and so is every file git
  ignores on the remote: before each fetch the app asks the remote's git
  (`git ls-files --others --ignored --exclude-standard --directory`, in each
  repository) and excludes that list, together with what your local git ignores. A
  repository missing on the remote is skipped. If git cannot answer there (no git, or not a
  repository), the remote's `.gitignore` files are used too. Files an older fetch pulled
  before they were excluded are dropped from the mirror. While rsync lists the remote files,
  the progress bar counts them. The status bar shows what the background worker is doing
  (asking the remote's git, listing, comparing files n/total). The thin progress bar (its step
  on hover), the small spinners and that status only appear once the work lasts over a second.
  Opening a workspace never blocks the
  window: the baseline, the local file list and the watcher are prepared in
  the background, and the local files are listed before the first fetch is
  done. On a first open the Changes panel stays empty until the first fetch
  ends, which can take a while for a big project: a fetch lasting more than
  a second shows a bar in the toolbar with the percentage, the amount
  received and the files checked so far.
- **Local watch**: the local workspace is watched with inotify, so the view
  updates when you save a file. Gitignored folders (like `target/` or `node_modules/`) and
  `.git` (except its index) are not watched, and new folders are picked up as they
  appear (unless git ignores them). If the system's watch limit is
  reached, a message names the folders left unwatched: their changes show at the
  next ⟳ refresh (raise `fs.inotify.max_user_watches` to fix it).
- **Baseline**: a snapshot of the last state both sides agreed on (initially
  your tracked local files). It decides who changed what:
  - *Changed on remote (Claude)*: only the remote moved
  - *Changed locally (you)*: only your copy moved
  - *Changed on both sides*: both moved
  
  When a file becomes identical on both sides, it becomes the new baseline.
  **📌** (Treat local as reference) makes the current local files the baseline
  again: every remaining difference is then listed as Claude's. Use it to
  start a new task, or after syncing by other means. No file is modified.
- **Renames / moves**: a deletion and an addition on the same side with
  identical or similar content (≥ 50% of lines, as in git) are shown as one
  renamed file (**R**). Its diff shows only real edits. Taking it moves the
  local file (`git add`/`git rm --cached`), and pushing the other way moves
  the remote file back.
- **Actions**:
  - `⬇ Take remote`: pull a hunk or a whole file into your local workspace.
  - `⬆ Push local`: push a hunk or file to the remote, e.g. to undo something
    Claude did there.
  - Bulk buttons, at the top of the **Changes** panel: **⬇ Pull (n)** pulls every Claude-only change, **⬆ Push (n)** pushes every local-only change (n files each), and **⚠ Make remote identical** empties the remote (its `.git` folders kept) and sends every file git tracks locally, nothing else: untracked and gitignored files are not sent, and their remote copies are deleted (uploads, `.env`, virtualenvs…), with no backup. It asks first. The top toolbar has **✔ Commit…**, **⟳** (refresh: compare your local files again at once, then fetch the remote), **⚙** (edit the workspace) and **📌** (treat local as reference); hover a button for what it does.
  - Right-click a file in the list or tree → **🗑 Delete** removes it
    locally (staged in git with `git rm --cached`) and on the remote, after
    asking. The local copy is backed up, the remote one too
    (`…/backups/<timestamp>/remote/`) when it differs.
    The **Delete** key does the same for the selected file (when no text
    field, editor or terminal has the keyboard), with the same confirmation.
  - Right-click a gitignored file or folder → **👁 Unignore** asks git which rule ignores it: a
    rule naming exactly that path is removed from its `.gitignore` (or `.git/info/exclude`), and a
    broader one (`*.log`, `__pycache__/`) gets an exception right after it (`!/path`). When a
    folder above it is ignored too, nothing is changed and the message names that folder's rule;
    a rule from your global excludes is not edited.
  - A gitignored file or folder (right-click it → **🗑 Delete…**, or the **Delete** key on a
    gitignored file) is deleted from your local workspace only, after asking, for good (no
    backup): it is never synced, so the remote is not touched.
  - Right-click a folder → **🗑 Delete…** does the same for every
    file listed in it, after asking; folders left empty are removed locally,
    and so is a folder that lists no file and holds none (like a new empty one).
    Gitignored files in it stay (they are never synced, so never deleted
    on the remote either).
  - Right-click a file → **⇄ Sync** `git add`s it locally, then syncs it
    the way it changed: your version is pushed to the remote, Claude's is
    pulled (a file already the same on both sides is only added). It is
    disabled for a file changed on both sides, and for a gitignored file. For an
    untracked file (**?**), this is how to add it to git and send it to the remote.
  - Right-click a file only you changed → **⬆ Push only** sends your
    version to the remote and leaves git untouched (an untracked file stays untracked).
  - Right-click a file → **✏ Rename…** moves it to a new path (from the
    workspace root) locally (`git rm --cached` + `git add`) and on the
    remote, keeping its sync state. It is refused if the new path exists on
    either side or the file changed since you looked at it.
  - Drag a file onto a folder (or onto **All files** or the panel's empty space, for the
    workspace root) to move it there, like **✏ Rename…**: locally, on the remote and in git.
    Drop files or folders from your file manager onto a folder to copy them into your local
    workspace, untracked (**⇄ Sync** adds and pushes them); a name already there is refused.
    When the desktop does not tell where the drop is, they go to the folder you last clicked.
  - Right-click a group header (like **All files**), or the empty space of the Files or Changes
    panel, for the same menu as a folder, on the whole workspace.
  - Right-click a folder in the tree → **📄 New file…** (or a file, for its
    folder) asks the file name and whether to sync it, then creates it empty in your local
    workspace. **⇄ Sync it** (on by default) `git add`s it, so it is listed as your change, to
    push when ready; off, it stays untracked (**?**; right-click → **⇄ Sync** adds and pushes
    it later). A name git ignores (like `deploy.env` under a `*.env` rule) is only created
    locally, never synced, and the gitignored folders above it are opened so it shows. **⬇ Pull…**,
    **⬆ Push…** and **⚠ Overwrite remote…** do the bulk actions for that folder
    only, after the same confirmation. The confirmation counts the files changed on both sides that a
    pull or push leaves out, and its button is off when nothing would be done.
  - Right-click a folder → **✏ Rename…** gives it a new path (from the workspace root): every
    file listed in it is renamed like a file (locally, on the remote and in git), then what is
    left locally (gitignored files, empty folders) moves too. A file changed since you looked
    at it is left where it is, and the message lists it. Refused if the new path exists or is
    inside the folder. Gitignored files on the remote are never synced, so they stay.
  - Right-click a folder → **📁 New folder…** asks the folder's name and creates it empty in your
    local workspace, where the tree shows it. Git keeps no empty folder: it is synced once it holds a file.
  - Right-click a file or a folder → **🙈 Ignore** adds it, as
    `/path` (`/path/` for a folder) relative to the file, to the closest
    `.gitignore` above it in its git repository (the repository's root one
    if there is none), escaping `* ? [ \`. The `.gitignore` change is then
    yours to sync and commit. A file git already tracks stays tracked (the
    message says so): `git rm --cached` it to stop.
  - **?** on a change sends it as a diff to a Claude terminal (the shown
    one, else the first), with "Explain this modification…". It is sent
    once Claude's prompt is ready. **?** in the file's header (Explain all)
    does the same with every change of the file at once ("Explain all the
    modifications…").
- **File tabs**: above the file, the **≡** main tab follows the file list and
  F7 (which moves it to the next file once a file is done): a click on a
  file opens it there. Double-click a file (or middle-click it, or
  right-click → **📑 New tab**) to keep it in its own
  **📌** tab: F7 there goes through that file's changes only and the tab
  stays on it, even once it is in sync. Clicking a file in the list goes
  back to the main tab; **×** closes a tab. Hovering a tab shows the
  file's path in the workspace.
- **Previews**: images, SVG, HTML, Markdown, sounds and 3D models open
  in a viewer instead of their diff. **👁 Preview | </> Source** above it
  switches to the file itself: the diff (or the editor for an unchanged
  text file), or for binary files a hex dump of both versions side by
  side, with the rows that differ highlighted. Each file opens on its
  preview, except HTML and Markdown files, which open on their source. Binary files with no viewer show the hex dump directly.
- **Private keys** are hidden behind a spoiler (**🔒 Private key**, **👁 Show**; **🔒 Hide**
  covers it again): a file whose text holds a `-----BEGIN … PRIVATE KEY-----` block, an SSH
  key (`id_rsa`, `id_ed25519`… without `.pub`), or a keystore (`.p12`, `.pfx`, `.jks`,
  `.keystore`). Its diff, editor and hex view stay hidden until shown.
- **Images**: PNG, JPEG, GIF, WebP, BMP and ICO files are shown as
  pictures: your version and Claude's side by side, with their pixel and
  file sizes, over a checkerboard so transparency shows (an unchanged image
  is shown once). Files over 64 MB, or pictures over 8192 px on a side, are
  not decoded (a message says so). The Pull / Push file buttons above apply as usual.
- **Sounds**: `.wav` and `.mp3` files show their waveform, your version and
  Claude's side by side, with duration (and the difference), sample rate,
  channels and format. **▶ Play** plays a version with the system's player,
  whichever is installed (`pw-play`, `paplay` or `aplay` for wav; `mpv`,
  `ffplay`, `mpg123`, `pw-play` or `paplay` for mp3); **■ Stop** stops it.
- **SVG**: rendered at the size of the panel (vector, so always sharp),
  your version and Claude's side by side.
- **HTML**: a simplified rendering of each version side by side (headings,
  paragraphs, lists, bold/italic, code, links, quotes, tables; no
  JavaScript), with **🌐 Open in browser** for the real page. The page's
  CSS is applied: `<style>` blocks, local `<link rel="stylesheet">` files
  and `style="…"` attributes, with tag, `.class`, `#id` and descendant
  selectors by specificity, `var(--x)`, and screen `@media` rules (not
  print, dark or `max-width` ones). It sets text and background colors
  (the page background from `html`/`body`), bold, italic, font size
  (px, em, rem, %, pt, keywords), monospace fonts, underline and
  strike-through, and hides `display: none` elements. A page with CSS is
  shown black on white like in a browser, whatever the app's theme; layout
  (margins, flex, grid, positions) is not.
- **Markdown** (`.md`, `.markdown`): rendered the same way, side by side:
  headings (`#` and underlined), paragraphs, bold, italic,
  strike-through, inline code, links, images (as their alt text),
  bullet, numbered and task lists (nested by indentation), quotes, fenced
  code, tables, rules, and a front matter block.
- **Run bar**: scripts keep their diff (or editor); a run bar above it shows
  **▶ Run local**, **▶ Run on remote** and the command line, with the arguments under
  **⚙ Arguments** (folded). Running opens the terminals on a local or remote shell tab
  (the first of that kind, or a new one) and types `cd <workspace> && <command>` there,
  so the output, Ctrl+C and the shell history are the terminal's.
- **Bash scripts** (`.sh`, `.bash`, or a bash shebang): a run bar built from
  the script: its `getopts` options (checkboxes, with a value field for
  options that take one), its positional arguments (named after the
  variables they are assigned to, e.g. `target="$1"`, and pre-filled from
  defaults like `${2:-latest}`), its settings like `${REGION:-eu-west-1}`
  (environment variables), an "extra arguments" field, and its
  `# Usage:` comment. The command line is shown below. **▶ Run local** runs
  your version in the local workspace, **▶ Run on remote** runs Claude's
  version in the remote workspace, each in a shell tab.
- **Other Python scripts** (`.py` files, or a python shebang, with an
  `if __name__ == "__main__":` block; plain modules get no run bar): the same
  run bar with a simple form: the interpreter (`python3`, or e.g. `.venv/bin/python`), the
  arguments and the environment (`NAME=value` words).
- **Python Typer CLIs** (`.py` files, or a python shebang, that import
  `typer`): the same run bar, with a form built from the CLI: a command
  picker (`@app.command()` or `app.command("name")(function)`, sub-apps
  added with `add_typer(..., name=)` at any depth, or the function given to
  `typer.run`), the global options of
  `@app.callback()`, and each parameter of the selected command: arguments
  in order, options by their flag (`--name`, or the declared ones like
  `"--region", "-r"`), booleans as checkboxes (`--x` / `--no-x`, or the
  declared `"--force/--no-force"`), lists as space-separated values.
  Defaults, `help=` (on hover) and required parameters (`*`) come from the
  code, in both the `Annotated[...]` and the `= typer.Option(...)` styles;
  options left at their default are not passed. Sub-apps and command
  functions imported from other modules of the workspace (`from pkg import
  mod`, `import pkg.mod as m`, relative imports, `src/` layouts) are read
  too, locally or from the remote mirror. The command's docstring is
  shown under the picker. A **python** field sets the interpreter
  (`python3`, `.venv/bin/python`, `uv run python`…).
- **3D models**: `.glb` and `.gltf` files are shown in a 3D viewer, your
  version and Claude's side by side with one shared camera (drag to turn,
  wheel to zoom, double-click to reset). Each side shows its triangle
  count and the difference; hover it for meshes, vertices, materials and
  size.
  Node transforms, base colors, vertex colors and base color textures are
  shown, lit from the camera. Rendering is done on the CPU, so very large
  models take a moment to load.
- **Shortcuts** (the NetBeans keymap of JetBrains IDEs), from anywhere,
  even a terminal:
  - **Ctrl+1** or **Alt+Shift+O**: the file list (its filter field gets
    the keyboard; ↑ / ↓ there select the previous / next file).
  - **Ctrl+2**: the Changes panel.
  - **Ctrl+0**: the file view (the editor if it is open, else nothing, so
    ↑ / ↓, Page Up / Page Down and F7 work on the diff).
  - **Ctrl+4**: the terminals (shown if they were minimized).
  - **Ctrl+Shift+F**: search in files.
  - **Shift+Esc**: enlarges the panel you are in to the whole window (the
    terminals as with **🗖**, or the file list, the Changes panel or the file view, with a tag
    in its corner), or restores the layout. The panel you are in is the one
    with the keyboard, else the last one you clicked.
  - **Ctrl+R** renames the selected file and **Ctrl+N** (or the **🗋** icon) creates a new file
    in the folder you last clicked, else in the selected file's folder (git added), when no terminal has the keyboard (there,
    the shell keeps them: history search, next command).
  - **Ctrl+B** goes to the definition of the name under the cursor (in the
    editor) or under the mouse (in the diff view); **Ctrl+click** on a name
    does the same. Definitions are found by name in every file of the
    workspace (classes, functions, structs, traits, types, constants… for
    Rust, Python, JS/TS, Go, Java/Kotlin/C#, C/C++, Ruby, PHP, Swift and
    shell, and methods without a keyword: `load(id) {`, `handle = () => {`, Go
    methods with a receiver): one is opened at its line (in the full file view),
    highlighted, several are listed to pick from. The definition opens in a
    pinned tab (the one already showing its file, if any), so the file you came
    from stays the main tab's. **Alt+←** goes back where you were. Not in a
    terminal, where Ctrl+B stays the shell's.
  - **Ctrl+O** opens a window to go to a file or a line: type part of a
    file name (every word must match, names starting with it first) or
    some text, and the files and the lines containing it (in every file,
    case-insensitive) are listed; ↑ / ↓ choose, Enter (or a click) opens
    it, at that line for a text match, Esc closes. Alt+← comes back.
  - **Ctrl+F** finds a text in the open file (the diff, in its full file
    view, or the editor): the matches are highlighted, Enter / F3 go to the
    next one and Shift+Enter / Shift+F3 to the previous one, with their
    count (2/7); Esc or × closes it.
  
  The panel reached is outlined for a moment. Going to another panel
  restores an enlarged one; going to a panel in its own window brings that
  window to the front.
- **Keyboard** (when no text field or terminal has the keyboard):
  - ↑/↓ scroll the diff or the editor by a line, Page Up/Page Down by a
    screen. While you type in the editor, Page Up/Page Down move the cursor
    by a screen too, keeping its column (with Shift: extending the
    selection).
  - **⏶ / ⏷**, at the start of the file tabs row (so they never move
    while you step), make the previous / next
    change current and scroll to its first line (at the top of the view,
    as when a file opens on its first change), without applying anything; past the
    first / last one they go on to the previous file (on its last change) /
    the next file in the list.
  - **F7**, or the **⏵ Take / Push change n/m and go on** button in the
    file's header, accepts the current change (outlined; click a change to make it
    current) and goes on to the next one, then to the next file once the
    file is in sync. Accepting takes Claude's version for a file changed on
    the remote or on both sides, and pushes yours for a file only you
    changed.
- **Editing**: **✏** (Edit) in a file's header, or double-clicking one of its
  lines, edits your local file in place of the diff. An unchanged file
  opens directly in the editor: click in it and type. The text stays still
  while you type: a mark in the margin shows the lines that differ from the
  remote (green added, orange changed, red removed), updated when you pause.
  - **Ctrl+E** deletes the line under the cursor (every line of the
    selection).
  - Selecting text makes it the primary selection, and the middle mouse button
    pastes the primary selection where you click, as usual on Linux.
  - **✔ Save** (Ctrl+Enter, green) saves and goes back to the diff; Ctrl+S
    saves and keeps editing; **× Cancel** (Esc, red) drops your changes.
    They are disabled until you change something. With no changes, a file
    that has a diff shows **← Diff** (or Esc) to go back to it.
  - Leaving a modified file (another file, Cancel) asks to save or discard.
  - The orange **✏** (Edit remote) edits Claude's version instead, on the remote: the
    margin then compares it with your local file. Saving re-fetches the
    remote first and refuses, with Overwrite / Reload / Keep editing, if
    the file changed there meanwhile; otherwise it writes the file and
    pushes it. The editor waits ("saving to the remote…") until it is done,
    so a refused save never loses your text.
  - If the file changed on disk since you started (a pull, another editor,
    git…), saving asks before overwriting it, or reloads it.
- **Commit**: **✔ Commit…** lists the local repositories with uncommitted
  changes (branch and files), with a checkbox each. A repository you
  untick stays unticked the next times (saved with the workspace) until you
  tick it again. Write one message, then
  **✔ Commit** (Ctrl+Enter) runs `git commit -a` in each checked repository,
  nested ones first, or **⬆ Commit & push** also runs `git push` in each
  (setting the upstream on `origin` for a branch that has none). Git never
  prompts: a push that needs a password fails with its error instead.
  Changes still only on the remote are not included: pull them first.
- **Progress**: while a pull or push runs, a bar in the toolbar shows its
  step and overall percentage: fetching the remote, applying each file, then
  pushing to the remote (the percentage reported by rsync; with an rsync
  older than 3.1 the push shows no intermediate progress).
- **Quitting while syncing**: closing the app (🗙, or the desktop's close)
  while a pull, push or commit is running asks first: **Quit now** (stops
  it midway), **Quit when finished** (the app closes by itself when it is
  done) or **Keep working**.
- **Messages**: the result of the last action (or its error) stays in the
  status bar at the bottom; **📋** copies it and **×** clears it.
- **Safety**:
  - Before an action that changes the remote (push, delete), the remote is
    re-fetched. If the file changed since you looked at it, on either side,
    the action is refused. A pull takes exactly the version you reviewed,
    so it does not wait for a fetch.
  - Local files that get overwritten or deleted are first copied to
    `…/<id>/backups/<timestamp>/`.

- **Panels in their own windows**: **⧉** on a panel (the terminals' bar,
  the file list toolbar, the Changes panel toolbar, the file tabs row of the file view) opens that panel in a separate window of its own, to
  put it on another screen for instance; the main window gives its space
  to the others (or says where they are, when all are out). **⧉** again in
  that window, or closing it, puts the panel back. Its shortcut (Ctrl+1,
  Ctrl+2, Ctrl+0, Ctrl+4) brings that window to the front. Which panels are out is saved (they reopen with the app). The
  windows stay open when you switch workspace tabs and show the panel of
  the workspace shown, with its name in their title.
- **Layout**: one layout for the whole app, saved with the settings (not
  per workspace): whether the terminals are shown or enlarged, which panel
  is enlarged with Shift+Esc, which panels are in their own windows, the
  width of the file list and of the Changes panel, and the height of the terminals. Switching
  workspace tabs keeps it.
- **Terminals**: **⌨** (Terminal) opens a panel at the bottom on a tab of
  the kind you pick (an existing one, else a new one): **✳ Claude**,
  **🖧 Remote shell** or **🖥 Local shell**. It also hides or shows the
  panel. Claude runs in the remote workspace (`ssh -t host`, then `cd` into
  the remote path, in an interactive login shell so your PATH applies; a
  plain directory remote runs it locally there). When Claude exits you get a
  shell.
  - Each terminal has its own Claude conversation ID, saved with the
    workspace, so reopening the app or **⟲ Reconnect** (after a lost
    connection; it asks first) resumes that exact conversation, whatever
    other Claude sessions run in the same remote folder.
  - **➕** opens a new tab: **✳ Claude** (another Claude on the same
    workspace, with its own conversation), **🖧 Remote shell** (a shell in
    the remote workspace) or **🖥 Local shell** (a shell in your local
    workspace). Shell tabs are saved too, and start a new shell under the
    previous one's output (its last 2 MB, replayed below a "restored from
    the previous session" line, which goes under the last line with text,
    clearing what was below it, so the new prompt starts on a clean line). A local shell tab also keeps its own
    command history (`HISTFILE`, unless your shell's startup files set
    it), so ↑ brings back that tab's commands. A shell tab also reopens in
    the folder it was last in: the shell reports its folder (OSC 7; bash
    does it through a `PROMPT_COMMAND` the tab sets, unless your startup
    files replace it; other shells need to send OSC 7 themselves). These live in
    `~/.agent-diff/terminals/<tab id>/`. **×** ends a tab (it
    asks first) and deletes them; a Claude's conversation stays on the
    remote (`claude --resume <id>`, shown when hovering the tab). A tab resumes its
    conversation when it is saved on the remote (`~/.claude/projects/*/<id>.jsonl`), and
    starts it under that ID otherwise.
  - Double-click a tab (or right-click → **✏ Rename**) to rename it;
    Enter or clicking elsewhere keeps the name, Escape cancels, an empty
    name restores `claude N`. Names are saved with the workspace.
  - **⑂ Fork** (or right-click a Claude tab → **⑂ Fork**) opens a new
    tab on a copy of the shown conversation, with its whole history, which
    then goes its own way (`claude --resume <id> --fork-session`, under a
    new ID the app saves, so it resumes like any tab). The original keeps
    running untouched. A conversation with nothing sent yet is not saved on the
    remote, so it cannot be forked: the new tab says so and starts a new one.
  - **🌳 Forks** shows the graph of the forked conversations (saved with
    the workspace): one box per conversation, an arrow from each to its
    forks, the shown one highlighted. Click a box to show its tab, or to
    reopen a closed conversation in a new tab; right-click it to fork it or
    copy its ID. **📋 Copy as dot** copies the graph in Graphviz's dot
    language and **🖼 Open as SVG** renders it with `dot -Tsvg` and opens
    it.
  - **🗋 Clear** restarts the shown tab on a new conversation (it asks
    first).
    Use it instead of `/clear`: the conversation `/clear` starts gets an
    ID the app does not know, so it would not be resumed.
  - In a shell tab (remote or local), **📋 Last output** copies the output
    of the last command you ran (from your Enter up to the next prompt,
    without colors). With a shell that emits the standard OSC 133 markers
    the boundaries are exact; otherwise a prompt spanning several lines
    leaves its first lines in the copy.
  - Drag to select text (double-click selects a word), then **Ctrl+C** or
    **Ctrl+Shift+C** copies it (or right-click → **📋 Copy**). With nothing
    selected, Ctrl+C is sent to the program (interrupt) and Ctrl+Shift+C
    does nothing. Copying keeps the selection, as in a text field, so you
    can copy it again; a click, Esc or any other key drops it. Ctrl+V pastes. As usual on Linux, selecting text also
    makes it the primary selection, and the middle mouse button pastes the
    primary selection (text selected in any app).
  - Word editing like in a text field: Ctrl+←/→ (or Alt) jumps a word
    (sent as `Esc b` / `Esc f`), Ctrl+Backspace deletes the word before the
    cursor (`Ctrl+W`), Ctrl+Delete the one after it (`Esc d`). Bash, other
    readline programs and Claude's prompt all understand these.
  - Selecting with the keyboard, like in a text field: Shift+←/→ selects
    characters from the cursor, Ctrl+Shift+←/→ whole words, Shift+Home/End
    up to the start / end of the line (the program moves its cursor and the
    selection follows it). Ctrl+C copies the selection and keeps it; Backspace or Delete
    erases it, and typing or pasting replaces it (the app sends that many
    Backspaces or Deletes first). Any other key drops the selection. It
    works on the line the program is editing (a shell or Claude's prompt).
  - Choosing a terminal tab (clicking it, a new tab from **➕**, **⑂ Fork**,
    a box of the fork graph) gives it the keyboard, so you can type at once.
  - Drag a tab onto another to move it there; workspace tabs move the same
    way, and the order is saved.
  - **Done notifications**: when a command that ran for 10 seconds or more
    ends in a shell tab, local or remote (the shell's next prompt), or when
    Claude answers after 20 seconds or more (its output stops for 3 seconds
    after you pressed Enter), a tab you are
    not looking at gets a green **✔** until you show it, and if the app's
    window is not focused a desktop notification (`notify-send`) says which
    tab, in which workspace, and how long it took.
    Until then, the tab shows a spinning loading icon (from the Enter that
    started it).
  - **Ctrl+PageUp / Ctrl+PageDown** go to the previous / next terminal tab
    (wrapping around), keeping the keyboard in the terminal.
  - **Ctrl+Shift+T**, from anywhere in the workspace, asks which terminal to
    open (Claude, remote shell or local shell; the shown one's type is
    preselected): ↑ ↓ to choose, Enter to open it, showing the terminals if
    hidden, with the keyboard in it; Esc to cancel. It works in the
    terminals' own window too, where the choice is shown.
  - Click a terminal to type. The mouse wheel (or Shift+PageUp/PageDown)
    scrolls back through the output; full-screen programs and programs
    that use the mouse get the wheel themselves. **🗕** minimizes the panel
    to its header bar (tabs and buttons stay visible, the sessions keep
    running); **🗗** or clicking a tab shows it again. Closing the
    workspace tab ends them.
  - **🗖** enlarges the terminals to the whole window, hiding the file list
    and diff; **🗗** puts them back at the bottom.

Pushes and remote deletions use
`rsync --files-from --delete-missing-args` and touch only the selected files.

## Build

```
cargo build --release
```

The previews are optional Cargo features, all on by default: `images`,
`svg`, `html`, `markdown`, `sounds` (wav, mp3), `models` (glb, gltf), `bash` (the
script runner) and `python` (the Python script and Typer CLI runners). Leave some out to build
faster and with fewer dependencies, e.g. without 3D models:

```
cargo build --release --no-default-features --features images,sounds
```

Files of a left-out kind are then shown as plain binary files.

To add it to your desktop's applications, with its icon (on Wayland the
taskbar icon comes from this desktop entry):

```
install -Dm755 target/release/agent-diff ~/.local/bin/agent-diff
install -Dm644 assets/agent-diff.png ~/.local/share/icons/hicolor/256x256/apps/agent-diff.png
install -Dm644 assets/agent-diff.desktop ~/.local/share/applications/agent-diff.desktop
```

The icon is drawn in code (`src/icon.rs`); regenerate the PNG with
`cargo test --release export_icon -- --ignored`.

`.cargo/config.toml` limits the build to one job because of low-memory
machines; delete it to build faster.
