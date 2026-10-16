use crate::diff::{self, Hunk};
use crate::remote::Remote;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TEXT: usize = 4 * 1024 * 1024;
const MAX_FUZZY_RENAME: u64 = 1024 * 1024;
const STREAM_CHUNK: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    /// Moved to another path (possibly modified too).
    Renamed,
}

/// A file moved on one side: it lives at `local_path` locally and at
/// `remote_path` on the remote.
#[derive(Clone, Debug, PartialEq)]
pub struct Rename {
    pub local_path: String,
    pub remote_path: String,
    /// Old path (the one the file was moved from).
    pub from: String,
    /// Percentage of identical lines (100 for a pure move).
    pub similarity: u8,
}

/// Minimum similarity for a delete + add pair to count as a rename (git's default).
const RENAME_THRESHOLD: f32 = 0.5;

/// Which side diverged from the baseline (the last state both sides agreed on).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Changed on the remote only (i.e. by Claude).
    Remote,
    /// Changed locally only (i.e. by you).
    Local,
    /// Changed on both sides.
    Both,
    /// Identical on both sides (only listed when showing unchanged files).
    Synced,
}

#[derive(Clone, Debug)]
pub enum Body {
    Text(Texts),
    Binary,
    /// Identical on both sides: the content is read when displayed.
    Unchanged,
}

#[derive(Clone, Debug)]
pub struct Texts {
    files: Option<(PathBuf, PathBuf)>,
    diff: std::sync::Arc<std::sync::OnceLock<TextDiff>>,
}

#[derive(Debug, Default)]
pub struct TextDiff {
    pub hunks: Vec<Hunk>,
    pub local: String,
    pub remote: String,
}

impl Texts {
    pub fn new(local: impl Into<String>, remote: impl Into<String>) -> Self {
        let (local, remote) = (local.into(), remote.into());
        let diff = TextDiff { hunks: diff::hunks(&local, &remote), local, remote };
        Texts { files: None, diff: std::sync::Arc::new(diff.into()) }
    }

    fn read_when_shown(local: PathBuf, remote: PathBuf) -> Self {
        Texts { files: Some((local, remote)), diff: Default::default() }
    }

    pub fn diff(&self) -> &TextDiff {
        self.diff.get_or_init(|| match &self.files {
            Some((local, remote)) => read_text_diff(local, remote),
            None => TextDiff::default(),
        })
    }
}

fn read_text_diff(local: &Path, remote: &Path) -> TextDiff {
    let small = |p: &Path| file_size(p).is_none_or(|size| size <= MAX_TEXT as u64);
    if !small(local) || !small(remote) {
        return TextDiff::default();
    }
    let (l, r) = (read(local), read(remote));
    match (as_text(&l), as_text(&r)) {
        (Some(lt), Some(rt)) => TextDiff { hunks: diff::hunks(lt, rt), local: lt.into(), remote: rt.into() },
        _ => TextDiff::default(),
    }
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    /// For a rename, the new path.
    pub path: String,
    pub rename: Option<Rename>,
    pub origin: Origin,
    pub local_change: Option<Change>,
    pub remote_change: Option<Change>,
    pub local_exists: bool,
    pub remote_exists: bool,
    pub body: Body,
    /// Fingerprint of (local, remote) contents, so actions are only applied to
    /// the version the user actually looked at.
    pub version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    TakeRemoteFile,
    TakeLocalFile,
    TakeRemoteHunk(usize),
    TakeLocalHunk(usize),
    /// Delete the file locally (staged in git) and on the remote.
    Delete,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    pub path: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RepoChanges {
    pub repo: Repo,
    pub files: Vec<(String, String)>,
}

pub fn uncommitted(local_root: &Path) -> Result<Vec<RepoChanges>> {
    let mut changes = Vec::new();
    for path in list_repos(local_root) {
        let dir = local_root.join(&path);
        let status = git_at(&dir, &["status", "--porcelain=v1", "-z", "--untracked-files=no"])?;
        let files = parse_status(&status, &path);
        if !files.is_empty() {
            changes.push(RepoChanges { repo: Repo { branch: branch(&dir), path }, files });
        }
    }
    Ok(changes)
}

pub fn commit(local_root: &Path, repo: &str, message: &str) -> Result<String> {
    let dir = local_root.join(repo);
    git_at(&dir, &["commit", "-a", "-q", "-m", message])?;
    let hash = git_at(&dir, &["rev-parse", "--short", "HEAD"])?;
    Ok(String::from_utf8_lossy(&hash).trim().to_string())
}

pub fn push_repo(local_root: &Path, repo: &str) -> Result<()> {
    let dir = local_root.join(repo);
    match git_at(&dir, &["push", "-q"]) {
        Err(e) if e.to_string().contains("no upstream branch") => {
            let branch = branch(&dir).context("no branch to push")?;
            git_at(&dir, &["push", "-q", "--set-upstream", "origin", &branch]).map(|_| ())
        }
        result => result.map(|_| ()),
    }
}

pub fn deepest_first(repos: &mut [String]) {
    repos.sort_by_key(|r| std::cmp::Reverse(if r.is_empty() { 0 } else { r.split('/').count() }));
}

fn parse_status(status: &[u8], repo: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut entries = status.split(|&b| b == 0).filter(|e| e.len() > 3);
    while let Some(entry) = entries.next() {
        let entry = String::from_utf8_lossy(entry);
        let (code, path) = entry.split_at(2);
        if code.contains(['R', 'C']) {
            entries.next();
        }
        files.push((code.trim().to_string(), join_repo(repo, &path[1..])));
    }
    files
}

fn git_ignored_among(repo_dir: &Path, paths: &[&str]) -> Vec<String> {
    use std::io::Write;
    if paths.is_empty() {
        return Vec::new();
    }
    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return Vec::new() };
    let list: Vec<u8> = paths.iter().flat_map(|p| p.as_bytes().iter().copied().chain([0])).collect();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&list));
    let Ok(out) = child.wait_with_output() else { return Vec::new() };
    let _ = writer.join();
    out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect()
}

pub(crate) fn git_at(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let out = cmd.output().context("failed to run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

pub fn app_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".agent-diff")
}

pub fn ssh_socket_dir() -> PathBuf {
    app_dir().join("ssh")
}

#[derive(Clone)]
pub enum Scanned {
    Differs(FileEntry),
    Synced(FileEntry),
    Gone,
}

pub struct Engine {
    pub local_root: PathBuf,
    pub remote: Remote,
    pub state_dir: PathBuf,
    mirror: PathBuf,
    base: PathBuf,
    backups: PathBuf,
    known_repos: std::sync::Mutex<Vec<String>>,
    include_untracked: std::sync::atomic::AtomicBool,
    include_gitignored: std::sync::atomic::AtomicBool,
    gitignored: std::sync::Mutex<BTreeSet<String>>,
    gitignored_dirs: std::sync::Mutex<BTreeSet<String>>,
    gitignored_listed: std::sync::Mutex<GitignoredListing>,
    opened_gitignored: std::sync::Mutex<BTreeSet<String>>,
    untracked: std::sync::Mutex<BTreeSet<String>>,
    scanned: std::sync::Mutex<std::collections::HashMap<String, (Stamps, Scanned)>>,
}

type Stamps = [Option<Stamp>; 3];

#[derive(Clone, Default)]
struct GitignoredListing {
    files: BTreeSet<String>,
    folders: BTreeSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    inode: u64,
}

fn stamp(p: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(p).ok()?;
    Some(Stamp { len: m.len(), modified: (m.mtime(), m.mtime_nsec()), changed: (m.ctime(), m.ctime_nsec()), inode: m.ino() })
}

type Content = Option<Vec<u8>>;

impl Engine {
    pub fn new(local_root: PathBuf, remote: Remote) -> Result<Self> {
        let local_root = local_root
            .canonicalize()
            .with_context(|| format!("local workspace {} not found", local_root.display()))?;
        let state_dir = state_dir_of(&local_root, &remote);
        Self::open(local_root, remote, state_dir)
    }

    pub(crate) fn open(local_root: PathBuf, remote: Remote, state_dir: PathBuf) -> Result<Self> {
        let engine = Engine {
            mirror: state_dir.join("mirror"),
            base: state_dir.join("base"),
            backups: state_dir.join("backups"),
            local_root,
            remote,
            state_dir,
            known_repos: Default::default(),
            include_untracked: Default::default(),
            include_gitignored: Default::default(),
            gitignored: Default::default(),
            gitignored_dirs: Default::default(),
            gitignored_listed: Default::default(),
            opened_gitignored: Default::default(),
            untracked: Default::default(),
            scanned: Default::default(),
        };
        engine.tracked()?;
        fs::create_dir_all(&engine.mirror)?;
        fs::write(
            engine.state_dir.join("workspace.txt"),
            format!("local: {}\nremote: {}\n", engine.local_root.display(), engine.remote.display()),
        )?;
        if !engine.base.exists() {
            engine.reset_base()?;
        }
        Ok(engine)
    }

    fn tracked(&self) -> Result<BTreeSet<String>> {
        let repos = list_repos(&self.local_root);
        *self.known_repos.lock().unwrap() = repos.clone();
        if repos.is_empty() {
            bail!("no git repository in {}", self.local_root.display());
        }
        self.tracked_in(&repos)
    }

    fn tracked_in(&self, repos: &[String]) -> Result<BTreeSet<String>> {
        let mut files = BTreeSet::new();
        for repo in repos {
            let out = self.git(repo, &["ls-files", "-z"]).with_context(|| format!("git ls-files failed in {}", self.repo_dir(repo).display()))?;
            for p in out.split(|&b| b == 0).filter(|p| !p.is_empty()) {
                files.insert(join_repo(repo, &String::from_utf8_lossy(p)));
            }
        }
        for repo in repos {
            files.remove(repo);
        }
        Ok(files)
    }

