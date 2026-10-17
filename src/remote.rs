use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the remote workspace lives. Anything rsync understands as a
/// remote (`host:path`, `user@host:path`) goes over ssh; everything else is
/// treated as a local directory (handy for testing or sshfs mounts).
#[derive(Clone, Debug)]
pub enum Remote {
    Local(PathBuf),
    Ssh { host: String, path: String },
}

pub const SHARED_CONNECTION: [&str; 14] = [
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=~/.agent-diff/ssh/%C",
    "-o", "ControlPersist=120",
    "-o", "ConnectTimeout=15",
    "-o", "ServerAliveInterval=10",
    "-o", "ServerAliveCountMax=3",
    "-o", "TCPKeepAlive=yes",
];

fn ssh_options() -> Vec<&'static str> {
    let mut options = vec!["-o", "BatchMode=yes"];
    options.extend(SHARED_CONNECTION);
    options
}

pub fn shell_quote(s: &str) -> String {
    let quoted = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    match s.strip_prefix("~/") {
        Some(rest) => format!("~/{}", quoted(rest)),
        None if s == "~" => "~".into(),
        None => quoted(s),
    }
}

impl Remote {
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            bail!("remote is empty");
        }
        if let Some((host, path)) = spec.split_once(':') {
            if !host.is_empty() && !host.contains('/') {
                let path = if path.is_empty() { "." } else { path };
                return Ok(Remote::Ssh { host: host.to_string(), path: path.trim_end_matches('/').to_string() });
            }
        }
        Ok(Remote::Local(PathBuf::from(spec)))
    }

    pub fn display(&self) -> String {
        match self {
            Remote::Local(p) => p.display().to_string(),
            Remote::Ssh { host, path } => format!("{host}:{path}"),
        }
    }

    /// rsync argument for the remote root, with a trailing slash so rsync
    /// copies the directory contents rather than the directory itself.
    fn rsync_root(&self) -> String {
        match self {
            Remote::Local(p) => format!("{}/", p.display().to_string().trim_end_matches('/')),
            Remote::Ssh { host, path } => format!("{host}:{path}/"),
        }
    }

    fn rsync(&self) -> Command {
        let mut cmd = Command::new("rsync");
        if let Remote::Ssh { .. } = self {
            cmd.arg(RSYNC_IO_TIMEOUT);
            // Reuse one ssh connection between polls; BatchMode because a GUI
            // cannot answer a password prompt (use key auth / ssh-agent).
            cmd.arg("-e").arg(format!("ssh {}", ssh_options().join(" ")));
        }
        cmd
    }

    pub fn forget_connection(&self) {
        if let Remote::Ssh { host, .. } = self {
            let _ = Command::new("ssh").args(ssh_options()).args(["-O", "exit"]).arg(host).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }

    /// Mirror the remote workspace into `mirror`. Returns true if anything changed.
    pub fn fetch(&self, mirror: &Path, excluded: Option<&Excluded>, progress: &mut dyn FnMut(Fetching)) -> Result<bool> {
        match self.fetch_with(mirror, excluded, FetchMode::Reporting, progress) {
            Err(e) if e.to_string().contains("--info") => self.fetch_with(mirror, excluded, FetchMode::Quiet, progress),
            result => result,
        }
    }

    fn shell(&self) -> (String, Command) {
        match self {
            Remote::Local(path) => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c");
                (path.display().to_string(), cmd)
            }
            Remote::Ssh { host, path } => {
                let mut cmd = Command::new("ssh");
                cmd.args(ssh_options()).arg(host);
                (path.clone(), cmd)
            }
        }
    }

    fn run_on_root(&self, script: impl Fn(&str) -> String) -> Result<()> {
        let root = shell_quote(&self.root_path());
        self.run(&script(&root))
    }

    fn run(&self, script: &str) -> Result<()> {
        let (_, mut cmd) = self.shell();
        let out = cmd.arg(script).stdin(Stdio::null()).output().context("failed to reach the remote")?;
        if !out.status.success() {
            self.forget_connection();
            bail!("{}: {}", self.display(), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    pub fn command(&self, script: &str) -> Command {
        let (_, mut cmd) = self.shell();
        cmd.arg(script);
        cmd
    }

    pub fn root_path(&self) -> String {
        match self {
            Remote::Local(path) => path.display().to_string(),
            Remote::Ssh { path, .. } => path.clone(),
        }
    }

    pub fn same_host(&self, other: &Remote) -> bool {
        match (self, other) {
            (Remote::Local(_), Remote::Local(_)) => true,
            (Remote::Ssh { host, .. }, Remote::Ssh { host: other, .. }) => host == other,
            _ => false,
        }
    }

    pub fn copy_missing_from(&self, source: &Remote, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        if !self.same_host(source) {
            bail!("{} and {} are not on the same host", source.display(), self.display());
        }
        self.run(&copy_missing_script(&source.root_path(), &self.root_path(), paths))
    }

    pub fn shell_line(&self, script: &str) -> String {
        match self {
            Remote::Local(_) => script.to_string(),
            Remote::Ssh { host, .. } => format!("ssh {} {}", shell_quote(host), shell_quote(script)),
        }
    }

    pub fn remove_dir_line(&self) -> String {
        self.shell_line(&format!("rm -rf -- {}", shell_quote(&self.root_path())))
    }

    pub fn create_dir_all(&self) -> Result<()> {
        self.run_on_root(|root| format!("mkdir -p -- {root}"))
    }

    pub fn exists(&self) -> Result<bool> {
        let (root, mut cmd) = self.shell();
        let out = cmd.arg(format!("test -e {}", shell_quote(&root))).stdin(Stdio::null()).output().context("failed to reach the remote")?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!("{}: {}", self.display(), String::from_utf8_lossy(&out.stderr).trim()),
        }
    }

    pub fn remove_dir_all(&self) -> Result<()> {
        self.run_on_root(|root| format!("rm -rf -- {root}"))
    }

    pub fn gitignored_paths(&self, repos: &[String]) -> Option<Vec<String>> {
        if repos.is_empty() {
            return None;
        }
        let (root, mut cmd) = self.shell();
        let out = cmd.arg(gitignored_script(&root, repos)).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_gitignored(&out.stdout)
    }

    fn fetch_with(&self, mirror: &Path, excluded: Option<&Excluded>, mode: FetchMode, progress: &mut dyn FnMut(Fetching)) -> Result<bool> {
        let mut cmd = self.rsync();
        cmd.args(["-rlpt", "--delete", "--delete-excluded", "-i", "--exclude=.git"]);
        if let Some(excluded) = excluded {
            cmd.arg(format!("--exclude-from={}", excluded.rules.display()));
        }
        if excluded.is_none_or(|e| !e.listed_by_remote) {
            cmd.arg("--filter=:- .gitignore");
        }
        if let FetchMode::Reporting = mode {
            cmd.args(["--info=progress2,flist2", "--no-inc-recursive"]);
        }
        let child = cmd
            .arg(self.rsync_root())
            .arg(format!("{}/", mirror.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run rsync (is it installed?)")?;
        let mut changed = false;
        let (status, stderr) = run_reading_lines(child, &mut |line| match (transfer_of(line), listed_of(line)) {
            (Some(transfer), _) => progress(Fetching::Transfer(transfer)),
            (None, Some(listed)) => progress(Fetching::Listing(listed)),
            (None, None) => changed |= !line.trim().is_empty() && !is_file_list_notice(line),
        })?;
        // 24 = "some source files vanished", normal while the agent is writing.
        if !status.success() && status.code() != Some(24) {
            self.forget_connection();
            if stderr.contains("change_dir") && stderr.contains("No such file or directory") {
                bail!("remote directory {} does not exist", self.display());
            }
            bail!("rsync fetch failed: {}", stderr.trim());
        }
        Ok(changed)
    }

    /// Send the given files from `mirror` to the remote. Files missing from the
    /// mirror are deleted on the remote.
    pub fn replace_with(&self, local_root: &Path, included: &Path) -> Result<()> {
        let mut cmd = self.rsync();
        cmd.args(["-rlpt", "--delete", "--delete-excluded", "--filter=P .git/", "--exclude=.git/"])
            .arg(format!("--include-from={}", included.display()))
            .arg("--exclude=*")
            .arg(format!("{}/", local_root.display()))
            .arg(self.rsync_root());
        let out = cmd.stdin(Stdio::null()).output().context("failed to run rsync (is it installed?)")?;
        if !out.status.success() {
            self.forget_connection();
            bail!("replacing the remote failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    pub fn push(&self, mirror: &Path, paths: &[String], progress: &mut dyn FnMut(f32)) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        match self.push_with(mirror, paths, PushMode::Reporting, progress) {
            Err(e) if e.to_string().contains("--info") => self.push_with(mirror, paths, PushMode::Quiet, progress),
            result => result,
        }
    }

    pub fn push_unless_newer(&self, mirror: &Path, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.push_with(mirror, paths, PushMode::UnlessNewer, &mut |_| {})
    }

    fn push_with(&self, mirror: &Path, paths: &[String], mode: PushMode, progress: &mut dyn FnMut(f32)) -> Result<()> {
        let mut cmd = self.rsync();
        cmd.args(["-lpt", "--delete-missing-args", "--from0", "--files-from=-"]);
        match mode {
            PushMode::Reporting => cmd.arg("--info=progress2"),
            PushMode::Quiet => &mut cmd,
            PushMode::UnlessNewer => cmd.arg("--update"),
        };
        let mut child = cmd
            .arg(format!("{}/", mirror.display()))
            .arg(self.rsync_root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run rsync (is it installed?)")?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let list: Vec<u8> = paths.iter().flat_map(|p| p.as_bytes().iter().copied().chain([0])).collect();
        let writer = std::thread::spawn(move || stdin.write_all(&list));
        let (status, stderr) = run_reading_lines(child, &mut |line| {
            if let Some(transfer) = transfer_of(line) {
                progress(transfer.fraction);
            }
        })?;
        let _ = writer.join();
        if matches!(mode, PushMode::UnlessNewer) && status.code().is_some_and(|code| SOME_FILES_NOT_SENT.contains(&code)) {
            return Ok(());
        }
        if !status.success() {
            self.forget_connection();
            bail!("rsync push failed: {}", stderr.trim());
        }
        progress(1.0);
        Ok(())
    }
}

fn run_reading_lines(mut child: std::process::Child, on_line: &mut dyn FnMut(&str)) -> Result<(std::process::ExitStatus, String)> {
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut line = Vec::new();
    let mut buf = [0u8; 8192];
    while let Ok(n @ 1..) = stdout.read(&mut buf) {
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                on_line(&String::from_utf8_lossy(&line));
                line.clear();
            } else {
                line.push(b);
            }
        }
    }
    on_line(&String::from_utf8_lossy(&line));
    let status = child.wait()?;
    Ok((status, errors.join().unwrap_or_default()))
}

pub struct Excluded {
    pub rules: PathBuf,
    pub listed_by_remote: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transfer {
    pub fraction: f32,
    pub bytes: u64,
    pub files_left: Option<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fetching {
    AskingRemoteGit,
    Listing(u64),
    Transfer(Transfer),
}

fn listed_of(line: &str) -> Option<u64> {
    line.trim().strip_suffix(" files...")?.parse().ok()
}

fn is_file_list_notice(line: &str) -> bool {
    let line = line.trim();
    line.contains("file list ...") || line.ends_with(" files to consider") || line.ends_with(" file to consider")
}

fn transfer_of(line: &str) -> Option<Transfer> {
    if !line.starts_with(' ') {
        return None;
    }
    let fraction = progress_percent(line)?;
    let bytes = line.split_whitespace().next().and_then(|t| t.replace(',', "").parse().ok()).unwrap_or(0);
    Some(Transfer { fraction, bytes, files_left: files_left(line) })
}

fn files_left(line: &str) -> Option<(u64, u64)> {
    let counts = line.split(['=', ')']).skip_while(|t| !t.ends_with("to-chk") && !t.ends_with("ir-chk")).nth(1)?;
    let (left, total) = counts.split_once('/')?;
    Some((left.parse().ok()?, total.parse().ok()?))
}

const REPO_MARKER: &str = "//repo:";
const LISTING_FAILED: &str = "//failed";

fn gitignored_script(root: &str, repos: &[String]) -> String {
    repos
        .iter()
        .map(|repo| {
            let dir = if repo.is_empty() { shell_quote(root) } else { format!("{}/{}", shell_quote(root), shell_quote(repo)) };
            let marker = shell_quote(&format!("{REPO_MARKER}{repo}"));
            format!("printf '%s\\0' {marker} && {{ [ ! -d {dir} ] || (cd {dir} && git ls-files -z --others --ignored --exclude-standard --directory) || printf '%s\\0' '{LISTING_FAILED}'; }}")
        })
        .collect::<Vec<_>>()
        .join(" && ")
}

fn copy_missing_script(source: &str, destination: &str, paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| {
            let path = path.trim_end_matches('/');
            let parent = path.rsplit_once('/').map(|(parent, _)| format!("/{parent}")).unwrap_or_default();
            let (from, to, folder) = (shell_quote(&format!("{source}/{path}")), shell_quote(&format!("{destination}/{path}")), shell_quote(&format!("{destination}{parent}")));
            format!("{{ [ -e {to} ] || [ ! -e {from} ] || {{ mkdir -p -- {folder} && cp -a -- {from} {to}; }}; }}")
        })
        .collect::<Vec<_>>()
        .join(" && ")
}

fn parse_gitignored(output: &[u8]) -> Option<Vec<String>> {
    let mut repo = String::new();
    let mut paths = Vec::new();
    for item in output.split(|&b| b == 0).filter(|item| !item.is_empty()) {
        let item = String::from_utf8_lossy(item);
        if item == LISTING_FAILED {
            return None;
        }
        match item.strip_prefix(REPO_MARKER) {
            Some(name) => repo = name.to_string(),
            None if repo.is_empty() => paths.push(item.into_owned()),
            None => paths.push(format!("{repo}/{item}")),
        }
    }
    Some(paths)
}

const RSYNC_IO_TIMEOUT: &str = "--timeout=60";
const SOME_FILES_NOT_SENT: [i32; 2] = [23, 24];

#[derive(Clone, Copy)]
enum FetchMode {
    Reporting,
    Quiet,
}

#[derive(Clone, Copy)]
enum PushMode {
    Reporting,
    Quiet,
    UnlessNewer,
}

fn progress_percent(line: &str) -> Option<f32> {
    let token = line.split_whitespace().find(|t| t.ends_with('%'))?;
    let percent: f32 = token.trim_end_matches('%').parse().ok()?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn reads_rsync_progress_lines() {
        assert_eq!(progress_percent("      1,234,567  45%   12.34MB/s    0:00:01 (xfr#3, to-chk=5/10)"), Some(0.45));
        assert_eq!(progress_percent("    100%"), Some(1.0));
        assert_eq!(progress_percent("sending incremental file list"), None);
    }

    #[test]
    fn reads_transfer_lines_but_not_itemized_files() {
        let line = "      1,234,567  45%   12.34MB/s    0:00:01 (xfr#3, to-chk=5/10)";
        assert_eq!(transfer_of(line), Some(Transfer { fraction: 0.45, bytes: 1234567, files_left: Some((5, 10)) }));
        assert_eq!(transfer_of(">f+++++++++ 50% done.txt"), None);
        assert_eq!(transfer_of("*deleting   old.txt"), None);
    }

    #[test]
    fn reads_file_list_progress() {
        assert_eq!(listed_of(" 1200 files..."), Some(1200));
        assert_eq!(listed_of(">f+++++++++ 12 files..."), None);
        assert!(is_file_list_notice("building file list ... "));
        assert!(is_file_list_notice("receiving file list ... done"));
        assert!(is_file_list_notice("3031 files to consider"));
        assert!(!is_file_list_notice(">f+++++++++ a.txt"));
    }

    #[test]
    fn fetch_reports_listing_and_no_change_on_a_second_run() {
        let root = std::env::temp_dir().join(format!("agent-diff-fetch-progress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (mirror, remote) = (root.join("mirror"), root.join("remote"));
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        for i in 0..2000 {
            std::fs::write(remote.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let remote = Remote::Local(remote);
        let mut seen = Vec::new();
        assert!(remote.fetch(&mirror, None, &mut |p| seen.push(p)).unwrap());
        assert!(seen.iter().any(|p| matches!(p, Fetching::Listing(n) if *n > 0)), "{seen:?}");
        assert!(seen.iter().any(|p| matches!(p, Fetching::Transfer(_))), "{seen:?}");
        assert!(!remote.fetch(&mirror, None, &mut |_| {}).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn push_reports_progress_up_to_the_end() {
        let root = std::env::temp_dir().join(format!("agent-diff-push-progress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (mirror, remote) = (root.join("mirror"), root.join("remote"));
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        let paths: Vec<String> = (0..20).map(|i| format!("f{i}.txt")).collect();
        for p in &paths {
            std::fs::write(mirror.join(p), vec![b'x'; 64 * 1024]).unwrap();
        }
        let mut seen = Vec::new();
        Remote::Local(remote.clone()).push(&mirror, &paths, &mut |p| seen.push(p)).unwrap();
        assert_eq!(seen.last(), Some(&1.0), "{seen:?}");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert!(remote.join("f19.txt").exists());
    }
}
