use super::*;

pub(super) enum Cmd {
    Refresh,
    LocalChanged,
    /// (path, version shown, action, rename info of the entry).
    Apply(Vec<(String, u64, Action, Option<Rename>)>),
    ResetBase,
    /// Create an empty local file, git added.
    CreateLocal { path: String, git_add: bool },
    CreateFolder(String),
    RemoveEmptyFolder(String),
    ImportFiles { folder: String, files: Vec<PathBuf> },
    MakeRemoteIdentical,
    DeleteIgnored(String),
    Unignore(String),
    RenameFolder { from: String, to: String, files: Vec<(String, u64, Option<Rename>)> },
    Rename { path: String, version: u64, rename: Option<Rename>, to: String },
    Sync { path: String, version: u64, origin: Origin, rename: Option<Rename> },
    OpenGitignored(Vec<String>),
    AddToGitignore(String),
    SaveRemote(RemoteSave),
}

#[derive(Default)]
pub(super) struct Shared {
    pub(super) entries: Arc<Vec<FileEntry>>,
    /// Files identical on both sides.
    pub(super) synced: Arc<Vec<FileEntry>>,
    pub(super) repos: Arc<Vec<Repo>>,
    pub(super) untracked: Arc<std::collections::BTreeSet<String>>,
    pub(super) gitignored: Arc<std::collections::BTreeSet<String>>,
    pub(super) gitignored_entries: Arc<Vec<FileEntry>>,
    pub(super) gitignored_dirs: Arc<std::collections::BTreeSet<String>>,
    pub(super) last_fetch: Option<SystemTime>,
    pub(super) fetch_error: Option<String>,
    pub(super) message: Option<(bool, String)>,
    pub(super) busy: bool,
    pub(super) progress: Option<Progress>,
    pub(super) activity: Option<String>,
    pub(super) remote_saved: Option<(u64, Result<bool, String>)>,
    pub(super) failed: Option<String>,
    pub(super) open_gitignored_folders: Option<Vec<String>>,
    pub(super) empty_folders: Arc<std::collections::BTreeSet<String>>,
}

#[derive(Clone)]
pub(super) struct Progress {
    pub(super) step: String,
    pub(super) fraction: f32,
}

const FETCH_SHARE: f32 = 0.1;
const SCAN_REPORT_EVERY: usize = 200;

fn scan_step(done: usize, total: usize) -> String {
    if done == total { "matching renamed files".into() } else { format!("comparing files {done}/{total}") }
}