    pub fn set_include_untracked(&self, on: bool) {
        self.include_untracked.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn untracked(&self) -> BTreeSet<String> {
        self.untracked.lock().unwrap().clone()
    }

    pub fn set_include_gitignored(&self, on: bool) {
        self.include_gitignored.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn gitignored(&self) -> BTreeSet<String> {
        self.gitignored.lock().unwrap().clone()
    }

    pub fn gitignored_dirs(&self) -> BTreeSet<String> {
        self.gitignored_dirs.lock().unwrap().clone()
    }

    pub fn set_opened_gitignored(&self, folders: Vec<String>) {
        *self.opened_gitignored.lock().unwrap() = folders.into_iter().collect();
    }

    fn untracked_files(&self) -> BTreeSet<String> {
        self.paths_not_in_git(&["--exclude-standard"]).into_iter().filter(|p| !p.ends_with('/')).collect()
    }

    fn gitignored_files(&self) -> (BTreeSet<String>, BTreeSet<String>) {
        let (folders, files): (BTreeSet<String>, BTreeSet<String>) = self.paths_not_in_git(&["--ignored", "--exclude-standard", "--directory"]).into_iter().partition(|p| p.ends_with('/'));
        let listed = GitignoredListing { files, folders };
        *self.gitignored_listed.lock().unwrap() = listed.clone();
        self.with_opened_folders(listed)
    }

    pub fn open_gitignored(&self, folders: Vec<String>) {
        self.set_opened_gitignored(folders);
        let listed = self.gitignored_listed.lock().unwrap().clone();
        let (files, folders) = self.with_opened_folders(listed);
        *self.gitignored.lock().unwrap() = files;
        *self.gitignored_dirs.lock().unwrap() = folders;
    }

    fn with_opened_folders(&self, GitignoredListing { mut files, mut folders }: GitignoredListing) -> (BTreeSet<String>, BTreeSet<String>) {
        let opened = self.opened_gitignored.lock().unwrap().clone();
        let mut to_open: Vec<String> = folders.iter().filter(|f| opened.contains(*f)).cloned().collect();
        while let Some(folder) = to_open.pop() {
            for (path, is_dir) in gitignored_folder_children(&self.local_root, &folder) {
                if !is_dir {
                    files.insert(path);
                } else if folders.insert(path.clone()) && opened.contains(&path) {
                    to_open.push(path);
                }
            }
        }
        (files, folders)
    }

    fn paths_not_in_git(&self, options: &[&str]) -> BTreeSet<String> {
        let repos = self.repo_list();
        let mut files = BTreeSet::new();
        for repo in &repos {
            for p in self.list_others(repo, options) {
                let path = join_repo(repo, &p);
                if closest_repo(&repos, &path) == Some(repo.as_str()) && self.contained(path.trim_end_matches('/')) {
                    files.insert(path);
                }
            }
        }
        files
    }

    fn list_others(&self, repo: &str, options: &[&str]) -> Vec<String> {
        let args: Vec<&str> = ["ls-files", "-z", "--others"].into_iter().chain(options.iter().copied()).collect();
        let listing = self.git(repo, &args).unwrap_or_default();
        listing.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect()
    }

    pub fn repos(&self) -> Vec<Repo> {
        list_repos(&self.local_root).into_iter().map(|path| Repo { branch: branch(&self.local_root.join(&path)), path }).collect()
    }

    pub fn linked_git_dirs(&self) -> Vec<PathBuf> {
        self.repo_list().iter().filter_map(|path| linked_git_dir(&self.local_root.join(path))).collect()
    }

    fn repo_list(&self) -> Vec<String> {
        let known = self.known_repos.lock().unwrap().clone();
        if known.is_empty() { list_repos(&self.local_root) } else { known }
    }

    pub fn save_remote(&self, path: &str, expected: Option<&[u8]>, text: &[u8]) -> Result<bool> {
        if !self.contained(path) {
            bail!("{path}: refusing, the path goes through a symlink or out of the workspace");
        }
        self.refuse_gitignored(&[path])?;
        self.fetch()?;
        let current = read(&self.mirror.join(path));
        if expected.is_some_and(|expected| current.as_deref() != Some(expected)) {
            return Ok(false);
        }
        self.set_remote(path, &current, &Some(text.to_vec()), &read(&self.base.join(path)))?;
        self.push_reporting(&[path.to_string()], &mut |_| {})?;
        Ok(true)
    }

    pub fn add_to_gitignore(&self, path: &str) -> Result<String> {
        let folder = path.ends_with('/');
        let path = path.trim_end_matches('/');
        if path.is_empty() || !self.contained(path) {
            bail!("{path}: refusing, the path goes through a symlink or out of the workspace");
        }
        let repos = self.repo_list();
        let repo = closest_repo(&repos, path).with_context(|| format!("{path} is in no git repository"))?;
        let dir = closest_gitignore_dir(&self.local_root, repo, path);
        let pattern = format!("/{}{}", gitignore_escaped(in_repo(&dir, path)), if folder { "/" } else { "" });
        let gitignore = join_repo(&dir, ".gitignore");
        append_line_once(&self.local_root.join(&gitignore), &pattern)?;
        let still_tracked = !self.git(repo, &["ls-files", "--", in_repo(repo, path)])?.is_empty();
        let note = if still_tracked { " (still tracked by git: git rm --cached it to stop tracking)" } else { "" };
        Ok(format!("Added {pattern} to {gitignore}{note}"))
    }

    pub fn remove_from_gitignore(&self, path: &str) -> Result<String> {
        let folder = path.ends_with('/');
        let path = path.trim_end_matches('/');
        if path.is_empty() || !contained(&self.local_root, path) {
            bail!("{path}: refusing, the path goes through a symlink or out of the workspace");
        }
        let repos = self.repo_list();
        let repo = closest_repo(&repos, path).with_context(|| format!("{path} is in no git repository"))?;
        let rel = in_repo(repo, path);
        let matched = self.git(repo, &["check-ignore", "-v", "--", rel]).ok().filter(|out| !out.is_empty()).with_context(|| format!("{path} is not gitignored"))?;
        let rule = IgnoreRule::parse(&String::from_utf8_lossy(&matched)).with_context(|| format!("cannot read git's answer for {path}"))?;
        let file = if Path::new(&rule.source).is_absolute() { PathBuf::from(&rule.source) } else { self.repo_dir(repo).join(&rule.source) };
        if !file.starts_with(&self.local_root) {
            bail!("{path} is ignored by {} line {} ({}), outside the workspace: edit it there", rule.source, rule.line, rule.pattern);
        }
        let base = match rule.source.strip_suffix(".gitignore") {
            Some(dir) => join_repo(repo, dir.trim_end_matches('/')),
            None => repo.to_string(),
        };
        let own = gitignore_escaped(in_repo(&base, path));
        let original = fs::read_to_string(&file)?;
        let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
        let index = rule.line.checked_sub(1).filter(|i| *i < lines.len()).with_context(|| format!("{} has no line {}", rule.source, rule.line))?;
        let done = if rule.names_exactly(&own) {
            lines.remove(index);
            format!("Removed {} from {}", rule.pattern, rule.source)
        } else {
            let exception = format!("!/{own}{}", if folder { "/" } else { "" });
            lines.insert(index + 1, exception.clone());
            format!("Added {exception} to {} (after {})", rule.source, rule.pattern)
        };
        fs::write(&file, lines.iter().map(|l| format!("{l}\n")).collect::<String>())?;
        if self.git(repo, &["check-ignore", "-q", "--", rel]).is_ok() {
            fs::write(&file, original)?;
            bail!("{path} stays ignored: a folder above it is ignored ({} line {}: {}). Un-ignore that folder first", rule.source, rule.line, rule.pattern);
        }
        self.gitignored.lock().unwrap().retain(|p| p != path && !p.starts_with(&format!("{path}/")));
        Ok(done)
    }

    pub fn current_version(&self, path: &str, rename: Option<&Rename>) -> u64 {
        let (lp, rp) = rename.map_or((path, path), |r| (r.local_path.as_str(), r.remote_path.as_str()));
        file_version(lp, rp, &self.local_root.join(lp), &self.mirror.join(rp))
    }

    fn git_add(&self, path: &str) -> Result<()> {
        let repos = self.repo_list();
        if let Some(repo) = closest_repo(&repos, path) {
            self.git(repo, &["add", "--", in_repo(repo, path)]).with_context(|| format!("{path} was written but `git add` failed"))?;
        }
        Ok(())
    }

    pub fn gitignored_reveal(&self, path: &str) -> Option<Vec<String>> {
        if !self.is_gitignored(path) {
            return None;
        }
        let folders = path.match_indices('/').map(|(end, _)| format!("{}/", &path[..end]));
        Some(folders.filter(|folder| self.is_gitignored(folder)).collect())
    }

    fn is_gitignored(&self, path: &str) -> bool {
        let repos = self.repo_list();
        closest_repo(&repos, path).is_some_and(|repo| self.git(repo, &["check-ignore", "-q", "--", in_repo(repo, path)]).is_ok())
    }

    fn git_rm_cached(&self, path: &str) -> Result<()> {
        let repos = self.repo_list();
        if let Some(repo) = closest_repo(&repos, path) {
            self.git(repo, &["rm", "--cached", "--quiet", "--ignore-unmatch", "--", in_repo(repo, path)])
                .with_context(|| format!("{path} was deleted but `git rm --cached` failed"))?;
        }
        Ok(())
    }

    fn without_gitignored(&self, paths: BTreeSet<String>) -> BTreeSet<String> {
        let repos = self.repo_list();
        let mut ignored = BTreeSet::new();
        for repo in &repos {
            let in_this_repo: Vec<&str> = paths.iter().filter(|p| closest_repo(&repos, p) == Some(repo.as_str())).map(|p| in_repo(repo, p)).collect();
            ignored.extend(git_ignored_among(&self.repo_dir(repo), &in_this_repo).into_iter().map(|p| join_repo(repo, &p)));
        }
        paths.into_iter().filter(|p| !ignored.contains(p)).collect()
    }

    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.local_root.join(repo)
    }

    fn git(&self, repo: &str, args: &[&str]) -> Result<Vec<u8>> {
        git_at(&self.repo_dir(repo), args)
    }

    /// Make the current local workspace (its tracked files) the baseline: from
    /// now on, every difference on the remote is attributed to the remote.
    pub fn reset_base(&self) -> Result<()> {
        let tmp = self.state_dir.join("base.tmp");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        for p in self.tracked()? {
            if !self.contained(&p) || !fs::symlink_metadata(self.local_root.join(&p)).is_ok_and(|m| m.is_file()) {
                continue;
            }
            let dst = tmp.join(&p);
            fs::create_dir_all(dst.parent().unwrap())?;
            fs::copy(self.local_root.join(&p), dst)?;
        }
        let _ = fs::remove_dir_all(&self.base);
        fs::rename(&tmp, &self.base)?;
        Ok(())
    }

    pub fn fetch(&self) -> Result<bool> {
        self.fetch_reporting(&mut |_| {})
    }

    pub fn fetch_reporting(&self, progress: &mut dyn FnMut(crate::remote::Fetching)) -> Result<bool> {
        self.finish_interrupted_push()?;
        progress(crate::remote::Fetching::AskingRemoteGit);
        let excluded = self.gitignored_rules()?;
        self.remote.fetch(&self.mirror, Some(&excluded), progress)
    }

    fn gitignored_rules(&self) -> Result<crate::remote::Excluded> {
        let remote = self.remote.gitignored_paths(&self.repo_list());
        let listed_by_remote = remote.is_some();
        let mut paths: BTreeSet<String> = remote.unwrap_or_default().into_iter().collect();
        paths.extend(self.paths_not_in_git(&["--ignored", "--exclude-standard", "--directory"]));
        let tracked = self.tracked_in(&self.repo_list())?;
        paths.retain(|p| !holds_tracked(&tracked, p));
        let rules: String = paths.iter().filter(|p| !p.contains('\n')).map(|p| format!("/{}\n", rsync_literal(p))).collect();
        let file = self.state_dir.join("remote-gitignored");
        fs::write(&file, rules)?;
        Ok(crate::remote::Excluded { rules: file, listed_by_remote })
    }

    #[cfg(test)]
    pub fn scan(&self) -> Result<Vec<FileEntry>> {
        Ok(self.scan_all()?.0)
    }

    /// Like `scan`, plus the files that are identical on both sides.
    pub fn scan_path(&self, path: String) -> Result<Scanned> {
        if !self.contained(&path) {
            return Ok(Scanned::Gone);
        }
        if self.gitignored.lock().unwrap().contains(&path) {
            return Ok(self.gitignored_entry(path).map_or(Scanned::Gone, Scanned::Synced));
        }
        let (lp, rp, bp) = (self.local_root.join(&path), self.mirror.join(&path), self.base.join(&path));
        if [&lp, &rp, &bp].iter().any(|p| file_size(p).is_some_and(|size| size > MAX_TEXT as u64)) {
            return scan_big_file(path, &lp, &rp, &bp);
        }
        let l = read(&lp);
        let r = read(&rp);
        let b = read(&bp);
        if l == r {
            if b != l {
                write_opt(&self.base.join(&path), &l)?;
            }
            if l.is_none() {
                return Ok(Scanned::Gone);
            }
            return Ok(Scanned::Synced(FileEntry {
                version: version(&path, &path, &l, &r),
                path,
                rename: None,
                origin: Origin::Synced,
                local_change: None,
                remote_change: None,
                local_exists: true,
                remote_exists: true,
                body: Body::Unchanged,
            }));
        }
        let local_change = change(&b, &l);
        let remote_change = change(&b, &r);
        let origin = match (local_change, remote_change) {
            (None, _) => Origin::Remote,
            (_, None) => Origin::Local,
            _ => Origin::Both,
        };
        let body = match (as_text(&l), as_text(&r)) {
            (Some(_), Some(_)) => Body::Text(Texts::read_when_shown(lp, rp)),
            _ => Body::Binary,
        };
        Ok(Scanned::Differs(FileEntry {
            version: version(&path, &path, &l, &r),
            rename: None,
            local_exists: l.is_some(),
            remote_exists: r.is_some(),
            path,
            origin,
            local_change,
            remote_change,
            body,
        }))
    }

    pub fn has_fetched(&self) -> bool {
        fs::read_dir(&self.mirror).is_ok_and(|mut entries| entries.next().is_some())
    }

    pub fn scan_local(&self) -> Result<Vec<FileEntry>> {
        let paths = self.local_scope()?;
        Ok(paths
            .into_iter()
            .filter(|p| self.contained(p))
            .filter_map(|path| {
                file_size(&self.local_root.join(&path))?;
                self.unchanged_local_entry(path, true)
            })
            .chain(self.gitignored_entries())
            .collect())
    }

    fn local_scope(&self) -> Result<BTreeSet<String>> {
        let mut paths = self.tracked()?;
        let untracked = if self.include_untracked.load(std::sync::atomic::Ordering::Relaxed) { self.untracked_files() } else { BTreeSet::new() };
        *self.untracked.lock().unwrap() = untracked.clone();
        let (gitignored, gitignored_dirs) = if self.include_gitignored.load(std::sync::atomic::Ordering::Relaxed) { self.gitignored_files() } else { Default::default() };
        *self.gitignored.lock().unwrap() = gitignored;
        *self.gitignored_dirs.lock().unwrap() = gitignored_dirs;
        paths.extend(untracked);
        Ok(paths)
    }

    pub fn gitignored_entries(&self) -> Vec<FileEntry> {
        let gitignored = self.gitignored.lock().unwrap().clone();
        gitignored.into_iter().filter_map(|path| self.gitignored_entry(path)).collect()
    }

    fn gitignored_entry(&self, path: String) -> Option<FileEntry> {
        self.unchanged_local_entry(path, false)
    }

    fn unchanged_local_entry(&self, path: String, remote_exists: bool) -> Option<FileEntry> {
        unchanged_local_entry(&self.local_root, path, remote_exists)
    }

    pub fn note_gitignored(&self, path: &str) {
        self.gitignored.lock().unwrap().insert(path.to_string());
    }

    pub fn is_gitignored_path(&self, path: &str) -> bool {
        self.gitignored.lock().unwrap().contains(path) || self.is_gitignored(path)
    }

    fn refuse_gitignored(&self, paths: &[&str]) -> Result<()> {
        match paths.iter().find(|p| self.is_gitignored_path(p)) {
            Some(p) => bail!("{p}: refusing, gitignored files are never synced"),
            None => Ok(()),
        }
    }

    pub fn scan_all(&self) -> Result<(Vec<FileEntry>, Vec<FileEntry>)> {
        self.scan_all_reporting(&mut |_, _| {})
    }

    pub fn scan_all_reporting(&self, progress: &mut dyn FnMut(usize, usize)) -> Result<(Vec<FileEntry>, Vec<FileEntry>)> {
        // Scope: files tracked by git locally, files that only exist on the
        // remote (new files from the agent), and files previously synced
        // (in the baseline). Local untracked files are never synced.
        let mut paths = self.local_scope()?;
        let remote_only = list_files(&self.mirror, true).into_iter().filter(|p| fs::symlink_metadata(self.local_root.join(p)).is_err()).collect();
        paths.extend(self.without_gitignored(remote_only));
        paths.extend(list_files(&self.base, false));
        let gitignored = self.gitignored.lock().unwrap().clone();
        let ignored_folders = self.ignored_folders();
        paths.retain(|p| !gitignored.contains(p) && !in_any_folder(&ignored_folders, p));

        let mut out = Vec::new();
        let mut synced = Vec::new();
        let previous = std::mem::take(&mut *self.scanned.lock().unwrap());
        let mut current = std::collections::HashMap::with_capacity(paths.len());
        let total = paths.len();
        for (done, path) in paths.into_iter().enumerate() {
            progress(done, total);
            let scanned = self.scan_path_reusing(path, &previous, &mut current)?;
            match scanned {
                Scanned::Differs(entry) => out.push(entry),
                Scanned::Synced(entry) => synced.push(entry),
                Scanned::Gone => {}
            }
        }
        *self.scanned.lock().unwrap() = current;
        progress(total, total);
        synced.extend(self.gitignored_entries());
        Ok((self.detect_renames(out)?, synced))
    }

    fn ignored_folders(&self) -> BTreeSet<String> {
        if self.include_gitignored.load(std::sync::atomic::Ordering::Relaxed) {
            return self.gitignored_dirs.lock().unwrap().clone();
        }
        self.paths_not_in_git(&["--ignored", "--exclude-standard", "--directory"]).into_iter().filter(|p| p.ends_with('/')).collect()
    }

    fn stamps(&self, path: &str) -> Stamps {
        [&self.local_root, &self.mirror, &self.base].map(|root| stamp(&root.join(path)))
    }

    fn scan_path_reusing(&self, path: String, previous: &std::collections::HashMap<String, (Stamps, Scanned)>, current: &mut std::collections::HashMap<String, (Stamps, Scanned)>) -> Result<Scanned> {
        let before = self.stamps(&path);
        if let Some((stamps, scanned)) = previous.get(&path)
            && *stamps == before
        {
            current.insert(path, (before, scanned.clone()));
            return Ok(scanned.clone());
        }
        let scanned = self.scan_path(path.clone())?;
        let after = self.stamps(&path);
        if before[..2] == after[..2] {
            current.insert(path, (after, scanned.clone()));
        }
        Ok(scanned)
    }

    /// Merge "deleted at A" + "added at B" pairs on the same side into renames,
    /// when the contents are similar enough.
    fn detect_renames(&self, mut entries: Vec<FileEntry>) -> Result<Vec<FileEntry>> {
        // (index, content) of deletions and additions, per side.
        let mut remote_del = Vec::new();
        let mut remote_add = Vec::new();
        let mut local_del = Vec::new();
        let mut local_add = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            match (e.local_change, e.remote_change) {
                (None, Some(Change::Deleted)) => remote_del.extend(RenameCandidate::of(i, self.local_root.join(&e.path))),
                (None, Some(Change::Added)) => remote_add.extend(RenameCandidate::of(i, self.mirror.join(&e.path))),
                (Some(Change::Deleted), None) => local_del.extend(RenameCandidate::of(i, self.mirror.join(&e.path))),
                (Some(Change::Added), None) => local_add.extend(RenameCandidate::of(i, self.local_root.join(&e.path))),
                _ => {}
            }
        }
        let mut merged = Vec::new();
        let mut used = BTreeSet::new();
        for (dels, adds, remote_side) in [(&remote_del, &remote_add, true), (&local_del, &local_add, false)] {
            for (del, add, similarity) in pair_renames(dels, adds) {
                let (old, new) = (entries[del].path.clone(), entries[add].path.clone());
                let (local_path, remote_path) = if remote_side { (old.clone(), new.clone()) } else { (new.clone(), old.clone()) };
                let (lf, rf) = (self.local_root.join(&local_path), self.mirror.join(&remote_path));
                let body = text_body(&lf, &rf);
                used.extend([del, add]);
                merged.push(FileEntry {
                    version: file_version(&local_path, &remote_path, &lf, &rf),
                    path: new,
                    rename: Some(Rename { local_path, remote_path, from: old, similarity }),
                    origin: if remote_side { Origin::Remote } else { Origin::Local },
                    local_change: if remote_side { None } else { Some(Change::Renamed) },
                    remote_change: if remote_side { Some(Change::Renamed) } else { None },
                    local_exists: true,
                    remote_exists: true,
                    body,
                });
            }
        }
        let mut i = 0;
        entries.retain(|_| {
            i += 1;
            !used.contains(&(i - 1))
        });
        entries.extend(merged);
        Ok(entries)
    }

