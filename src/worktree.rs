use crate::engine::git_at;
use crate::remote::Remote;
use crate::setup::{self, Setup};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const FOLDER_SUFFIX: &str = ".wt";

pub fn folder_name(branch: &str) -> String {
    branch.trim().chars().map(|c| if c == '/' || c.is_whitespace() { '-' } else { c }).collect()
}

pub fn sibling_path(path: &str, folder: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((parent, _)) if parent.ends_with(FOLDER_SUFFIX) => format!("{parent}/{folder}"),
        _ => format!("{trimmed}{FOLDER_SUFFIX}/{folder}"),
    }
}

pub fn sibling_remote(remote: &str, folder: &str) -> String {
    match Remote::parse(remote) {
        Ok(Remote::Ssh { host, path }) => format!("{host}:{}", sibling_path(&path, folder)),
        _ => sibling_path(remote, folder),
    }
}

fn in_worktree_folder(path: &str) -> bool {
    path.trim_end_matches('/').rsplit_once('/').is_some_and(|(parent, name)| parent.ends_with(FOLDER_SUFFIX) && !name.is_empty() && name != "." && name != "..")
}

pub fn remote_in_worktree_folder(remote: &Remote) -> bool {
    match remote {
        Remote::Ssh { path, .. } => in_worktree_folder(path),
        Remote::Local(path) => in_worktree_folder(&path.display().to_string()),
    }
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_at(dir, args)?).trim().to_string())
}

fn has_ref(repo: &Path, name: &str) -> bool {
    git_at(repo, &["rev-parse", "--verify", "--quiet", name]).is_ok()
}

fn remote_branch(repo: &Path, branch: &str) -> Option<String> {
    let listed = git_text(repo, &["for-each-ref", "--format=%(refname:short)", &format!("refs/remotes/*/{branch}")]).ok()?;
    let refs: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
    match refs.as_slice() {
        [only] => Some(only.to_string()),
        _ => None,
    }
}

pub fn branches(repo: &Path) -> Vec<String> {
    let listed = git_text(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes"]).unwrap_or_default();
    listed.lines().filter(|l| !l.is_empty() && !l.ends_with("/HEAD")).map(str::to_string).collect()
}

pub fn add(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<()> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("the branch name is empty");
    }
    git_at(repo, &["check-ref-format", "--branch", branch]).with_context(|| format!("{branch} is not a valid branch name"))?;
    let dir_arg = dir.display().to_string();
    let base = if base.trim().is_empty() { "HEAD" } else { base.trim() };
    if has_ref(repo, &format!("refs/heads/{branch}")) {
        git_at(repo, &["worktree", "add", &dir_arg, branch])?;
    } else if let Some(tracked) = remote_branch(repo, branch) {
        git_at(repo, &["worktree", "add", "--track", "-b", branch, &dir_arg, &tracked])?;
    } else {
        git_at(repo, &["worktree", "add", "-b", branch, &dir_arg, base])?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub main: bool,
    pub missing: bool,
}

pub fn list(repo: &Path) -> Vec<Listed> {
    let listed = git_text(repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    parse_list(&listed)
}

fn parse_list(porcelain: &str) -> Vec<Listed> {
    let mut found: Vec<Listed> = Vec::new();
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            found.push(Listed { path: PathBuf::from(path), branch: None, main: found.is_empty(), missing: false });
        } else if let Some(last) = found.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                last.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).to_string());
            } else if line.starts_with("prunable") {
                last.missing = true;
            }
        }
    }
    found
}

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub name: String,
    pub upstream: Option<String>,
    pub current: bool,
    pub checked_out_in: Option<PathBuf>,
}

impl Branch {
    pub fn remote_and_name(&self) -> Option<(&str, &str)> {
        self.upstream.as_deref()?.split_once('/')
    }
}

const BRANCH_FORMAT: &str = "--format=%(refname:short)%00%(upstream:short)%00%(HEAD)%00%(worktreepath)";

pub fn local_branches(repo: &Path) -> Vec<Branch> {
    let listed = git_text(repo, &["for-each-ref", BRANCH_FORMAT, "refs/heads"]).unwrap_or_default();
    parse_branches(&listed)
}