fn fetch_progress(fetching: crate::remote::Fetching) -> Progress {
    match fetching {
        crate::remote::Fetching::AskingRemoteGit => Progress { step: "fetching the remote: asking its git for the ignored files".into(), fraction: 0.0 },
        crate::remote::Fetching::Listing(listed) => Progress { step: format!("fetching the remote: listing files, {listed} so far"), fraction: 0.0 },
        crate::remote::Fetching::Transfer(transfer) => {
            let files = transfer.files_left.map(|(left, total)| format!(", {}/{total} files", total - left.min(total))).unwrap_or_default();
            Progress { step: format!("fetching the remote: {}{files}", human_bytes(transfer.bytes)), fraction: transfer.fraction }
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}
const APPLY_SHARE: f32 = 0.5;

pub(super) struct Session {
    pub(super) tx: Sender<Cmd>,
    pub(super) shared: Arc<Mutex<Shared>>,
    pub(super) local: String,
    pub(super) remote: String,
    pub(super) state_dir: String,
    pub(super) _watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

pub(super) fn start_session(local: &str, remote: Remote, configure: impl FnOnce(&Engine) + Send + 'static, interval: Duration, ctx: &egui::Context) -> anyhow::Result<Session> {
    let local_root = PathBuf::from(local).canonicalize().map_err(|_| anyhow::anyhow!("local workspace {local} not found"))?;
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let watcher = Arc::new(Mutex::new(None));
    let session = Session {
        tx: tx.clone(),
        shared: shared.clone(),
        local: local_root.display().to_string(),
        remote: remote.display(),
        state_dir: crate::engine::state_dir_of(&local_root, &remote).display().to_string(),
        _watcher: watcher.clone(),
    };
    let watcher = Arc::downgrade(&watcher);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let engine = match Engine::new(local_root, remote) {
            Ok(engine) => engine,
            Err(e) => {
                shared.lock().unwrap().failed = Some(format!("{e:#}"));
                ctx.request_repaint();
                return;
            }
        };
        configure(&engine);
        show_local_files(&engine, &shared, &ctx);
        let Some(slot) = watcher.upgrade() else { return };
        let mut installed = slot.lock().unwrap();
        let failed = match watch_local(&engine, tx, watcher.clone()) {
            Ok((local_watcher, failed)) => {
                *installed = Some(local_watcher);
                failed
            }
            Err(e) => vec![e],
        };
        drop(installed);
        drop(slot);
        if !failed.is_empty() {
            shared.lock().unwrap().message = Some((true, watch_failure(&failed)));
            ctx.request_repaint();
        }
        worker(engine, rx, shared, ctx, interval)
    });
    Ok(session)
}

pub(super) fn show_gitignored_folder_now(shared: &Mutex<Shared>, local_root: &std::path::Path, folder: &str, opened: &std::collections::BTreeSet<String>) {
    let mut s = shared.lock().unwrap();
    let (mut files, mut dirs, mut entries) = ((*s.gitignored).clone(), (*s.gitignored_dirs).clone(), (*s.gitignored_entries).clone());
    if opened.contains(folder) {
        let mut to_open = vec![folder.to_string()];
        while let Some(next) = to_open.pop() {
            for (path, is_dir) in crate::engine::gitignored_folder_children(local_root, &next) {
                if is_dir {
                    dirs.insert(path.clone());
                    if opened.contains(&path) {
                        to_open.push(path);
                    }
                } else if files.insert(path.clone()) {
                    entries.extend(crate::engine::unchanged_local_entry(local_root, path, false));
                }
            }
        }
    } else {
        files.retain(|p| !p.starts_with(folder));
        dirs.retain(|d| d == folder || !d.starts_with(folder));
        entries.retain(|e| !e.path.starts_with(folder));
    }
    s.gitignored = Arc::new(files);
    s.gitignored_dirs = Arc::new(dirs);
    s.gitignored_entries = Arc::new(entries);
}

fn show_local_files(engine: &Engine, shared: &Mutex<Shared>, ctx: &egui::Context) {
    let listed = engine.scan_local().map(|files| (Vec::new(), files));
    let Ok((entries, synced)) = listed else { return };
    let gitignored = engine.gitignored();
    let (gitignored_entries, synced) = split_gitignored(synced, &gitignored);
    let mut s = shared.lock().unwrap();
    s.gitignored_dirs = Arc::new(engine.gitignored_dirs());
    s.untracked = Arc::new(engine.untracked());
    s.gitignored = Arc::new(gitignored);
    s.gitignored_entries = Arc::new(gitignored_entries);
    s.entries = Arc::new(entries);
    s.synced = Arc::new(synced);
    s.repos = Arc::new(engine.repos());
    drop(s);
    ctx.request_repaint();
}

type WatcherSlot = std::sync::Weak<Mutex<Option<notify::RecommendedWatcher>>>;

fn watch_local(engine: &Engine, watch_tx: Sender<Cmd>, slot: WatcherSlot) -> Result<(notify::RecommendedWatcher, Vec<String>), String> {
    let scope = WatchScope { root: engine.local_root.clone(), gitignored_dirs: Arc::new(engine.gitignored_dirs()), linked_git_dirs: Arc::new(engine.linked_git_dirs()) };
    let root = scope.root.clone();
    let on_event = local_event_handler(scope.clone(), watch_tx, slot);
    let mut watcher = notify::recommended_watcher(on_event).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut failed = Vec::new();
    watch_tree(&mut watcher, &scope, &root, &mut failed);
    for git_dir in scope.linked_git_dirs.iter() {
        if let Err(e) = watcher.watch(git_dir, notify::RecursiveMode::NonRecursive) {
            failed.push(format!("{}: {e}", git_dir.display()));
        }
    }
    Ok((watcher, failed))
}

#[derive(Clone)]
struct WatchScope {
    root: PathBuf,
    gitignored_dirs: Arc<std::collections::BTreeSet<String>>,
    linked_git_dirs: Arc<Vec<PathBuf>>,
}

impl WatchScope {
    fn rel(&self, path: &std::path::Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
    }

    fn keeps_folder(&self, rel: &str) -> bool {
        !self.gitignored_dirs.contains(&format!("{rel}/"))
    }

    fn watches_new_folder(&self, path: &std::path::Path) -> bool {
        let Some(rel) = self.rel(path) else { return false };
        let inside_git = rel.split('/').rev().skip(1).any(|name| name == ".git");
        !rel.is_empty() && !inside_git && self.keeps_folder(&rel) && path.is_dir() && !path.is_symlink() && !self.git_ignores(&rel)
    }

    fn git_ignores(&self, rel: &str) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["check-ignore", "-q", "--", rel])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn is_linked_index(&self, path: &std::path::Path) -> bool {
        self.linked_git_dirs.iter().any(|dir| path == dir.join("index") || path == dir.join("HEAD"))
    }

    fn ignores_event(&self, path: &std::path::Path) -> bool {
        let Some(rel) = self.rel(path) else { return false };
        let inside = format!("{rel}/");
        self.gitignored_dirs.range(..=inside.clone()).next_back().is_some_and(|dir| inside.starts_with(dir.as_str()))
    }
}