    /// Apply an action to the local workspace or the mirror. Returns true if
    /// the mirror was modified and the file must be pushed to the remote.
    /// Apply an action to the local workspace or the mirror. Returns the
    /// paths changed in the mirror, to push to the remote.
    ///
    /// For a rename, taking the remote side moves the local file to the new
    /// path (and pushing the local side moves the remote file back), together
    /// with the change taken.
    pub fn apply(&self, path: &str, expected_version: u64, action: Action, rename: Option<&Rename>) -> Result<Vec<String>> {
        let (lp, rp) = match rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (path, path),
        };
        self.refuse_gitignored(&[lp, rp])?;
        for p in [lp, rp] {
            if !self.contained(p) {
                bail!("{p}: refusing, the path goes through a symlink or out of the workspace");
            }
        }
        let l = read(&self.local_root.join(lp));
        let r = read(&self.mirror.join(rp));
        if version(lp, rp, &l, &r) != expected_version {
            bail!("{path} changed while you were looking at it, review it again");
        }
        let hunk = |i: usize, take_remote: bool| -> Result<Content> {
            let (Some(lt), Some(rt)) = (as_text(&l), as_text(&r)) else {
                bail!("{path}: hunk actions need a text file on both sides");
            };
            let h = diff::hunks(lt, rt).get(i).cloned().context("hunk no longer exists")?;
            let text = if take_remote { diff::take_remote(lt, rt, &h) } else { diff::take_local(lt, rt, &h) };
            Ok(Some(text.into_bytes()))
        };
        let base = |p: &str| read(&self.base.join(p));
        match action {
            Action::Delete => {
                let mut pushed = Vec::new();
                for p in BTreeSet::from([lp, rp]) {
                    let (l, r) = (read(&self.local_root.join(p)), read(&self.mirror.join(p)));
                    self.set_local(p, &l, &None, &base(p))?;
                    self.remove_empty_local_parents(p);
                    // Also when the file was already gone from the disk.
                    self.git_rm_cached(p)?;
                    if let Some(r) = &r {
                        if l.as_ref() != Some(r) {
                            self.backup(&format!("remote/{p}"), r)?;
                        }
                        self.set_remote(p, &Some(r.clone()), &None, &base(p))?;
                        pushed.push(p.to_string());
                    }
                }
                Ok(pushed)
            }
            Action::TakeRemoteFile | Action::TakeRemoteHunk(_) => {
                let new = match action {
                    Action::TakeRemoteHunk(i) => hunk(i, true)?,
                    _ => r.clone(),
                };
                if lp == rp {
                    self.set_local(lp, &l, &new, &base(lp))?;
                } else {
                    // Move the local file to the remote path.
                    self.set_local(rp, &read(&self.local_root.join(rp)), &new, &base(rp))?;
                    self.set_local(lp, &l, &None, &base(lp))?;
                }
                Ok(Vec::new())
            }
            Action::TakeLocalFile | Action::TakeLocalHunk(_) => {
                let new = match action {
                    Action::TakeLocalHunk(i) => hunk(i, false)?,
                    _ => l.clone(),
                };
                if lp == rp {
                    self.set_remote(rp, &r, &new, &base(rp))?;
                    // Pushing a local deletion: stage it in git too, so the file is
                    // gone from the workspace and from the index alike.
                    if l.is_none() {
                        self.git_rm_cached(lp)?;
                    }
                    Ok(vec![rp.to_string()])
                } else {
                    // Move the remote file to the local path.
                    self.set_remote(lp, &read(&self.mirror.join(lp)), &new, &base(lp))?;
                    self.set_remote(rp, &r, &None, &base(rp))?;
                    Ok(vec![lp.to_string(), rp.to_string()])
                }
            }
        }
    }

    pub fn sync(&self, path: &str, expected_version: u64, origin: Origin, rename: Option<&Rename>) -> Result<Vec<String>> {
        let local_path = rename.map_or(path, |r| r.local_path.as_str());
        if !self.contained(local_path) {
            bail!("{local_path}: refusing, the path goes through a symlink or out of the workspace");
        }
        self.refuse_gitignored(&[local_path])?;
        if fs::symlink_metadata(self.local_root.join(local_path)).is_ok() && !self.is_gitignored(local_path) {
            self.git_add(local_path)?;
        }
        let action = match origin {
            Origin::Synced => return Ok(Vec::new()),
            Origin::Local => Action::TakeLocalFile,
            Origin::Remote => Action::TakeRemoteFile,
            Origin::Both => bail!("{path} changed on both sides: take one side or pick its hunks"),
        };
        self.apply(path, expected_version, action, rename)
    }

    /// Create the empty file `path` in the local workspace and git add it.
    pub fn create_local(&self, path: &str) -> Result<()> {
        self.create_untracked(path)?;
        self.git_add(path)
    }

    pub fn delete_ignored(&self, path: &str) -> Result<()> {
        let path = path.trim_end_matches('/');
        if path.is_empty() || !contained(&self.local_root, path) {
            bail!("{path}: refusing, the path goes through a symlink or out of the workspace");
        }
        if !self.is_gitignored_path(path) {
            bail!("{path} is not gitignored: delete it with the usual Delete, which also updates the remote and git");
        }
        let full = self.local_root.join(path);
        match fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&full)?,
            Ok(_) => fs::remove_file(&full)?,
            Err(_) => bail!("{path} does not exist"),
        }
        self.gitignored.lock().unwrap().retain(|p| p != path && !p.starts_with(&format!("{path}/")));
        Ok(())
    }

    pub fn make_remote_identical(&self) -> Result<usize> {
        let tracked: Vec<String> = self.tracked()?.into_iter().filter(|p| fs::symlink_metadata(self.local_root.join(p)).is_ok()).collect();
        let mut rules = BTreeSet::new();
        for path in &tracked {
            let mut folder = String::new();
            for part in path.split('/').collect::<Vec<_>>().split_last().map(|(_, dirs)| dirs.to_vec()).unwrap_or_default() {
                folder.push_str(part);
                folder.push('/');
                rules.insert(format!("/{}", rsync_literal(&folder)));
            }
            rules.insert(format!("/{}", rsync_literal(path)));
        }
        let included = self.state_dir.join("identical-include");
        fs::write(&included, rules.iter().filter(|r| !r.contains('\n')).map(|r| format!("{r}\n")).collect::<String>())?;
        let _ = fs::remove_file(self.pending_push_file());
        self.remote.replace_with(&self.local_root, &included)?;
        Ok(tracked.len())
    }

    pub fn import_files(&self, folder: &str, sources: &[PathBuf]) -> Result<usize> {
        let mut copied = 0;
        for source in sources {
            let name = source.file_name().and_then(|n| n.to_str()).with_context(|| format!("{}: no usable name", source.display()))?;
            let rel = format!("{folder}{name}");
            if !self.contained(&rel) {
                bail!("{rel}: refusing, the path goes through a symlink or out of the workspace");
            }
            let target = self.local_root.join(&rel);
            if fs::symlink_metadata(&target).is_ok() {
                bail!("{rel} already exists");
            }
            copied += copy_tree(source, &target)?;
        }
        Ok(copied)
    }

    pub fn remove_empty_folder(&self, folder: &str) -> Result<bool> {
        let folder = folder.trim_end_matches('/');
        if folder.is_empty() || !self.contained(folder) {
            bail!("{folder}: refusing, the path goes through a symlink or out of the workspace");
        }
        let full = self.local_root.join(folder);
        if !full.is_dir() || holds_files(&full) {
            return Ok(false);
        }
        fs::remove_dir_all(&full)?;
        self.remove_empty_local_parents(folder);
        Ok(true)
    }

    pub fn create_folder(&self, path: &str) -> Result<()> {
        let path = path.trim_end_matches('/');
        if !self.contained(path) {
            bail!("{path}: invalid name, or the path goes through a symlink or out of the workspace");
        }
        let full = self.local_root.join(path);
        if fs::symlink_metadata(&full).is_ok() {
            bail!("{path} already exists");
        }
        Ok(fs::create_dir_all(full)?)
    }

    pub fn create_untracked(&self, path: &str) -> Result<()> {
        if !self.contained(path) {
            bail!("{path}: invalid name, or the path goes through a symlink or out of the workspace");
        }
        let full = self.local_root.join(path);
        if fs::symlink_metadata(&full).is_ok() {
            bail!("{path} already exists");
        }
        write_opt(&full, &Some(Vec::new()))
    }

    pub fn rename(&self, path: &str, expected_version: u64, rename: Option<&Rename>, to: &str) -> Result<Vec<String>> {
        let (lp, rp) = rename.map_or((path, path), |r| (r.local_path.as_str(), r.remote_path.as_str()));
        for p in [lp, rp, to] {
            if !self.contained(p) {
                bail!("{p}: invalid name, or the path goes through a symlink or out of the workspace");
            }
        }
        self.refuse_gitignored(&[lp, rp, to])?;
        if fs::symlink_metadata(self.local_root.join(to)).is_ok() || fs::symlink_metadata(self.mirror.join(to)).is_ok() {
            bail!("{to} already exists");
        }
        let l = read(&self.local_root.join(lp));
        let r = read(&self.mirror.join(rp));
        if version(lp, rp, &l, &r) != expected_version {
            bail!("{path} changed while you were looking at it, review it again");
        }
        let base = read(&self.base.join(lp)).or_else(|| read(&self.base.join(rp)));
        if l.is_some() {
            move_file(&self.local_root.join(lp), &self.local_root.join(to))?;
        }
        self.git_rm_cached(lp)?;
        if l.is_some() {
            self.git_add(to)?;
        }
        if r.is_some() {
            self.note_pending_push(&[rp, to])?;
            move_file(&self.mirror.join(rp), &self.mirror.join(to))?;
        }
        write_opt(&self.base.join(to), &base)?;
        for old in BTreeSet::from([lp, rp]) {
            write_opt(&self.base.join(old), &None)?;
        }
        Ok(if r.is_some() { vec![rp.to_string(), to.to_string()] } else { Vec::new() })
    }

    pub fn rename_folder(&self, from: &str, to: &str, files: &[(String, u64, Option<Rename>)]) -> Result<(Vec<String>, Vec<String>)> {
        let (from, to) = (from.trim_end_matches('/'), to.trim_end_matches('/'));
        for p in [from, to] {
            if p.is_empty() || !self.contained(p) {
                bail!("{p}: invalid name, or the path goes through a symlink or out of the workspace");
            }
        }
        if to.starts_with(&format!("{from}/")) {
            bail!("{to} is inside {from}");
        }
        if fs::symlink_metadata(self.local_root.join(to)).is_ok() || fs::symlink_metadata(self.mirror.join(to)).is_ok() {
            bail!("{to} already exists");
        }
        let (mut pushed, mut skipped) = (Vec::new(), Vec::new());
        for (path, version, rename) in files {
            let Some(rest) = path.strip_prefix(&format!("{from}/")) else { continue };
            match self.rename(path, *version, rename.as_ref(), &format!("{to}/{rest}")) {
                Ok(moved) => pushed.extend(moved),
                Err(e) => skipped.push(format!("{e:#}")),
            }
        }
        let left = self.local_root.join(from);
        if left.is_dir() {
            move_tree(&left, &self.local_root.join(to))?;
            self.remove_empty_local_parents(&format!("{from}/"));
        }
        Ok((pushed, skipped))
    }

    #[cfg(test)]
    pub fn push(&self, paths: &[String]) -> Result<()> {
        self.push_reporting(paths, &mut |_| {})
    }

    pub fn push_reporting(&self, paths: &[String], progress: &mut dyn FnMut(f32)) -> Result<()> {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        self.note_pending_push(&refs)?;
        self.remote.push(&self.mirror, paths, progress)?;
        self.forget_pending_push(paths)
    }

    fn finish_interrupted_push(&self) -> Result<()> {
        let pending = self.pending_pushes();
        if pending.is_empty() {
            return Ok(());
        }
        self.remote
            .push_unless_newer(&self.mirror, &pending)
            .with_context(|| format!("could not finish pushing {} file(s) to the remote (an earlier push was interrupted); retrying at the next sync", pending.len()))?;
        self.forget_pending_push(&pending)
    }

    fn pending_push_file(&self) -> PathBuf {
        self.state_dir.join("pending-push")
    }

    fn pending_pushes(&self) -> Vec<String> {
        let bytes = fs::read(self.pending_push_file()).unwrap_or_default();
        let paths: BTreeSet<String> = bytes.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
        paths.into_iter().filter(|p| self.contained(p)).collect()
    }

    fn note_pending_push(&self, paths: &[&str]) -> Result<()> {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(self.pending_push_file())?;
        for p in paths {
            file.write_all(p.as_bytes())?;
            file.write_all(b"\0")?;
        }
        file.sync_all()?;
        Ok(())
    }

    fn forget_pending_push(&self, pushed: &[String]) -> Result<()> {
        let left: Vec<String> = self.pending_pushes().into_iter().filter(|p| !pushed.contains(p)).collect();
        if left.is_empty() {
            let _ = fs::remove_file(self.pending_push_file());
            return Ok(());
        }
        let text: Vec<u8> = left.iter().flat_map(|p| p.as_bytes().iter().copied().chain([0])).collect();
        fs::write(self.pending_push_file(), text)?;
        Ok(())
    }

    /// True if `rel` stays inside every tree we touch: plain relative
    /// components only, and no symlink anywhere along the path, so no read or
    /// write can escape the workspace.
    fn contained(&self, rel: &str) -> bool {
        [&self.local_root, &self.mirror, &self.base].iter().all(|root| contained(root, rel))
    }

    fn remove_empty_local_parents(&self, path: &str) {
        let mut dir = path;
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if fs::remove_dir(self.local_root.join(parent)).is_err() {
                break;
            }
            dir = parent;
        }
    }

    fn set_local(&self, path: &str, old: &Content, new: &Content, base: &Content) -> Result<()> {
        if let Some(old) = old {
            self.backup(path, old)?;
        }
        write_opt(&self.local_root.join(path), new)?;
        // A file pulled into existence locally joins the git index, so it is
        // tracked (and stays in scope) like the rest of the workspace.
        // Likewise a pulled deletion is staged.
        match (old, new) {
            (None, Some(_)) => {
                self.git_add(path)?;
            }
            (Some(_), None) => self.git_rm_cached(path)?,
            _ => {}
        }
        // If this side was untouched, the change we just took becomes the
        // baseline so the file keeps its origin and only remaining hunks show.
        if base == old {
            write_opt(&self.base.join(path), new)?;
        }
        Ok(())
    }

    /// Copy `content` to `backups/<timestamp>/<rel>`.
    fn backup(&self, rel: &str, content: &[u8]) -> Result<()> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let dst = self.backups.join(ts.to_string()).join(rel);
        fs::create_dir_all(dst.parent().unwrap())?;
        fs::write(dst, content)?;
        Ok(())
    }

    fn set_remote(&self, path: &str, old: &Content, new: &Content, base: &Content) -> Result<()> {
        self.note_pending_push(&[path])?;
        write_opt(&self.mirror.join(path), new)?;
        if base == old {
            write_opt(&self.base.join(path), new)?;
        }
        Ok(())
    }
}

