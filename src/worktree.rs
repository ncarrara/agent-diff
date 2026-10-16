use crate::engine::git_at;
use crate::remote::Remote;
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

pub struct NewWorktree {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    pub local: PathBuf,
    pub remote: String,
}

pub fn create(plan: &NewWorktree) -> Result<()> {
    let remote = Remote::parse(&plan.remote)?;
    if plan.local.exists() {
        bail!("{} already exists", plan.local.display());
    }
    if remote.exists()? {
        bail!("{} already exists on the remote", remote.display());
    }
    add(&plan.repo, &plan.local, &plan.branch, &plan.base)?;
    let seeded = remote.create_dir_all().and_then(|()| remote.push(&plan.local, &tracked_files(&plan.local)?, &mut |_| {}));
    if let Err(e) = seeded {
        let _ = git_at(&plan.repo, &["worktree", "remove", "--force", &plan.local.display().to_string()]);
        let _ = remote.remove_dir_all();
        return Err(e.context("could not copy the worktree to the remote"));
    }
    Ok(())
}

pub fn remove(local: &Path, remote: &Remote, delete_remote: bool) -> Result<()> {
    if !is_linked(local) {
        bail!("{} is not a linked git worktree", local.display());
    }
    if delete_remote && !remote_in_worktree_folder(remote) {
        bail!("{} is not in a {FOLDER_SUFFIX} folder, so it is not deleted", remote.display());
    }
    let main = main_worktree(local)?;
    git_at(&main, &["worktree", "remove", "--force", &local.display().to_string()])?;
    if delete_remote {
        remote.remove_dir_all()?;
    }
    Ok(())
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
        let plan = NewWorktree { repo: repo.clone(), branch: "feat/x".into(), base: String::new(), local: local.clone(), remote: remote.display().to_string() };
        create(&plan).unwrap();
        assert!(is_linked(&local));
        assert!(!is_linked(&repo));
        assert_eq!(git(&local, &["branch", "--show-current"]).trim(), "feat/x");
        assert_eq!(std::fs::read_to_string(remote.join("src/a.txt")).unwrap(), "a");
        assert!(!remote.join("untracked.txt").exists());
        assert!(!remote.join(".git").exists());
        assert_eq!(main_worktree(&local).unwrap().canonicalize().unwrap(), repo.canonicalize().unwrap());
        assert!(branches(&repo).contains(&"feat/x".to_string()));

        remove(&local, &Remote::Local(remote.clone()), true).unwrap();
        assert!(!local.exists());
        assert!(!remote.exists());
        assert!(git(&repo, &["branch", "--list", "feat/x"]).contains("feat/x"));
    }

    #[test]
    fn checks_out_an_existing_branch_and_refuses_a_taken_folder() {
        let root = scratch("existing");
        let repo = repo_with_a_commit(&root);
        git(&repo, &["branch", "old"]);
        let local = root.join("proj.wt/old");
        let plan = NewWorktree { repo: repo.clone(), branch: "old".into(), base: String::new(), local: local.clone(), remote: root.join("r.wt/old").display().to_string() };
        create(&plan).unwrap();
        assert_eq!(git(&local, &["branch", "--show-current"]).trim(), "old");
        assert!(create(&plan).is_err());
    }
}