fn watch_tree(watcher: &mut notify::RecommendedWatcher, scope: &WatchScope, folder: &std::path::Path, failed: &mut Vec<String>) {
    if let Err(e) = watcher.watch(folder, notify::RecursiveMode::NonRecursive) {
        failed.push(format!("{}: {e}", folder.display()));
        return;
    }
    if folder.file_name().is_some_and(|name| name == ".git") {
        return;
    }
    let Ok(read) = std::fs::read_dir(folder) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) && scope.rel(&path).is_some_and(|rel| scope.keeps_folder(&rel)) {
            watch_tree(watcher, scope, &path, failed);
        }
    }
}

fn local_event_handler(scope: WatchScope, watch_tx: Sender<Cmd>, slot: WatcherSlot) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
    move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if ev.kind.is_create() {
            for folder in ev.paths.iter().filter(|p| scope.watches_new_folder(p)) {
                watch_new_folder(slot.clone(), scope.clone(), folder.clone());
            }
        }
        let interesting = ev.paths.iter().any(|p| {
            p.ends_with(".git/index") || scope.is_linked_index(p) || !p.components().any(|c| c == Component::Normal(".git".as_ref())) && !scope.ignores_event(p)
        });
        if interesting && !ev.kind.is_access() {
            let _ = watch_tx.send(Cmd::LocalChanged);
        }
    }
}

fn watch_new_folder(slot: WatcherSlot, scope: WatchScope, folder: PathBuf) {
    std::thread::spawn(move || {
        let Some(slot) = slot.upgrade() else { return };
        if let Some(watcher) = slot.lock().unwrap().as_mut() {
            watch_tree(watcher, &scope, &folder, &mut Vec::new());
        }
    });
}

const SHOWN_WATCH_FAILURES: usize = 3;

fn watch_failure(failed: &[String]) -> String {
    let more = failed.len().saturating_sub(SHOWN_WATCH_FAILURES);
    format!(
        "Local changes are not watched in {} folder(s), so they show only at the next ⟳ refresh (raise fs.inotify.max_user_watches): {}{}",
        failed.len(),
        failed.iter().take(SHOWN_WATCH_FAILURES).cloned().collect::<Vec<_>>().join("; "),
        if more > 0 { format!("; and {more} more") } else { String::new() }
    )
}