fn list_repos(root: &Path) -> Vec<String> {
    let mut repos = walk_repos(root);
    let mut next = 0;
    while next < repos.len() {
        let repo = repos[next].clone();
        for ignored in ignored_repos(&root.join(&repo)) {
            let path = join_repo(&repo, &ignored);
            if !repos.contains(&path) {
                repos.extend(walk_repos(&root.join(&path)).into_iter().map(|inner| join_repo(&path, &inner)));
            }
        }
        next += 1;
    }
    repos.sort();
    repos.dedup();
    repos
}

fn ignored_repos(repo_dir: &Path) -> Vec<String> {
    let listing = git_at(repo_dir, &["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]).unwrap_or_default();
    listing
        .split(|&b| b == 0)
        .filter_map(|entry| String::from_utf8_lossy(entry).strip_suffix('/').map(str::to_string))
        .filter(|dir| repo_dir.join(dir).join(".git").exists())
        .collect()
}

fn walk_repos(root: &Path) -> Vec<String> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .parents(false)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut repos: Vec<String> = walker
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_dir()) && e.path().join(".git").exists())
        .filter_map(|e| e.path().strip_prefix(root).ok().map(slash_path))
        .collect();
    repos.sort();
    repos
}

fn linked_git_dir(repo_dir: &Path) -> Option<PathBuf> {
    let dot_git = repo_dir.join(".git");
    if !dot_git.is_file() {
        return None;
    }
    let link = fs::read_to_string(&dot_git).ok()?;
    let git_dir = repo_dir.join(link.trim().strip_prefix("gitdir:")?.trim());
    Some(git_dir.canonicalize().unwrap_or(git_dir))
}