fn parse_branches(listed: &str) -> Vec<Branch> {
    let present = |field: &str| (!field.is_empty()).then(|| field.to_string());
    listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next().filter(|n| !n.is_empty())?.to_string();
            let upstream = present(fields.next().unwrap_or(""));
            let current = fields.next() == Some("*");
            let checked_out_in = present(fields.next().unwrap_or("")).map(PathBuf::from);
            Some(Branch { name, upstream, current, checked_out_in })
        })
        .collect()
}

pub fn branch_deletion(repo: &Path, branch: &Branch, force: bool, on_remote: bool) -> Result<String> {
    if let Some(folder) = &branch.checked_out_in {
        bail!("{} is checked out in {}", branch.name, folder.display());
    }
    let quote = crate::remote::shell_quote;
    let repo = quote(&repo.display().to_string());
    let flag = if force { "-D" } else { "-d" };
    let local = format!("git -C {repo} branch {flag} {}", quote(&branch.name));
    if !on_remote {
        return Ok(local);
    }
    let (remote, name) = branch.remote_and_name().with_context(|| format!("{} has no upstream branch", branch.name))?;
    Ok(format!("{local} && git -C {repo} push {} --delete {}", quote(remote), quote(name)))
}

pub fn is_linked(dir: &Path) -> bool {
    if !dir.join(".git").is_file() {
        return false;
    }
    let dirs = git_text(dir, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"]).unwrap_or_default();
    let mut lines = dirs.lines();
    matches!((lines.next(), lines.next()), (Some(own), Some(common)) if own != common)
}

pub fn main_worktree(dir: &Path) -> Result<PathBuf> {
    let common = git_text(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let common = PathBuf::from(common);
    if common.file_name().is_some_and(|name| name == ".git") {
        return Ok(common.parent().context("no main worktree")?.to_path_buf());
    }
    Ok(common)
}

pub fn uncommitted(dir: &Path) -> Vec<String> {
    git_text(dir, &["status", "--porcelain"]).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn tracked_files(dir: &Path) -> Result<Vec<String>> {
    let listed = git_at(dir, &["ls-files", "-z"])?;
    Ok(listed.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).filter(|p| dir.join(p).is_file()).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Extra {
    pub path: String,
    pub folder: bool,
    pub local: bool,
    pub remote: bool,
}

pub fn gitignored_extras(local: &Path, remote: &Remote) -> Vec<Extra> {
    let listed = git_at(local, &["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]).unwrap_or_default();
    let local_paths = listed.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned());
    let remote_paths = remote.gitignored_paths(&[String::new()]).unwrap_or_default();
    let mut extras: std::collections::BTreeMap<String, Extra> = Default::default();
    let mut note = |listed: String, on_local: bool| {
        let path = listed.trim_end_matches('/').to_string();
        let extra = extras.entry(path.clone()).or_insert(Extra { path, folder: false, local: false, remote: false });
        extra.folder |= listed.ends_with('/');
        if on_local { extra.local = true } else { extra.remote = true }
    };
    local_paths.for_each(|p| note(p, true));
    remote_paths.into_iter().for_each(|p| note(p, false));
    extras.into_values().collect()
}

pub struct NewWorktree {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    pub local: PathBuf,
    pub remote: String,
    pub source_remote: String,
    pub extras: Vec<Extra>,
    pub setups: Vec<Setup>,
    pub workspaces_dir: PathBuf,
}

pub fn discover_setup(repo: &Path, extras: &[Extra]) -> Vec<Setup> {
    setup::discover(&tracked_files(repo).unwrap_or_default(), extras)
}

impl NewWorktree {
    fn extras_on(&self, on_local: bool) -> Vec<String> {
        self.extras.iter().filter(|e| if on_local { e.local } else { e.remote }).map(|e| e.path.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    CheckingFolders,
    AddingWorktree,
    CreatingRemoteFolder,
    CopyingToRemote { files: usize, fraction: f32 },
    CopyingLocalExtras { entries: usize },
    CopyingRemoteExtras { entries: usize },
    PreparingMirror { files: usize },
    RunningSetup { label: String, last_line: String, outcome: Option<bool> },
    Undoing,
}

impl Step {
    pub fn describe(&self, plan: &NewWorktree) -> String {
        match self {
            Step::CheckingFolders => format!("checking that {} and {} are free", plan.local.display(), plan.remote),
            Step::AddingWorktree => format!("git worktree add {} ({})", plan.local.display(), plan.branch),
            Step::CreatingRemoteFolder => format!("creating {} on the remote", plan.remote),
            Step::CopyingToRemote { files, fraction } => format!("copying {files} tracked files to the remote: {:.0}%", fraction * 100.0),
            Step::CopyingLocalExtras { entries } => format!("copying {entries} gitignored entries into the new local worktree"),
            Step::CopyingRemoteExtras { entries } => format!("copying {entries} gitignored entries into the new remote folder, from {}", plan.source_remote),
            Step::PreparingMirror { files } => format!("preparing the local copy of the remote ({files} files, no download)"),
            Step::RunningSetup { label, last_line, .. } if last_line.is_empty() => label.clone(),
            Step::RunningSetup { label, last_line, .. } => format!("{label}: {}", shortened(last_line)),
            Step::Undoing => "undoing: removing the new worktree and remote folder".to_string(),
        }
    }

    pub fn same_kind(&self, other: &Step) -> bool {
        match (self, other) {
            (Step::RunningSetup { label, .. }, Step::RunningSetup { label: other, .. }) => label == other,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

const SHOWN_OUTPUT: usize = 90;

fn shortened(line: &str) -> String {
    match line.char_indices().nth(SHOWN_OUTPUT) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

pub fn create(plan: &NewWorktree, report: &mut dyn FnMut(Step)) -> Result<Vec<String>> {
    let remote = Remote::parse(&plan.remote)?;
    report(Step::CheckingFolders);
    if plan.local.exists() {
        bail!("{} already exists", plan.local.display());
    }
    if remote.exists()? {
        bail!("{} already exists on the remote", remote.display());
    }
    report(Step::AddingWorktree);
    add(&plan.repo, &plan.local, &plan.branch, &plan.base)?;
    let files = tracked_files(&plan.local);
    let copied = files.and_then(|files| {
        copy_to_remote(&remote, &plan.local, &files, report)?;
        copy_extras(plan, &remote, report)?;
        Ok(files)
    });
    let files = match copied {
        Ok(files) => files,
        Err(e) => {
            report(Step::Undoing);
            let _ = git_at(&plan.repo, &["worktree", "remove", "--force", &plan.local.display().to_string()]);
            let _ = remote.remove_dir_all();
            return Err(e.context("could not copy the worktree to the remote"));
        }
    };
    report(Step::PreparingMirror { files: files.len() });
    let _ = prepare_mirror(&plan.local, &remote, &plan.workspaces_dir, &files);
    Ok(run_setups(plan, &remote, report))
}

fn run_setups(plan: &NewWorktree, remote: &Remote, report: &mut dyn FnMut(Step)) -> Vec<String> {
    let mut failures = Vec::new();
    for chosen in &plan.setups {
        for on_remote in [false, true].into_iter().filter(|&on_remote| if on_remote { chosen.remote } else { chosen.local }) {
            let label = setup::label(chosen, on_remote);
            report(Step::RunningSetup { label: label.clone(), last_line: String::new(), outcome: None });
            let mut on_line = |line: &str| report(Step::RunningSetup { label: label.clone(), last_line: line.to_string(), outcome: None });
            let ran = if on_remote { setup::run_remote(remote, chosen, &mut on_line) } else { setup::run_local(&plan.local, chosen, &mut on_line) };
            report(Step::RunningSetup { label: label.clone(), last_line: String::new(), outcome: Some(ran.is_ok()) });
            if let Err(e) = ran {
                failures.push(format!("{label} failed ({e:#})"));
            }
        }
    }
    failures
}

fn copy_to_remote(remote: &Remote, local: &Path, files: &[String], report: &mut dyn FnMut(Step)) -> Result<()> {
    report(Step::CreatingRemoteFolder);
    remote.create_dir_all()?;
    report(Step::CopyingToRemote { files: files.len(), fraction: 0.0 });
    remote.push(local, files, &mut |fraction| report(Step::CopyingToRemote { files: files.len(), fraction }))
}

fn copy_extras(plan: &NewWorktree, remote: &Remote, report: &mut dyn FnMut(Step)) -> Result<()> {
    let on_local = plan.extras_on(true);
    if !on_local.is_empty() {
        report(Step::CopyingLocalExtras { entries: on_local.len() });
        copy_missing_locally(&plan.repo, &plan.local, &on_local)?;
    }
    let on_remote = plan.extras_on(false);
    if !on_remote.is_empty() {
        report(Step::CopyingRemoteExtras { entries: on_remote.len() });
        remote.copy_missing_from(&Remote::parse(&plan.source_remote)?, &on_remote)?;
    }
    Ok(())
}

fn copy_missing_locally(source: &Path, destination: &Path, paths: &[String]) -> Result<()> {
    for path in paths {
        let (from, to) = (source.join(path), destination.join(path));
        if to.exists() || !from.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let copied = std::process::Command::new("cp").arg("-a").arg("--").arg(&from).arg(&to).stdin(std::process::Stdio::null()).output().context("failed to run cp")?;
        if !copied.status.success() {
            bail!("copying {path}: {}", String::from_utf8_lossy(&copied.stderr).trim());
        }
    }
    Ok(())
}

fn prepare_mirror(local: &Path, remote: &Remote, workspaces_dir: &Path, files: &[String]) -> Result<()> {
    let local = local.canonicalize()?;
    let mirror = workspaces_dir.join(crate::engine::workspace_id(&local, remote)).join("mirror");
    std::fs::create_dir_all(&mirror)?;
    Remote::Local(mirror).push(&local, files, &mut |_| {})
}

pub struct Removal {
    pub main: PathBuf,
    pub command: String,
}

pub fn removal(local: &Path, remote: &Remote, delete_remote: bool) -> Result<Removal> {
    if !is_linked(local) {
        bail!("{} is not a linked git worktree", local.display());
    }
    if delete_remote && !remote_in_worktree_folder(remote) {
        bail!("{} is not in a {FOLDER_SUFFIX} folder, so it is not deleted", remote.display());
    }
    let main = main_worktree(local)?;
    let quoted = |path: &Path| crate::remote::shell_quote(&path.display().to_string());
    let git = format!("git -C {} worktree remove --force {}", quoted(&main), quoted(local));
    let command = if delete_remote { format!("{git} && {}", remote.remove_dir_line()) } else { git };
    Ok(Removal { main, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("agent-diff-worktree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn repo_with_a_commit(root: &Path) -> PathBuf {
        let repo = root.join("proj");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("src/a.txt"), "a").unwrap();
        std::fs::write(repo.join("untracked.txt"), "u").unwrap();
        git(&repo, &["add", "src/a.txt"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        repo
    }

    #[test]
    fn reads_the_branch_list() {
        let listed = "main\0origin/main\0*\0/w/proj\nfeat/x\0\0 \0\nold\0origin/old\0 \0/w/proj.wt/old\n";
        let branches = parse_branches(listed);
        assert_eq!(branches.len(), 3);
        assert!(branches[0].current && branches[0].checked_out_in.is_some());
        assert_eq!(branches[1], Branch { name: "feat/x".into(), upstream: None, current: false, checked_out_in: None });
        assert_eq!(branches[2].remote_and_name(), Some(("origin", "old")));
    }

    #[test]
    fn deletes_a_branch_locally_and_on_its_remote() {
        let root = scratch("branches");
        let repo = repo_with_a_commit(&root);
        git(&root, &["init", "-q", "--bare", "origin.git"]);
        git(&repo, &["remote", "add", "origin", root.join("origin.git").to_str().unwrap()]);
        git(&repo, &["branch", "gone"]);
        git(&repo, &["push", "-q", "-u", "origin", "gone"]);
        git(&repo, &["branch", "kept"]);
        let branches = local_branches(&repo);
        let main = branches.iter().find(|b| b.name == "main").unwrap();
        assert!(branch_deletion(&repo, main, false, false).is_err());
        let kept = branches.iter().find(|b| b.name == "kept").unwrap();
        assert!(branch_deletion(&repo, kept, false, true).is_err());
        let gone = branches.iter().find(|b| b.name == "gone").unwrap();
        let command = branch_deletion(&repo, gone, false, true).unwrap();
        assert!(std::process::Command::new("bash").arg("-c").arg(&command).status().unwrap().success());
        assert!(!local_branches(&repo).iter().any(|b| b.name == "gone"));
        assert_eq!(git(&root.join("origin.git"), &["branch", "--list", "gone"]).trim(), "");
    }

    #[test]
    fn reads_the_worktree_list() {
        let porcelain = "worktree /w/proj\nHEAD 1\nbranch refs/heads/main\n\nworktree /w/proj.wt/feat\nHEAD 2\nbranch refs/heads/feat/x\n\nworktree /w/proj.wt/old\nHEAD 3\ndetached\nprunable gitdir file points to non-existent location\n";
        let listed = parse_list(porcelain);
        assert_eq!(listed.len(), 3);
        assert!(listed[0].main && !listed[1].main);
        assert_eq!(listed[1].branch.as_deref(), Some("feat/x"));
        assert_eq!(listed[1].path, PathBuf::from("/w/proj.wt/feat"));
        assert!(listed[2].branch.is_none() && listed[2].missing);
    }

    #[test]
    fn derives_sibling_folders() {
        assert_eq!(folder_name("feat/x y"), "feat-x-y");
        assert_eq!(sibling_path("/home/me/proj/", "feat-x"), "/home/me/proj.wt/feat-x");
        assert_eq!(sibling_path("/home/me/proj.wt/feat-x", "fix-y"), "/home/me/proj.wt/fix-y");
        assert_eq!(sibling_remote("me@host:/srv/proj", "feat-x"), "me@host:/srv/proj.wt/feat-x");
        assert_eq!(sibling_remote("me@host:/srv/proj.wt/feat-x", "fix-y"), "me@host:/srv/proj.wt/fix-y");
        assert_eq!(sibling_remote("/mnt/proj", "feat-x"), "/mnt/proj.wt/feat-x");
    }

    #[test]
    fn only_folders_in_a_worktree_folder_can_be_deleted() {
        assert!(remote_in_worktree_folder(&Remote::parse("host:/srv/proj.wt/feat-x").unwrap()));
        assert!(!remote_in_worktree_folder(&Remote::parse("host:/srv/proj").unwrap()));
        assert!(!remote_in_worktree_folder(&Remote::parse("host:/srv/proj.wt/..").unwrap()));
        assert!(!remote_in_worktree_folder(&Remote::parse("/srv/proj.wt").unwrap()));
    }

    #[test]
    fn creates_a_worktree_with_its_remote_copy_then_removes_both() {
        let root = scratch("create");
        let repo = repo_with_a_commit(&root);
        let local = root.join("proj.wt/feat-x");
        let remote = root.join("remote.wt/feat-x");
        let plan = NewWorktree { repo: repo.clone(), branch: "feat/x".into(), base: String::new(), local: local.clone(), remote: remote.display().to_string(), source_remote: root.join("none").display().to_string(), extras: Vec::new(), setups: Vec::new(), workspaces_dir: root.join("workspaces") };
        let mut steps = Vec::new();
        create(&plan, &mut |step| steps.push(step)).unwrap();
        assert_eq!(steps.first(), Some(&Step::CheckingFolders));
        assert!(steps.contains(&Step::CopyingToRemote { files: 1, fraction: 1.0 }));
        assert_eq!(steps.last(), Some(&Step::PreparingMirror { files: 1 }));
        let mirror = root.join("workspaces").join(crate::engine::workspace_id(&local.canonicalize().unwrap(), &Remote::Local(remote.clone()))).join("mirror");
        assert_eq!(std::fs::read_to_string(mirror.join("src/a.txt")).unwrap(), "a");
        assert!(is_linked(&local));
        assert!(!is_linked(&repo));
        assert_eq!(git(&local, &["branch", "--show-current"]).trim(), "feat/x");
        assert_eq!(std::fs::read_to_string(remote.join("src/a.txt")).unwrap(), "a");
        assert!(!remote.join("untracked.txt").exists());
        assert!(!remote.join(".git").exists());
        assert_eq!(main_worktree(&local).unwrap().canonicalize().unwrap(), repo.canonicalize().unwrap());
        assert!(branches(&repo).contains(&"feat/x".to_string()));

        assert!(removal(&local, &Remote::Local(root.join("outside")), true).is_err());
        let removing = removal(&local, &Remote::Local(remote.clone()), true).unwrap();
        assert_eq!(removing.main.canonicalize().unwrap(), repo.canonicalize().unwrap());
        assert!(std::process::Command::new("bash").arg("-c").arg(&removing.command).status().unwrap().success());
        assert!(!local.exists());
        assert!(!remote.exists());
        assert!(git(&repo, &["branch", "--list", "feat/x"]).contains("feat/x"));
    }

    #[test]
    fn copies_the_chosen_gitignored_entries_from_each_side() {
        let root = scratch("extras");
        let repo = repo_with_a_commit(&root);
        std::fs::write(repo.join(".gitignore"), ".env\nbuild/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "ignore"]);
        std::fs::write(repo.join(".env"), "local secret").unwrap();
        std::fs::create_dir_all(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/out"), "o").unwrap();
        let source_remote = root.join("remote");
        std::fs::create_dir_all(&source_remote).unwrap();
        std::fs::write(source_remote.join(".env"), "remote secret").unwrap();

        let extras = gitignored_extras(&repo, &Remote::Local(source_remote.clone()));
        assert!(extras.contains(&Extra { path: ".env".into(), folder: false, local: true, remote: false }));
        assert!(extras.contains(&Extra { path: "build".into(), folder: true, local: true, remote: false }));

        let env = Extra { path: ".env".into(), folder: false, local: true, remote: true };
        let (local, remote) = (root.join("proj.wt/feat"), root.join("remote.wt/feat"));
        let plan = NewWorktree {
            repo: repo.clone(),
            branch: "feat".into(),
            base: String::new(),
            local: local.clone(),
            remote: remote.display().to_string(),
            source_remote: source_remote.display().to_string(),
            extras: vec![env],
            setups: vec![Setup { dir: String::new(), reason: String::new(), command: "touch set-up".into(), local: true, remote: true }],
            workspaces_dir: root.join("workspaces"),
        };
        assert!(create(&plan, &mut |_| {}).unwrap().is_empty());
        assert!(local.join("set-up").exists() && remote.join("set-up").exists());
        assert_eq!(std::fs::read_to_string(local.join(".env")).unwrap(), "local secret");
        assert_eq!(std::fs::read_to_string(remote.join(".env")).unwrap(), "remote secret");
        assert!(!local.join("build").exists());
        assert_eq!(std::fs::read_to_string(local.join(".gitignore")).unwrap(), ".env\nbuild/\n");
    }

    #[test]
    fn checks_out_an_existing_branch_and_refuses_a_taken_folder() {
        let root = scratch("existing");
        let repo = repo_with_a_commit(&root);
        git(&repo, &["branch", "old"]);
        let local = root.join("proj.wt/old");
        let plan = NewWorktree { repo: repo.clone(), branch: "old".into(), base: String::new(), local: local.clone(), remote: root.join("r.wt/old").display().to_string(), source_remote: root.join("none").display().to_string(), extras: Vec::new(), setups: Vec::new(), workspaces_dir: root.join("workspaces") };
        create(&plan, &mut |_| {}).unwrap();
        assert_eq!(git(&local, &["branch", "--show-current"]).trim(), "old");
        assert!(create(&plan, &mut |_| {}).is_err());
    }
}