pub(super) fn worker(engine: Engine, rx: Receiver<Cmd>, shared: Arc<Mutex<Shared>>, ctx: egui::Context, interval: Duration) {
    let set = |f: &dyn Fn(&mut Shared)| {
        f(&mut shared.lock().unwrap());
        ctx.request_repaint();
    };
    let fetch = || {
        let res = engine.fetch_reporting(&mut |transfer| {
            let progress = fetch_progress(transfer);
            set(&|s| {
                s.activity = Some(progress.step.clone());
                s.progress = Some(progress.clone());
            });
        });
        set(&|s| {
            s.activity = None;
            s.last_fetch = Some(SystemTime::now());
            s.fetch_error = res.as_ref().err().map(|e| format!("{e:#}"));
            s.progress = None;
        });
        res.unwrap_or(false)
    };
    let scan = || {
        set(&|s| s.activity = Some("listing the files to compare".into()));
        let scanned = engine.scan_all_reporting(&mut |done, total| {
            if done % SCAN_REPORT_EVERY == 0 || done == total {
                let step = scan_step(done, total);
                set(&|s| s.activity = Some(step.clone()));
            }
        });
        set(&|s| s.activity = None);
        match scanned {
            Ok((entries, synced)) => {
                let gitignored = engine.gitignored();
                let (gitignored_entries, synced) = split_gitignored(synced, &gitignored);
                let (entries, gitignored_entries, synced, repos) = (Arc::new(entries), Arc::new(gitignored_entries), Arc::new(synced), Arc::new(engine.repos()));
                let (untracked, gitignored, gitignored_dirs) = (Arc::new(engine.untracked()), Arc::new(gitignored), Arc::new(engine.gitignored_dirs()));
                set(&|s| {
                    s.gitignored_dirs = gitignored_dirs.clone();
                    s.untracked = untracked.clone();
                    s.gitignored = gitignored.clone();
                    s.gitignored_entries = gitignored_entries.clone();
                    s.entries = entries.clone();
                    s.synced = synced.clone();
                    s.repos = repos.clone();
                    s.empty_folders = Arc::new(s.empty_folders.iter().filter(|f| engine.local_root.join(f).is_dir()).cloned().collect());
                })
            }
            Err(e) => set(&|s| s.message = Some((true, format!("scan failed: {e:#}")))),
        }
    };
    let mut dirty = true;
    let mut last_fetch: Option<Instant> = None;
    loop {
        if last_fetch.is_none_or(|t| t.elapsed() >= interval) {
            dirty |= fetch();
            last_fetch = Some(Instant::now());
        }
        if dirty {
            scan();
            dirty = false;
        }

        let wait = interval.saturating_sub(last_fetch.map(|t| t.elapsed()).unwrap_or_default());
        let first = match rx.recv_timeout(wait) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if matches!(first, Cmd::LocalChanged) {
            std::thread::sleep(Duration::from_millis(80));
        }
        let cmds: Vec<Cmd> = std::iter::once(first).chain(rx.try_iter()).collect();

        for cmd in cmds {
            match cmd {
                Cmd::LocalChanged => dirty = true,
                Cmd::Refresh => {
                    scan();
                    last_fetch = None;
                }
                Cmd::SaveRemote(save) => {
                    set(&|s| s.busy = true);
                    let outcome = engine.save_remote(&save.path, save.expected.as_deref(), &save.text).map_err(|e| format!("{e:#}"));
                    let message = match &outcome {
                        Ok(true) => Some((false, format!("Saved {} on the remote", save.path))),
                        Ok(false) => None,
                        Err(e) => Some((true, format!("saving {} on the remote failed: {e}", save.path))),
                    };
                    set(&|s| {
                        s.busy = false;
                        s.remote_saved = Some((save.ticket, outcome.clone()));
                        if message.is_some() {
                            s.message = message.clone();
                        }
                    });
                    last_fetch = Some(Instant::now());
                    dirty = true;
                }
                Cmd::Unignore(path) => {
                    let res = engine.remove_from_gitignore(&path);
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(done) => (false, format!("{path}: no longer ignored. {done}")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::DeleteIgnored(path) => {
                    let res = engine.delete_ignored(&path);
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(()) => (false, format!("{path}: deleted locally")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::MakeRemoteIdentical => {
                    set(&|s| {
                        s.busy = true;
                        s.activity = Some("replacing the remote with your git files".into());
                    });
                    let res = engine.make_remote_identical();
                    fetch();
                    last_fetch = Some(Instant::now());
                    set(&|s| {
                        s.busy = false;
                        s.message = Some(match &res {
                            Ok(n) => (false, format!("The remote now holds your {n} git file(s), and nothing else (its .git folders kept)")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::RenameFolder { from, to, files } => {
                    set(&|s| s.busy = true);
                    fetch();
                    last_fetch = Some(Instant::now());
                    let res = engine.rename_folder(&from, &to, &files).and_then(|(pushed, skipped)| engine.push_reporting(&pushed, &mut |_| {}).map(|()| skipped));
                    let new_prefix = format!("{}/", to.trim_end_matches('/'));
                    set(&|s| {
                        s.busy = false;
                        s.empty_folders = Arc::new(s.empty_folders.iter().map(|f| f.strip_prefix(from.as_str()).map_or(f.clone(), |rest| format!("{new_prefix}{rest}"))).collect());
                        s.message = Some(match &res {
                            Ok(skipped) if skipped.is_empty() => (false, format!("Renamed {from} to {to}")),
                            Ok(skipped) => (true, format!("Renamed {from} to {to}, except:\n{}", skipped.join("\n"))),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::ImportFiles { folder, files } => {
                    let res = engine.import_files(&folder, &files);
                    let place = if folder.is_empty() { "the workspace root".to_string() } else { folder.clone() };
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(n) => (false, format!("{n} file(s) copied into {place}, not added to git: right-click → ⇄ Sync to add and push them")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::RemoveEmptyFolder(folder) => {
                    let res = engine.remove_empty_folder(&folder);
                    set(&|s| {
                        s.empty_folders = Arc::new(s.empty_folders.iter().filter(|f| !f.starts_with(folder.as_str())).cloned().collect());
                        match &res {
                            Ok(true) => s.message = Some((false, format!("{folder}: folder removed locally"))),
                            Ok(false) => {}
                            Err(e) => s.message = Some((true, format!("{e:#}"))),
                        }
                    });
                    dirty = true;
                }
                Cmd::CreateFolder(path) => {
                    let res = engine.create_folder(&path);
                    set(&|s| match &res {
                        Ok(()) => {
                            let mut folders = (*s.empty_folders).clone();
                            folders.insert(format!("{}/", path.trim_end_matches('/')));
                            s.empty_folders = Arc::new(folders);
                            s.message = Some((false, format!("{path}: folder created locally. Git keeps no empty folder: it is synced once it holds a file")));
                        }
                        Err(e) => s.message = Some((true, format!("{e:#}"))),
                    });
                }
                Cmd::CreateLocal { path, git_add } => {
                    let gitignored = engine.gitignored_reveal(&path);
                    let res = if git_add && gitignored.is_none() { engine.create_local(&path) } else { engine.create_untracked(&path) };
                    let how = match (&gitignored, git_add) {
                        (Some(_), _) => "gitignored: never synced",
                        (None, true) => "git added",
                        (None, false) => "not added to git",
                    };
                    let exists = engine.local_root.join(&path).exists();
                    if exists {
                        if gitignored.is_some() {
                            engine.note_gitignored(&path);
                        }
                        if res.is_ok() {
                            mark_created(&path, gitignored.is_some(), git_add, &set);
                        }
                        show_files_now(&engine, std::slice::from_ref(&path), &set);
                    }
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(()) => (false, format!("Created {path} ({how})")),
                            Err(e) => (true, format!("{e:#}")),
                        });
                        s.open_gitignored_folders = gitignored.clone().filter(|_| exists);
                    });
                    dirty = true;
                }
                Cmd::Rename { path, version, rename, to } => {
                    set(&|s| s.busy = true);
                    fetch();
                    last_fetch = Some(Instant::now());
                    let res = engine
                        .rename(&path, version, rename.as_ref(), &to)
                        .and_then(|pushed| engine.push_reporting(&pushed, &mut |_| {}));
                    set(&|s| {
                        s.busy = false;
                        s.message = Some(match &res {
                            Ok(()) => (false, format!("Renamed {path} to {to}")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::AddToGitignore(path) => {
                    let res = engine.add_to_gitignore(&path);
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(done) => (false, done.clone()),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::OpenGitignored(folders) => {
                    engine.open_gitignored(folders);
                    let (gitignored, gitignored_dirs, gitignored_entries) = (Arc::new(engine.gitignored()), Arc::new(engine.gitignored_dirs()), Arc::new(engine.gitignored_entries()));
                    set(&|s| {
                        s.gitignored = gitignored.clone();
                        s.gitignored_dirs = gitignored_dirs.clone();
                        s.gitignored_entries = gitignored_entries.clone();
                    });
                }
                Cmd::Sync { path, version, origin, rename } => {
                    set(&|s| s.busy = true);
                    if origin != Origin::Remote {
                        fetch();
                        last_fetch = Some(Instant::now());
                    }
                    let res = engine
                        .sync(&path, version, origin, rename.as_ref())
                        .and_then(|pushed| engine.push_reporting(&pushed, &mut |_| {}).map(|()| pushed));
                    set(&|s| {
                        s.busy = false;
                        s.message = Some(match &res {
                            Ok(pushed) if pushed.is_empty() => (false, format!("{path}: git added, and in sync")),
                            Ok(_) => (false, format!("{path}: git added and synced with the remote")),
                            Err(e) => (true, format!("{e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::ResetBase => {
                    let res = engine.reset_base();
                    set(&|s| {
                        s.message = Some(match &res {
                            Ok(()) => (false, "Local files are now the reference: remaining differences are listed as Claude's".into()),
                            Err(e) => (true, format!("reset failed: {e:#}")),
                        })
                    });
                    dirty = true;
                }
                Cmd::Apply(items) => {
                    let report = |step: String, fraction: f32| {
                        set(&|s| s.progress = Some(Progress { step: step.clone(), fraction }));
                    };
                    set(&|s| s.busy = true);
                    if touches_remote(&items) {
                        report("fetching the remote".into(), 0.0);
                        fetch();
                        last_fetch = Some(Instant::now());
                    }
                    let mut to_push = Vec::new();
                    let mut errors = Vec::new();
                    // Hunk indices shift after applying one, so apply the
                    // highest index first within a file.
                    let mut items = items;
                    items.sort_by_key(|(p, _, a, _)| (p.clone(), std::cmp::Reverse(hunk_index(a))));
                    let mut versions = std::collections::HashMap::new();
                    let mut applied_paths = Vec::new();
                    let total = items.len();
                    for (done, (path, version, action, rename)) in items.into_iter().enumerate() {
                        report(format!("applying {}/{total}", done + 1), FETCH_SHARE + APPLY_SHARE * done as f32 / total.max(1) as f32);
                        let version = *versions.get(&path).unwrap_or(&version);
                        match engine.apply(&path, version, action, rename.as_ref()) {
                            Ok(pushed) => {
                                for p in pushed {
                                    if !to_push.contains(&p) {
                                        to_push.push(p);
                                    }
                                }
                                let current = engine.current_version(&path, rename.as_ref());
                                if rename.is_none() && !applied_paths.contains(&path) {
                                    applied_paths.push(path.clone());
                                }
                                versions.insert(path, current);
                            }
                            Err(e) => errors.push(format!("{e:#}")),
                        }
                    }
                    let applied: Vec<String> = applied_paths.drain(..).collect();
                    show_files_now(&engine, &applied, &set);
                    let pushing = format!("pushing {} file(s)", to_push.len());
                    let pushed = engine.push_reporting(&to_push, &mut |p| report(pushing.clone(), FETCH_SHARE + APPLY_SHARE + (1.0 - FETCH_SHARE - APPLY_SHARE) * p));
                    if let Err(e) = pushed {
                        errors.push(format!("{e:#}"));
                    }
                    set(&|s| {
                        s.busy = false;
                        s.progress = None;
                        s.message = Some(if errors.is_empty() {
                            (false, format!("Done{}", if to_push.is_empty() { String::new() } else { format!(" — pushed {} file(s) to remote", to_push.len()) }))
                        } else {
                            (true, errors.join("\n"))
                        });
                    });
                    dirty = true;
                }
            }
        }
    }
}

fn touches_remote(items: &[(String, u64, Action, Option<Rename>)]) -> bool {
    !items.iter().all(|(_, _, action, _)| matches!(action, Action::TakeRemoteFile | Action::TakeRemoteHunk(_)))
}

fn split_gitignored(entries: Vec<FileEntry>, gitignored: &std::collections::BTreeSet<String>) -> (Vec<FileEntry>, Vec<FileEntry>) {
    entries.into_iter().partition(|e| gitignored.contains(&e.path))
}

fn mark_created(path: &str, gitignored: bool, git_add: bool, set: &dyn Fn(&dyn Fn(&mut Shared))) {
    set(&|s| {
        let add = |set: &Arc<std::collections::BTreeSet<String>>| {
            let mut set = (**set).clone();
            set.insert(path.to_string());
            Arc::new(set)
        };
        if gitignored {
            s.gitignored = add(&s.gitignored);
        } else if !git_add {
            s.untracked = add(&s.untracked);
        }
    });
}

fn show_files_now(engine: &Engine, paths: &[String], set: &dyn Fn(&dyn Fn(&mut Shared))) {
    let scanned: Vec<(String, crate::engine::Scanned)> = paths.iter().filter_map(|p| Some((p.clone(), engine.scan_path(p.clone()).ok()?))).collect();
    if scanned.is_empty() {
        return;
    }
    set(&|s| {
        let others = |list: &Arc<Vec<FileEntry>>| list.iter().filter(|e| !paths.contains(&e.path)).cloned().collect::<Vec<_>>();
        let (mut entries, mut gitignored_entries, mut synced) = (others(&s.entries), others(&s.gitignored_entries), others(&s.synced));
        for (_, scanned) in &scanned {
            match scanned {
                crate::engine::Scanned::Differs(e) => entries.push(e.clone()),
                crate::engine::Scanned::Synced(e) if s.gitignored.contains(&e.path) => gitignored_entries.push(e.clone()),
                crate::engine::Scanned::Synced(e) => synced.push(e.clone()),
                crate::engine::Scanned::Gone => {}
            }
        }
        s.entries = Arc::new(entries);
        s.gitignored_entries = Arc::new(gitignored_entries);
        s.synced = Arc::new(synced);
    });
}

pub(super) fn hunk_index(a: &Action) -> usize {
    match a {
        Action::TakeRemoteHunk(i) | Action::TakeLocalHunk(i) => *i,
        _ => usize::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace(name: &str, files: usize) -> (PathBuf, PathBuf, Engine) {
        let root = std::env::temp_dir().join(format!("agent-diff-session-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (local, remote) = (root.join("local"), root.join("remote"));
        for i in 0..files {
            let rel = format!("pkg{}/file{i}.rs", i % 30);
            let body: String = (0..60).map(|n| format!("line {n} of {rel}\n")).collect();
            for side in [&local, &remote] {
                fs::create_dir_all(side.join(format!("pkg{}", i % 30))).unwrap();
                fs::write(side.join(&rel), &body).unwrap();
            }
        }
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&local).args(args).output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["add", "."]);
        let engine = Engine::open(local.clone(), Remote::Local(remote.clone()), root.join("state")).unwrap();
        (local, remote, engine)
    }

    #[test]
    fn a_pulled_change_shows_up_at_once() {
        let (local, remote, engine) = workspace("f7", 3000);
        let rel = "pkg0/file0.rs";
        let text = fs::read_to_string(remote.join(rel)).unwrap().replace("line 3 of", "LINE three of").replace("line 50 of", "LINE fifty of");
        fs::write(remote.join(rel), text).unwrap();
        engine.fetch().unwrap();
        let entry = engine.scan().unwrap().into_iter().find(|e| e.path == rel).unwrap();

        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker_shared = shared.clone();
        let running = std::thread::spawn(move || worker(engine, rx, worker_shared, egui::Context::default(), Duration::from_secs(3600)));
        let version_of = |shared: &Arc<Mutex<Shared>>| shared.lock().unwrap().entries.iter().find(|e| e.path == rel).map(|e| e.version);
        for _ in 0..500 {
            if version_of(&shared) == Some(entry.version) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(version_of(&shared), Some(entry.version));

        let started = Instant::now();
        tx.send(Cmd::Apply(vec![(rel.to_string(), entry.version, Action::TakeRemoteHunk(0), None)])).unwrap();
        while version_of(&shared) == Some(entry.version) && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let shown_after = started.elapsed();
        println!("pulled change shown after {shown_after:?}");
        assert!(fs::read_to_string(local.join(rel)).unwrap().contains("LINE three of"));
        let after = shared.lock().unwrap().entries.iter().find(|e| e.path == rel).cloned().unwrap();
        assert!(matches!(&after.body, Body::Text(t) if t.diff().hunks.len() == 1));
        assert!(shown_after < Duration::from_millis(100), "{shown_after:?}");
        drop(tx);
        running.join().unwrap();
        let _ = fs::remove_dir_all(local.parent().unwrap());
    }
}