fn branch(repo_dir: &Path) -> Option<String> {
    let git_dir = linked_git_dir(repo_dir).unwrap_or_else(|| repo_dir.join(".git"));
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(name) => name.to_string(),
        None => head.chars().take(7).collect(),
    })
}

fn slash_path(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn closest_repo<'a>(repos: &'a [String], path: &str) -> Option<&'a str> {
    repos
        .iter()
        .filter(|r| r.is_empty() || path.strip_prefix(r.as_str()).is_some_and(|rest| rest.starts_with('/')))
        .max_by_key(|r| r.len())
        .map(String::as_str)
}

fn in_repo<'a>(repo: &str, path: &'a str) -> &'a str {
    if repo.is_empty() { path } else { &path[repo.len() + 1..] }
}

struct IgnoreRule {
    source: String,
    line: usize,
    pattern: String,
}

impl IgnoreRule {
    fn parse(answer: &str) -> Option<Self> {
        let (rule, _) = answer.lines().next()?.split_once('\t')?;
        let mut parts = rule.splitn(3, ':');
        let (source, line, pattern) = (parts.next()?, parts.next()?.parse().ok()?, parts.next()?);
        Some(IgnoreRule { source: source.to_string(), line, pattern: pattern.to_string() })
    }

    fn names_exactly(&self, own: &str) -> bool {
        let core = self.pattern.trim_end_matches('/');
        let anchored = core.starts_with('/') || core.contains('/');
        anchored && core.trim_start_matches('/') == own
    }
}

fn closest_gitignore_dir(local_root: &Path, repo: &str, path: &str) -> String {
    let mut dir = path;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        dir = parent;
        if dir.len() < repo.len() {
            break;
        }
        if local_root.join(dir).join(".gitignore").is_file() {
            return dir.to_string();
        }
    }
    repo.to_string()
}

fn gitignore_escaped(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.ends_with(' ') {
        out.insert(out.len() - 1, '\\');
    }
    out
}

fn append_line_once(file: &Path, line: &str) -> Result<()> {
    let text = fs::read_to_string(file).unwrap_or_default();
    if text.lines().any(|l| l.trim_end() == line) {
        return Ok(());
    }
    let separator = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    fs::write(file, format!("{text}{separator}{line}\n"))?;
    Ok(())
}

fn join_repo(repo: &str, path: &str) -> String {
    match (repo.is_empty(), path.is_empty()) {
        (true, _) => path.to_string(),
        (_, true) => repo.to_string(),
        _ => format!("{repo}/{path}"),
    }
}

/// Relative paths ('/'-separated) of all regular files under `root`.
fn rsync_literal(path: &str) -> String {
    path.chars().flat_map(|c| if matches!(c, '*' | '?' | '[' | '\\') { vec!['\\', c] } else { vec![c] }).collect()
}

fn list_files(root: &Path, respect_ignore: bool) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if !root.exists() {
        return set;
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(respect_ignore)
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let rel: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
            set.insert(rel.join("/"));
        }
    }
    set
}

fn move_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && target.is_dir() {
            move_tree(&entry.path(), &target)?;
        } else if fs::symlink_metadata(&target).is_err() {
            fs::rename(entry.path(), &target)?;
        }
    }
    let _ = fs::remove_dir(from);
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    let meta = fs::symlink_metadata(from).with_context(|| format!("{}: cannot read it", from.display()))?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        let mut copied = 0;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copied += copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(copied);
    }
    if !meta.is_file() {
        return Ok(0);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    Ok(1)
}

fn in_any_folder(folders: &BTreeSet<String>, path: &str) -> bool {
    path.match_indices('/').any(|(i, _)| folders.contains(&path[..=i]))
}

fn holds_files(dir: &Path) -> bool {
    let Ok(read) = fs::read_dir(dir) else { return true };
    read.flatten().any(|e| !e.file_type().is_ok_and(|t| t.is_dir()) || holds_files(&e.path()))
}

fn holds_tracked(tracked: &BTreeSet<String>, path: &str) -> bool {
    if path.ends_with('/') {
        tracked.range(path.to_string()..).next().is_some_and(|t| t.starts_with(path))
    } else {
        tracked.contains(path)
    }
}

pub fn gitignored_folder_children(local_root: &Path, folder: &str) -> Vec<(String, bool)> {
    let Ok(read) = fs::read_dir(local_root.join(folder)) else { return Vec::new() };
    read.flatten()
        .filter(|e| e.file_name() != ".git" && !e.path().join(".git").exists())
        .filter_map(|e| {
            let kind = e.file_type().ok()?;
            let path = format!("{folder}{}", e.file_name().to_str()?);
            let is_dir = kind.is_dir();
            (is_dir || kind.is_file()).then_some(())?;
            contained(local_root, &path).then(|| (if is_dir { format!("{path}/") } else { path }, is_dir))
        })
        .collect()
}

pub fn unchanged_local_entry(local_root: &Path, path: String, remote_exists: bool) -> Option<FileEntry> {
    let stamp = stamp(&local_root.join(&path))?;
    let version = fnv(&[path.as_bytes(), &stamp.len.to_le_bytes(), &stamp.modified.0.to_le_bytes(), &stamp.modified.1.to_le_bytes()]);
    Some(FileEntry { path, rename: None, origin: Origin::Synced, local_change: None, remote_change: None, local_exists: true, remote_exists, body: Body::Unchanged, version })
}

pub(crate) fn contained(root: &Path, rel: &str) -> bool {
    let rel = Path::new(rel);
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(_) => break, // the rest does not exist yet, it will be created as real dirs
        }
    }
    true
}

fn read(p: &Path) -> Content {
    fs::read(p).ok()
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to).with_context(|| format!("moving {} to {}", from.display(), to.display()))
}

fn write_opt(p: &Path, content: &Content) -> Result<()> {
    match content {
        Some(c) => {
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(p, c).with_context(|| format!("writing {}", p.display()))
        }
        None => match fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("deleting {}", p.display()))
            }
            _ => Ok(()),
        },
    }
}

fn change(base: &Content, now: &Content) -> Option<Change> {
    match (base, now) {
        (a, b) if a == b => None,
        (None, Some(_)) => Some(Change::Added),
        (Some(_), None) => Some(Change::Deleted),
        _ => Some(Change::Modified),
    }
}

/// Missing files count as empty text so additions/deletions get a diff too.
fn as_text(c: &Content) -> Option<&str> {
    match c {
        None => Some(""),
        Some(b) if b.len() <= MAX_TEXT && !b.contains(&0) => std::str::from_utf8(b).ok(),
        _ => None,
    }
}

fn version(local_path: &str, remote_path: &str, l: &Content, r: &Content) -> u64 {
    let mut hash = Fnv::new();
    hash.update(local_path.as_bytes());
    hash.update(b"\0");
    hash.update(remote_path.as_bytes());
    hash.update(b"\0");
    for (content, last) in [(l, false), (r, true)] {
        hash.update(if content.is_some() { b"1" } else { b"0" });
        hash.update(content.as_deref().unwrap_or_default());
        if !last {
            hash.update(b"\0");
        }
    }
    hash.finish()
}

fn file_version(local_path: &str, remote_path: &str, local: &Path, remote: &Path) -> u64 {
    let mut hash = Fnv::new();
    hash.update(local_path.as_bytes());
    hash.update(b"\0");
    hash.update(remote_path.as_bytes());
    hash.update(b"\0");
    for (file, last) in [(local, false), (remote, true)] {
        let exists = file_size(file).is_some();
        hash.update(if exists { b"1" } else { b"0" });
        if exists {
            for_each_chunk(file, &mut |chunk| hash.update(chunk));
        }
        if !last {
            hash.update(b"\0");
        }
    }
    hash.finish()
}

fn file_size(p: &Path) -> Option<u64> {
    fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

fn for_each_chunk(p: &Path, on_chunk: &mut dyn FnMut(&[u8])) {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(p) else { return };
    let mut buf = vec![0u8; STREAM_CHUNK];
    while let Ok(n @ 1..) = file.read(&mut buf) {
        on_chunk(&buf[..n]);
    }
}

pub fn same_content(a: &Path, b: &Path) -> bool {
    match (file_size(a), file_size(b)) {
        (None, None) => return true,
        (Some(x), Some(y)) if x == y => {}
        _ => return false,
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; STREAM_CHUNK], vec![0u8; STREAM_CHUNK]);
    loop {
        let (Ok(na), Ok(nb)) = (fill(&mut fa, &mut ba), fill(&mut fb, &mut bb)) else { return false };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na < STREAM_CHUNK {
            return true;
        }
    }
}

fn fill(reader: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn copy_or_remove(from: &Path, to: &Path) -> Result<()> {
    if file_size(from).is_none() {
        return write_opt(to, &None);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn file_change(base: &Path, now: &Path) -> Option<Change> {
    match (file_size(base).is_some(), file_size(now).is_some()) {
        _ if same_content(base, now) => None,
        (false, true) => Some(Change::Added),
        (true, false) => Some(Change::Deleted),
        _ => Some(Change::Modified),
    }
}

fn scan_big_file(path: String, local: &Path, remote: &Path, base: &Path) -> Result<Scanned> {
    let (local_exists, remote_exists) = (file_size(local).is_some(), file_size(remote).is_some());
    let version = file_version(&path, &path, local, remote);
    if same_content(local, remote) {
        if !same_content(base, local) {
            copy_or_remove(local, base)?;
        }
        if !local_exists {
            return Ok(Scanned::Gone);
        }
        return Ok(Scanned::Synced(FileEntry { version, path, rename: None, origin: Origin::Synced, local_change: None, remote_change: None, local_exists, remote_exists, body: Body::Unchanged }));
    }
    let (local_change, remote_change) = (file_change(base, local), file_change(base, remote));
    let origin = match (local_change, remote_change) {
        (None, _) => Origin::Remote,
        (_, None) => Origin::Local,
        _ => Origin::Both,
    };
    Ok(Scanned::Differs(FileEntry { version, path, rename: None, origin, local_change, remote_change, local_exists, remote_exists, body: Body::Binary }))
}

fn text_body(local: &Path, remote: &Path) -> Body {
    let small = |p: &Path| file_size(p).is_none_or(|size| size <= MAX_TEXT as u64);
    if !small(local) || !small(remote) {
        return Body::Binary;
    }
    let (l, r) = (read(local), read(remote));
    match (as_text(&l), as_text(&r)) {
        (Some(_), Some(_)) => Body::Text(Texts::read_when_shown(local.to_path_buf(), remote.to_path_buf())),
        _ => Body::Binary,
    }
}

struct RenameCandidate {
    index: usize,
    path: PathBuf,
    len: u64,
    hash: u64,
}

impl RenameCandidate {
    fn of(index: usize, path: PathBuf) -> Option<Self> {
        let len = file_size(&path)?;
        let mut hash = Fnv::new();
        for_each_chunk(&path, &mut |chunk| hash.update(chunk));
        Some(RenameCandidate { index, path, len, hash: hash.finish() })
    }

    fn same_as(&self, other: &Self) -> bool {
        self.len == other.len && self.hash == other.hash
    }

    fn fuzzy_text(&self) -> Option<String> {
        if self.len > MAX_FUZZY_RENAME {
            return None;
        }
        String::from_utf8(fs::read(&self.path).ok()?).ok()
    }
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf29ce484222325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Pair deletions with additions whose content is the same or similar enough:
/// returns (deletion index, addition index, similarity %). Best matches first;
/// each file is used at most once.
fn pair_renames(dels: &[RenameCandidate], adds: &[RenameCandidate]) -> Vec<(usize, usize, u8)> {
    // Fuzzy matching is quadratic: past this, only identical contents pair up.
    let fuzzy = dels.len() * adds.len() <= 2500;
    let dels_held = dels.len() <= adds.len();
    let (held, streamed) = if dels_held { (dels, adds) } else { (adds, dels) };
    let held_texts: Vec<Option<String>> = held.iter().map(|c| if fuzzy { c.fuzzy_text() } else { None }).collect();
    let mut candidates = Vec::new();
    for s in streamed {
        let mut s_text = None;
        for (h, h_text) in held.iter().zip(&held_texts) {
            let score = if s.same_as(h) {
                1.0
            } else if !fuzzy || s.len.min(h.len) * 3 < s.len.max(h.len) {
                continue;
            } else {
                let s_text = s_text.get_or_insert_with(|| s.fuzzy_text());
                match (s_text.as_deref(), h_text.as_deref()) {
                    (Some(x), Some(y)) => similar::TextDiff::from_lines(x, y).ratio(),
                    _ => continue,
                }
            };
            if score >= RENAME_THRESHOLD {
                let (d, a) = if dels_held { (h.index, s.index) } else { (s.index, h.index) };
                candidates.push((score, d, a));
            }
        }
    }
    candidates.sort_by(|x, y| y.0.total_cmp(&x.0));
    let (mut used_d, mut used_a) = (BTreeSet::new(), BTreeSet::new());
    candidates
        .into_iter()
        .filter(|&(_, d, a)| used_d.insert(d) && used_a.insert(a))
        .map(|(score, d, a)| (d, a, (score * 100.0).round() as u8))
        .collect()
}

pub fn workspace_id(local_root: &Path, remote: &Remote) -> String {
    let key = format!("{}\0{}", local_root.display(), remote.display());
    format!("{:016x}", fnv(&[key.as_bytes()]))
}

pub fn state_dir_of(local_root: &Path, remote: &Remote) -> PathBuf {
    app_dir().join("workspaces").join(workspace_id(local_root, remote))
}

pub fn workspace_state_dirs(local: &str, remote: &Remote) -> Vec<PathBuf> {
    let Ok(local_root) = Path::new(local).canonicalize() else { return Vec::new() };
    vec![state_dir_of(&local_root, remote)]
}

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash = Fnv::new();
    for part in parts {
        hash.update(part);
    }
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Engine {
        /// `apply` for non-renamed files, as the old tests use it.
        fn apply_simple(&self, path: &str, version: u64, action: Action) -> Result<bool> {
            Ok(!self.apply(path, version, action, None)?.is_empty())
        }
    }

    fn setup(name: &str) -> (PathBuf, PathBuf, Engine) {
        let root = std::env::temp_dir().join(format!("agent-diff-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (local, remote) = (root.join("local"), root.join("remote"));
        for d in [&local, &remote] {
            fs::create_dir_all(d.join("src")).unwrap();
            fs::write(d.join("src/a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
            fs::write(d.join(".gitignore"), "target/\n").unwrap();
        }
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&local).args(args).output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        let engine = Engine::open(local.clone(), Remote::Local(remote.clone()), root.join("state")).unwrap();
        (local, remote, engine)
    }

    #[test]
    fn remote_only_files_ignored_outside_gitignore_files_are_not_compared() {
        let (local, _remote, e) = setup("info-exclude-scan");
        fs::write(local.join(".git/info/exclude"), "venv/\n").unwrap();
        fs::create_dir_all(e.mirror.join("venv/lib")).unwrap();
        fs::write(e.mirror.join("venv/lib/remote_only.py"), "x").unwrap();
        assert!(e.scan().unwrap().iter().all(|f| !f.path.starts_with("venv/")));
    }

    #[test]
    fn repositories_inside_folders_ignored_by_info_exclude_are_not_indexed() {
        let (local, _remote, _engine) = setup("info-exclude-repos");
        fs::write(local.join(".git/info/exclude"), "venv/\n").unwrap();
        let checkout = local.join("venv/src/pkg");
        fs::create_dir_all(&checkout).unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&checkout).args(["init", "-q"]).status().unwrap().success());
        assert_eq!(list_repos(&local), [""]);
    }

    #[test]
    fn files_in_gitignored_folders_are_not_compared_even_when_synced_before() {
        let (local, _remote, e) = setup("ignored-baseline");
        e.fetch().unwrap();
        fs::create_dir_all(local.join("target/deep")).unwrap();
        fs::write(local.join("target/deep/data.bin"), "x").unwrap();
        fs::create_dir_all(e.base.join("target/deep")).unwrap();
        fs::write(e.base.join("target/deep/data.bin"), "x").unwrap();
        assert!(e.scan().unwrap().iter().all(|f| !f.path.starts_with("target/")), "{:?}", e.scan().unwrap());
        e.set_include_gitignored(true);
        assert!(e.scan().unwrap().iter().all(|f| !f.path.starts_with("target/")), "{:?}", e.scan().unwrap());
    }

    #[test]
    fn unignores_by_removing_its_own_rule_or_adding_an_exception() {
        let (local, _remote, e) = setup("unignore");
        fs::write(local.join(".gitignore"), "target/\n*.log\n/telemetry\n").unwrap();
        fs::create_dir_all(local.join("telemetry")).unwrap();
        fs::write(local.join("telemetry/a.json"), "x").unwrap();
        fs::write(local.join("debug.log"), "x").unwrap();
        fs::write(local.join("keep.log"), "x").unwrap();
        e.remove_from_gitignore("telemetry/").unwrap();
        e.remove_from_gitignore("debug.log").unwrap();
        let text = fs::read_to_string(local.join(".gitignore")).unwrap();
        assert_eq!(text, "target/\n*.log\n!/debug.log\n");
        assert!(!e.is_gitignored_path("telemetry") && !e.is_gitignored_path("debug.log") && e.is_gitignored_path("keep.log"));
        fs::create_dir_all(local.join("target")).unwrap();
        fs::write(local.join("target/x.bin"), "x").unwrap();
        assert!(e.remove_from_gitignore("target/x.bin").is_err());
        assert_eq!(fs::read_to_string(local.join(".gitignore")).unwrap(), text);
    }

    #[test]
    fn deletes_gitignored_files_and_folders_locally_only() {
        let (local, _remote, e) = setup("delete-ignored");
        fs::create_dir_all(local.join("target/deep")).unwrap();
        fs::write(local.join("target/deep/a.bin"), "x").unwrap();
        fs::write(local.join("notes.log"), "x").unwrap();
        fs::write(local.join(".gitignore"), "target/\n*.log\n").unwrap();
        e.delete_ignored("notes.log").unwrap();
        e.delete_ignored("target/").unwrap();
        assert!(!local.join("notes.log").exists() && !local.join("target").exists());
        assert!(e.delete_ignored("src/a.txt").is_err());
        assert!(local.join("src/a.txt").exists());
    }

    #[test]
    fn making_the_remote_identical_keeps_only_git_files_and_its_git_folder() {
        let (local, remote, e) = setup("identical");
        fs::create_dir_all(remote.join("uploads")).unwrap();
        fs::write(remote.join("uploads/a.png"), "x").unwrap();
        fs::write(remote.join("src/extra.txt"), "x").unwrap();
        fs::create_dir_all(remote.join(".git")).unwrap();
        fs::write(remote.join(".git/HEAD"), "ref").unwrap();
        fs::write(local.join("untracked.txt"), "x").unwrap();
        assert_eq!(e.make_remote_identical().unwrap(), 2);
        assert!(remote.join("src/a.txt").exists() && remote.join(".gitignore").exists());
        assert!(!remote.join("uploads").exists() && !remote.join("src/extra.txt").exists() && !remote.join("untracked.txt").exists());
        assert!(remote.join(".git/HEAD").exists());
    }

    #[test]
    fn renames_a_folder_with_its_files_and_what_git_ignores() {
        let (local, remote, e) = setup("rename-folder");
        fs::write(local.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(local.join("src/debug.log"), "x").unwrap();
        e.fetch().unwrap();
        let a = e.scan_path("src/a.txt".into()).unwrap();
        let crate::engine::Scanned::Synced(a) = a else { panic!("src/a.txt should be in sync") };
        let (pushed, skipped) = e.rename_folder("src/", "lib", &[(a.path.clone(), a.version, None)]).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        e.push(&pushed).unwrap();
        assert!(local.join("lib/a.txt").exists() && !local.join("src").exists());
        assert!(local.join("lib/debug.log").exists());
        assert!(remote.join("lib/a.txt").exists() && !remote.join("src/a.txt").exists());
        assert!(e.rename_folder("lib/", "lib/inner", &[]).is_err());
    }

    #[test]
    fn imports_dropped_files_and_folders() {
        let (local, _remote, e) = setup("import-files");
        let outside = local.parent().unwrap().join("outside");
        fs::create_dir_all(outside.join("assets/icons")).unwrap();
        fs::write(outside.join("notes.txt"), "n").unwrap();
        fs::write(outside.join("assets/icons/a.svg"), "<svg/>").unwrap();
        assert_eq!(e.import_files("src/", &[outside.join("notes.txt"), outside.join("assets")]).unwrap(), 2);
        assert!(local.join("src/notes.txt").exists());
        assert!(local.join("src/assets/icons/a.svg").exists());
        assert!(e.import_files("src/", &[outside.join("notes.txt")]).is_err());
    }

    #[test]
    fn removes_a_folder_only_when_it_holds_no_file() {
        let (local, _remote, e) = setup("remove-empty-folder");
        fs::create_dir_all(local.join("empty/inner")).unwrap();
        assert!(e.remove_empty_folder("empty/").unwrap());
        assert!(!local.join("empty").exists());
        fs::create_dir_all(local.join("kept/inner")).unwrap();
        fs::write(local.join("kept/inner/x.log"), "x").unwrap();
        assert!(!e.remove_empty_folder("kept/").unwrap());
        assert!(local.join("kept/inner/x.log").exists());
    }

    #[test]
    fn fetch_skips_folders_ignored_outside_gitignore_files() {
        let (local, remote, e) = setup("info-exclude");
        fs::write(local.join(".git/info/exclude"), "venv/\n").unwrap();
        for d in [&local, &remote] {
            fs::create_dir_all(d.join("venv/lib")).unwrap();
            fs::write(d.join("venv/lib/both.py"), "x").unwrap();
        }
        fs::write(remote.join("venv/lib/remote_only.py"), "x").unwrap();
        fs::create_dir_all(e.mirror.join("venv/lib")).unwrap();
        fs::write(e.mirror.join("venv/lib/remote_only.py"), "x").unwrap();
        e.fetch().unwrap();
        assert!(!e.mirror.join("venv").exists());
        assert!(e.scan().unwrap().is_empty(), "{:?}", e.scan().unwrap());
    }

    #[test]
    fn end_to_end() {
        let (local, remote, e) = setup("e2e");
        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());

        // Claude edits two distant lines, adds a file and an ignored build artifact.
        fs::write(remote.join("src/a.txt"), "ONE\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n").unwrap();
        fs::write(remote.join("new.txt"), "hello\n").unwrap();
        fs::create_dir_all(remote.join("target")).unwrap();
        fs::write(remote.join("target/junk"), "x").unwrap();
        // You edit a different file locally.
        fs::write(local.join(".gitignore"), "target/\n*.log\n").unwrap();

        assert!(e.fetch().unwrap());
        let s = e.scan().unwrap();
        let get = |s: &[FileEntry], p: &str| s.iter().find(|x| x.path == p).cloned().unwrap();
        assert_eq!(s.len(), 3, "{s:?}");
        let a = get(&s, "src/a.txt");
        assert_eq!(a.origin, Origin::Remote);
        assert!(matches!(&a.body, Body::Text(t) if t.diff().hunks.len() == 2));
        assert_eq!(get(&s, "new.txt").remote_change, Some(Change::Added));
        assert_eq!(get(&s, ".gitignore").origin, Origin::Local);

        // Pull only the first hunk: the file stays "remote changed" with one hunk left.
        e.apply_simple("src/a.txt", a.version, Action::TakeRemoteHunk(0)).unwrap();
        let a = get(&e.scan().unwrap(), "src/a.txt");
        assert_eq!(a.origin, Origin::Remote);
        assert!(matches!(&a.body, Body::Text(t) if t.diff().hunks.len() == 1));
        assert!(fs::read_to_string(local.join("src/a.txt")).unwrap().starts_with("ONE\n"));

        // Reject the second hunk by pushing your version back to the remote.
        assert!(e.apply_simple("src/a.txt", a.version, Action::TakeLocalHunk(0)).unwrap());
        e.push(&["src/a.txt".into()]).unwrap();
        assert!(fs::read_to_string(remote.join("src/a.txt")).unwrap().ends_with("\n10\n"));

        // Push the local .gitignore, pull the new file.
        let s = e.scan().unwrap();
        assert!(e.apply_simple(".gitignore", get(&s, ".gitignore").version, Action::TakeLocalFile).unwrap());
        e.apply_simple("new.txt", get(&s, "new.txt").version, Action::TakeRemoteFile).unwrap();
        e.push(&[".gitignore".into()]).unwrap();
        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());

        // Stale versions are rejected.
        fs::write(remote.join("new.txt"), "changed\n").unwrap();
        e.fetch().unwrap();
        let v = get(&e.scan().unwrap(), "new.txt").version;
        fs::write(remote.join("new.txt"), "changed again\n").unwrap();
        e.fetch().unwrap();
        assert!(e.apply_simple("new.txt", v, Action::TakeRemoteFile).is_err());

        // Deleting on the remote and pulling deletes locally (with a backup).
        fs::remove_file(remote.join("new.txt")).unwrap();
        e.fetch().unwrap();
        let n = get(&e.scan().unwrap(), "new.txt");
        assert_eq!(n.remote_change, Some(Change::Deleted));
        e.apply_simple("new.txt", n.version, Action::TakeRemoteFile).unwrap();
        assert!(!local.join("new.txt").exists());
        assert!(!e.tracked().unwrap().contains("new.txt"));
        assert!(e.scan().unwrap().is_empty());
    }

    #[test]
    fn delete_everywhere() {
        let (local, remote, e) = setup("delete");
        fs::write(remote.join("new.txt"), "from claude\n").unwrap();
        e.fetch().unwrap();
        let (changed, synced) = e.scan_all().unwrap();
        let a = synced.iter().find(|f| f.path == "src/a.txt").unwrap();
        let n = changed.iter().find(|f| f.path == "new.txt").unwrap();

        // A stale version is refused, as for the other actions.
        assert!(e.apply_simple("src/a.txt", a.version + 1, Action::Delete).is_err());

        for f in [a, n] {
            let pushed = e.apply(&f.path, f.version, Action::Delete, None).unwrap();
            assert_eq!(pushed, vec![f.path.clone()]);
            e.push(&pushed).unwrap();
        }
        for p in ["src/a.txt", "new.txt"] {
            assert!(!local.join(p).exists() && !remote.join(p).exists(), "{p}");
        }
        assert!(!e.tracked().unwrap().contains("src/a.txt"));
        // The remote-only file is backed up, the local one too.
        let backups: Vec<_> = list_files(&e.backups, false).into_iter().collect();
        assert!(backups.iter().any(|b| b.ends_with("/remote/new.txt")), "{backups:?}");
        assert!(backups.iter().any(|b| b.ends_with("/src/a.txt") && !b.contains("remote/")), "{backups:?}");
        e.fetch().unwrap();
        let (changed, synced) = e.scan_all().unwrap();
        assert!(changed.is_empty(), "{changed:?}");
        assert!(synced.iter().all(|f| f.path != "src/a.txt"));
    }

    #[test]
    fn create_local_file() {
        let (local, _remote, e) = setup("create");
        e.create_local("src/new/b.rs").unwrap();
        assert_eq!(fs::read(local.join("src/new/b.rs")).unwrap(), b"");
        assert!(e.tracked().unwrap().contains("src/new/b.rs"));
        e.fetch().unwrap();
        let f = e.scan().unwrap().into_iter().find(|f| f.path == "src/new/b.rs").unwrap();
        assert_eq!((f.origin, f.local_change), (Origin::Local, Some(Change::Added)));
        assert!(e.create_local("src/a.txt").is_err());
        assert!(e.create_local("../out.txt").is_err());
        assert!(e.create_local("src").is_err());
    }

    #[test]
    fn several_repos_in_one_workspace() {
        let root = std::env::temp_dir().join(format!("agent-diff-test-repos-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (local, remote) = (root.join("local"), root.join("remote"));
        let git = |dir: &Path, args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success());
        for repo in ["front", "back", "back/plugins/x"] {
            for side in [&local, &remote] {
                fs::create_dir_all(side.join(repo)).unwrap();
                fs::write(side.join(repo).join("README"), repo).unwrap();
            }
            git(&local.join(repo), &["init", "-q"]);
            git(&local.join(repo), &["add", "README"]);
        }
        fs::create_dir_all(root.join("empty")).unwrap();
        let err = Engine::open(root.join("empty"), Remote::Local(remote.clone()), root.join("s0")).err().unwrap();
        assert!(err.to_string().contains("no git repository"), "{err}");
        let e = Engine::open(local.clone(), Remote::Local(remote.clone()), root.join("state")).unwrap();
        assert_eq!(list_repos(&local), ["back", "back/plugins/x", "front"]);
        git(&local.join("front"), &["checkout", "-q", "-b", "feature-y"]);
        let repos = e.repos();
        assert_eq!(repos[2], Repo { path: "front".into(), branch: Some("feature-y".into()) });
        assert!(repos[0].branch.is_some());
        let tracked = e.tracked().unwrap();
        assert_eq!(tracked.iter().map(String::as_str).collect::<Vec<_>>(), ["back/README", "back/plugins/x/README", "front/README"]);

        fs::create_dir_all(remote.join("back/plugins/x/src")).unwrap();
        fs::write(remote.join("back/plugins/x/src/new.rs"), "claude\n").unwrap();
        fs::write(remote.join("back/api.rs"), "claude\n").unwrap();
        fs::write(remote.join("loose.txt"), "claude\n").unwrap();
        e.fetch().unwrap();
        for f in e.scan().unwrap() {
            e.apply(&f.path, f.version, Action::TakeRemoteFile, None).unwrap();
        }
        let ls = |dir: &str| String::from_utf8(std::process::Command::new("git").arg("-C").arg(local.join(dir)).args(["ls-files"]).output().unwrap().stdout).unwrap();
        assert_eq!(ls("back/plugins/x"), "README\nsrc/new.rs\n");
        assert_eq!(ls("back"), "README\napi.rs\n");
        assert!(local.join("loose.txt").exists());
        assert!(e.tracked().unwrap().contains("back/plugins/x/src/new.rs"));
        assert!(e.scan().unwrap().is_empty());

        fs::remove_file(remote.join("back/plugins/x/src/new.rs")).unwrap();
        e.fetch().unwrap();
        let f = e.scan().unwrap().into_iter().find(|f| f.path == "back/plugins/x/src/new.rs").unwrap();
        e.apply(&f.path, f.version, Action::TakeRemoteFile, None).unwrap();
        assert_eq!(ls("back/plugins/x"), "README\n");
    }

    #[test]
    fn commit_and_push_each_repository() {
        let root = std::env::temp_dir().join(format!("agent-diff-test-commit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let local = root.join("local");
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap()
        };
        let ident = ["-c", "user.name=t", "-c", "user.email=t@t"];
        for repo in ["", "lib"] {
            let dir = local.join(repo);
            fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            git(&dir, &["config", "user.name", "t"]);
            git(&dir, &["config", "user.email", "t@t"]);
            fs::write(dir.join("a.txt"), "1\n").unwrap();
            fs::write(dir.join("gone.txt"), "x\n").unwrap();
            git(&dir, &["add", "a.txt", "gone.txt"]);
            git(&dir, &[&ident[..], &["commit", "-qm", "init"]].concat());
        }
        fs::write(local.join(".gitignore"), "lib/\n").unwrap();
        git(&root, &["init", "-q", "--bare", "origin.git"]);
        git(&local.join("lib"), &["remote", "add", "origin", root.join("origin.git").to_str().unwrap()]);

        assert!(uncommitted(&local).unwrap().is_empty());
        fs::write(local.join("lib/a.txt"), "2\n").unwrap();
        fs::remove_file(local.join("lib/gone.txt")).unwrap();
        fs::write(local.join("lib/new.txt"), "n\n").unwrap();
        git(&local.join("lib"), &["add", "new.txt"]);
        fs::write(local.join("a.txt"), "root\n").unwrap();
        fs::write(local.join("untracked.txt"), "u\n").unwrap();

        let changes = uncommitted(&local).unwrap();
        let summary: Vec<(String, Vec<(String, String)>)> = changes.iter().map(|c| (c.repo.path.clone(), c.files.clone())).collect();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(summary[0], ("".into(), vec![pair("M", "a.txt")]));
        assert_eq!(summary[1], ("lib".into(), vec![pair("M", "lib/a.txt"), pair("D", "lib/gone.txt"), pair("A", "lib/new.txt")]));
        assert_eq!(changes[1].repo.branch.as_deref(), Some("main"));

        let hash = commit(&local, "lib", "lib work").unwrap();
        assert_eq!(git(&local.join("lib"), &["rev-parse", "--short", "HEAD"]).trim(), hash);
        assert_eq!(git(&local.join("lib"), &["ls-files"]), "a.txt\nnew.txt\n");
        push_repo(&local, "lib").unwrap();
        assert_eq!(git(&root.join("origin.git"), &["log", "-1", "--format=%s", "main"]).trim(), "lib work");
        fs::write(local.join("lib/a.txt"), "3\n").unwrap();
        commit(&local, "lib", "again").unwrap();
        push_repo(&local, "lib").unwrap();
        assert_eq!(git(&root.join("origin.git"), &["log", "-1", "--format=%s", "main"]).trim(), "again");

        assert!(push_repo(&local, "").is_err());
        assert!(commit(&local, "lib", "nothing").is_err());
        assert_eq!(uncommitted(&local).unwrap().len(), 1);
    }

    #[test]
    fn nested_repositories_commit_first() {
        let mut repos = vec!["".to_string(), "a/b".into(), "a".into(), "c".into()];
        deepest_first(&mut repos);
        assert_eq!(repos, ["a/b", "a", "c", ""]);
    }

    #[test]
    fn status_with_renames() {
        let status = b"R  new.rs\0old.rs\0 M x.rs\0";
        assert_eq!(parse_status(status, "lib"), vec![("R".to_string(), "lib/new.rs".to_string()), ("M".to_string(), "lib/x.rs".to_string())]);
    }

    #[test]
    #[ignore]
    fn bench_pull() {
        let (local, remote, e) = setup("bench");
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&local).args(args).output().unwrap().status.success());
        for d in 0..30 {
            for f in 0..100 {
                let rel = format!("pkg{d}/src/file{f}.rs");
                let body: String = (0..80).map(|i| format!("line {i} of {rel}\n")).collect();
                for side in [&local, &remote] {
                    fs::create_dir_all(side.join(format!("pkg{d}/src"))).unwrap();
                    fs::write(side.join(&rel), &body).unwrap();
                }
            }
        }
        git(&["add", "."]);
        e.reset_base().unwrap();
        e.fetch().unwrap();
        for f in 0..40 {
            let rel = format!("pkg{}/src/file{f}.rs", f % 30);
            let body = fs::read_to_string(remote.join(&rel)).unwrap().replace("line 3 ", "line three ");
            fs::write(remote.join(&rel), body).unwrap();
        }
        for f in 0..20 {
            fs::write(remote.join(format!("pkg{f}/src/new{f}.rs")), "new\n").unwrap();
        }
        let t = std::time::Instant::now();
        e.fetch().unwrap();
        println!("fetch: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let entries = e.scan().unwrap();
        println!("one scan: {:?} ({} entries)", t.elapsed(), entries.len());
        let t = std::time::Instant::now();
        let repos = list_repos(&e.local_root);
        println!("list_repos: {:?} ({repos:?})", t.elapsed());
        let t = std::time::Instant::now();
        let mut scans = std::time::Duration::ZERO;
        for f in &entries {
            e.apply(&f.path, f.version, Action::TakeRemoteFile, None).unwrap();
            let s = std::time::Instant::now();
            e.current_version(&f.path, None);
            scans += s.elapsed();
        }
        println!("apply loop: {:?}, of which per-item scans {:?}", t.elapsed(), scans);
    }

    #[test]
    fn saving_a_remote_file_checks_it_did_not_change() {
        let (_local, remote, e) = setup("save-remote");
        fs::write(remote.join("src/a.txt"), "claude v1\n").unwrap();
        e.fetch().unwrap();
        assert!(e.save_remote("src/a.txt", Some(b"claude v1\n"), b"mine\n").unwrap());
        assert_eq!(fs::read_to_string(remote.join("src/a.txt")).unwrap(), "mine\n");

        fs::write(remote.join("src/a.txt"), "claude v2\n").unwrap();
        assert!(!e.save_remote("src/a.txt", Some(b"mine\n"), b"mine again\n").unwrap());
        assert_eq!(fs::read_to_string(remote.join("src/a.txt")).unwrap(), "claude v2\n");

        assert!(e.save_remote("src/a.txt", None, b"forced\n").unwrap());
        assert_eq!(fs::read_to_string(remote.join("src/a.txt")).unwrap(), "forced\n");
        assert!(e.save_remote("../escape.txt", None, b"x").is_err());
    }

    #[test]
    fn closest_repo_wins() {
        let repos = ["".to_string(), "a".into(), "a/b".into(), "ab".into()];
        assert_eq!(closest_repo(&repos, "a/b/c.rs"), Some("a/b"));
        assert_eq!(closest_repo(&repos, "a/bc.rs"), Some("a"));
        assert_eq!(closest_repo(&repos, "ab/x"), Some("ab"));
        assert_eq!(closest_repo(&repos, "abc"), Some(""));
        assert_eq!(closest_repo(&repos[1..], "z.rs"), None);
        assert_eq!(in_repo("a/b", "a/b/c.rs"), "c.rs");
    }

    #[test]
    fn never_escapes_through_symlinks() {
        let (local, remote, e) = setup("symlink");
        let outside = local.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        // Remote: `lib` is a symlink out of the workspace; local: a real dir.
        std::os::unix::fs::symlink(&outside, remote.join("lib")).unwrap();
        fs::create_dir_all(local.join("lib")).unwrap();
        fs::write(local.join("lib/x.txt"), "mine\n").unwrap();
        // Local: `evil` is a symlink out; remote: a real dir with a file.
        std::os::unix::fs::symlink(&outside, local.join("evil")).unwrap();
        fs::create_dir_all(remote.join("evil")).unwrap();
        fs::write(remote.join("evil/y.txt"), "claude\n").unwrap();

        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert!(s.iter().all(|f| !f.path.starts_with("lib/") && !f.path.starts_with("evil/")), "{s:?}");
        assert!(e.apply_simple("lib/x.txt", 0, Action::TakeLocalFile).is_err());
        assert!(e.apply_simple("evil/y.txt", 0, Action::TakeRemoteFile).is_err());
        assert!(e.apply_simple("../escape.txt", 0, Action::TakeRemoteFile).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn only_tracked_and_new_remote_files() {
        let (local, remote, e) = setup("scope");
        // Untracked local file: never synced, even if the remote differs.
        fs::write(local.join("notes.txt"), "private\n").unwrap();
        fs::write(remote.join("notes.txt"), "other\n").unwrap();
        // Local-only untracked file: not shown.
        fs::write(local.join("scratch.txt"), "x\n").unwrap();
        // New file from Claude: shown.
        fs::write(remote.join("feature.rs"), "fn f() {}\n").unwrap();

        e.fetch().unwrap();
        let s = e.scan().unwrap();
        let paths: Vec<_> = s.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["feature.rs"]);

        // Once pulled, it is `git add`ed and stays in scope.
        e.apply_simple("feature.rs", s[0].version, Action::TakeRemoteFile).unwrap();
        assert!(e.tracked().unwrap().contains("feature.rs"));
        assert!(e.scan().unwrap().is_empty());
        fs::write(remote.join("feature.rs"), "fn f() { g() }\n").unwrap();
        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].origin, Origin::Remote);
    }

    #[test]
    fn deletions_both_ways() {
        let (local, remote, e) = setup("deletions");
        let get = |p: &str| e.scan().unwrap().into_iter().find(|f| f.path == p).unwrap();

        // Deleted locally, pushed: deleted on the remote.
        fs::remove_file(local.join("src/a.txt")).unwrap();
        e.fetch().unwrap();
        let f = get("src/a.txt");
        assert_eq!((f.origin, f.local_change), (Origin::Local, Some(Change::Deleted)));
        assert!(e.apply_simple("src/a.txt", f.version, Action::TakeLocalFile).unwrap());
        e.push(&["src/a.txt".into()]).unwrap();
        assert!(!remote.join("src/a.txt").exists());
        assert!(!e.tracked().unwrap().contains("src/a.txt"));

        // Deleted on the remote, pulled: deleted locally and unstaged from git.
        fs::remove_file(remote.join(".gitignore")).unwrap();
        e.fetch().unwrap();
        let f = get(".gitignore");
        assert_eq!((f.origin, f.remote_change), (Origin::Remote, Some(Change::Deleted)));
        e.apply_simple(".gitignore", f.version, Action::TakeRemoteFile).unwrap();
        assert!(!local.join(".gitignore").exists());
        assert!(!e.tracked().unwrap().contains(".gitignore"));
        let status = std::process::Command::new("git").arg("-C").arg(&local).args(["status", "--porcelain"]).output().unwrap();
        let status = String::from_utf8_lossy(&status.stdout).to_string();
        // Both deletions are staged.
        assert!(status.lines().any(|l| l == "D  .gitignore"), "{status}");
        assert!(status.lines().any(|l| l == "D  src/a.txt"), "{status}");

        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());
    }

    fn git_in(dir: &Path, args: &[&str]) {
        assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success());
    }

    #[test]
    fn remote_rename_is_detected_and_pulled() {
        let (local, remote, e) = setup("rename-pull");
        fs::create_dir_all(remote.join("lib")).unwrap();
        fs::rename(remote.join("src/a.txt"), remote.join("lib/a.txt")).unwrap();
        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 1, "{s:?}");
        let f = &s[0];
        assert_eq!(f.path, "lib/a.txt");
        assert_eq!((f.origin, f.remote_change), (Origin::Remote, Some(Change::Renamed)));
        let r = f.rename.clone().unwrap();
        assert_eq!((r.from.as_str(), r.similarity), ("src/a.txt", 100));
        assert!(matches!(&f.body, Body::Text(t) if t.diff().hunks.is_empty()));

        assert!(e.apply(&f.path, f.version, Action::TakeRemoteFile, Some(&r)).unwrap().is_empty());
        assert!(local.join("lib/a.txt").exists() && !local.join("src/a.txt").exists());
        let tracked = e.tracked().unwrap();
        assert!(tracked.contains("lib/a.txt") && !tracked.contains("src/a.txt"));
        assert!(e.scan().unwrap().is_empty());
    }

    #[test]
    fn rename_with_edits_take_one_hunk() {
        let (local, remote, e) = setup("rename-hunk");
        fs::remove_file(remote.join("src/a.txt")).unwrap();
        fs::write(remote.join("src/b.txt"), "ONE\n2\n3\n4\n5\n6\n7\n8\n9\nTEN\n").unwrap();
        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 1, "{s:?}");
        let f = &s[0];
        let r = f.rename.clone().unwrap();
        assert_eq!((f.path.as_str(), r.similarity), ("src/b.txt", 80));
        assert!(matches!(&f.body, Body::Text(t) if t.diff().hunks.len() == 2));

        // Taking one change moves the file; the other change remains, as a
        // normal remote modification at the new path.
        e.apply(&f.path, f.version, Action::TakeRemoteHunk(0), Some(&r)).unwrap();
        assert!(!local.join("src/a.txt").exists());
        assert!(fs::read_to_string(local.join("src/b.txt")).unwrap().starts_with("ONE\n2\n"));
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!((s[0].path.as_str(), s[0].origin, s[0].rename.is_none()), ("src/b.txt", Origin::Remote, true));
        assert!(matches!(&s[0].body, Body::Text(t) if t.diff().hunks.len() == 1));
    }

    #[test]
    fn local_rename_is_pushed_and_remote_rename_undone() {
        let (local, remote, e) = setup("rename-push");
        // You `git mv` a file: pushing moves it on the remote too.
        git_in(&local, &["mv", "src/a.txt", "src/moved.txt"]);
        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 1, "{s:?}");
        let f = &s[0];
        assert_eq!((f.path.as_str(), f.origin, f.local_change), ("src/moved.txt", Origin::Local, Some(Change::Renamed)));
        let pushed = e.apply(&f.path, f.version, Action::TakeLocalFile, f.rename.as_ref()).unwrap();
        e.push(&pushed).unwrap();
        assert!(remote.join("src/moved.txt").exists() && !remote.join("src/a.txt").exists());
        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());

        // Claude renames it again; undoing that on the remote moves it back.
        fs::rename(remote.join("src/moved.txt"), remote.join("src/claude.txt")).unwrap();
        e.fetch().unwrap();
        let f = e.scan().unwrap().remove(0);
        assert_eq!(f.rename.as_ref().unwrap().from, "src/moved.txt");
        let pushed = e.apply(&f.path, f.version, Action::TakeLocalFile, f.rename.as_ref()).unwrap();
        e.push(&pushed).unwrap();
        assert!(remote.join("src/moved.txt").exists() && !remote.join("src/claude.txt").exists());
        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());
    }

    #[test]
    fn unrelated_delete_and_add_are_not_a_rename() {
        let (_local, remote, e) = setup("no-rename");
        fs::remove_file(remote.join("src/a.txt")).unwrap();
        fs::write(remote.join("other.txt"), "something\nelse\nentirely\n").unwrap();
        e.fetch().unwrap();
        let s = e.scan().unwrap();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|f| f.rename.is_none()));
    }

    #[test]
    fn make_remote_identical_to_local() {
        let (local, remote, e) = setup("overwrite-remote");
        fs::write(local.join("mine.txt"), "mine\n").unwrap();
        fs::write(remote.join("mine.txt"), "mine\n").unwrap();
        git_in(&local, &["add", "mine.txt"]);
        git_in(&local, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "mine"]);
        e.fetch().unwrap();
        e.scan().unwrap();
        // Claude: edits a file, adds one, deletes one, renames one, and leaves
        // an ignored build artifact.
        fs::write(remote.join(".gitignore"), "target/\nclaude\n").unwrap();
        fs::write(remote.join("claude-new.txt"), "new\n").unwrap();
        fs::remove_file(remote.join("mine.txt")).unwrap();
        fs::rename(remote.join("src/a.txt"), remote.join("src/renamed.txt")).unwrap();
        fs::create_dir_all(remote.join("target")).unwrap();
        fs::write(remote.join("target/out"), "bin").unwrap();
        e.fetch().unwrap();
        let entries = e.scan().unwrap();
        assert_eq!(entries.len(), 4, "{entries:?}");

        // What the button sends: push the local side of every entry.
        let mut pushed = Vec::new();
        for f in &entries {
            pushed.extend(e.apply(&f.path, f.version, Action::TakeLocalFile, f.rename.as_ref()).unwrap());
        }
        e.push(&pushed).unwrap();

        e.fetch().unwrap();
        assert!(e.scan().unwrap().is_empty());
        assert_eq!(fs::read_to_string(remote.join(".gitignore")).unwrap(), "target/\n");
        assert!(remote.join("mine.txt").exists() && remote.join("src/a.txt").exists());
        assert!(!remote.join("claude-new.txt").exists() && !remote.join("src/renamed.txt").exists());
        // Ignored files are left alone.
        assert!(remote.join("target/out").exists());
    }

    #[test]
    fn unchanged_files_are_listed_separately() {
        let (local, remote, e) = setup("unchanged");
        fs::write(local.join("untracked.txt"), "x\n").unwrap();
        fs::write(remote.join("untracked.txt"), "x\n").unwrap();
        fs::write(remote.join("src/a.txt"), "changed\n").unwrap();
        e.fetch().unwrap();
        let (diffs, synced) = e.scan_all().unwrap();
        assert_eq!(diffs.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["src/a.txt"]);
        // Tracked and identical: listed. Untracked: never.
        assert_eq!(synced.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), [".gitignore"]);
        assert!(synced.iter().all(|f| f.origin == Origin::Synced && matches!(f.body, Body::Unchanged)));
    }
}

