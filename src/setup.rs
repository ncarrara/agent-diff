use crate::remote::{Remote, shell_quote};
use crate::worktree::Extra;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub dir: String,
    pub reason: String,
    pub command: String,
    pub local: bool,
    pub remote: bool,
}

const NODE_LOCKS: [(&str, &str); 5] = [
    ("pnpm-lock.yaml", "pnpm install --frozen-lockfile"),
    ("yarn.lock", "yarn install --frozen-lockfile"),
    ("bun.lock", "bun install --frozen-lockfile"),
    ("bun.lockb", "bun install --frozen-lockfile"),
    ("package-lock.json", "npm ci"),
];

const PYTHON_LOCKS: [(&str, &str, Option<&str>); 3] = [("uv.lock", "uv sync", Some(".venv")), ("poetry.lock", "poetry install", Some(".venv")), ("Pipfile.lock", "pipenv sync --dev", None)];

const OTHER_LOCKS: [(&str, &str, Option<&str>); 3] = [("Cargo.lock", "cargo build", Some("target")), ("Gemfile.lock", "bundle install", None), ("go.sum", "go mod download", None)];

const VENV_FROM_REQUIREMENTS: &str = "python3 -m venv .venv && .venv/bin/pip install -r requirements.txt";

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

fn files_by_folder(tracked: &[String]) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut folders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for path in tracked {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        folders.entry(dir).or_default().insert(name);
    }
    folders
}

fn inside_any(dir: &str, roots: &[&str]) -> bool {
    roots.iter().any(|root| if root.is_empty() { !dir.is_empty() } else { dir.starts_with(&format!("{root}/")) })
}

fn environment<'a>(extras: &'a [Extra], dir: &str, folder: &str) -> Option<&'a Extra> {
    let path = join(dir, folder);
    extras.iter().find(|extra| extra.path == path)
}

fn setup_for(dir: &str, file: &str, command: &str, env: Option<&str>, extras: &[Extra]) -> Setup {
    let found = env.and_then(|env| environment(extras, dir, env));
    Setup { dir: dir.to_string(), reason: join(dir, file), command: command.to_string(), local: found.is_some_and(|e| e.local), remote: found.is_some_and(|e| e.remote) }
}

pub fn discover(tracked: &[String], extras: &[Extra]) -> Vec<Setup> {
    let folders = files_by_folder(tracked);
    let node_roots: Vec<&str> = folders.iter().filter(|(_, names)| NODE_LOCKS.iter().any(|(lock, _)| names.contains(lock))).map(|(dir, _)| *dir).collect();
    let mut setups = Vec::new();
    for (dir, names) in &folders {
        let has = |name: &str| names.contains(name);
        let node = NODE_LOCKS.iter().find(|(lock, _)| has(lock)).copied();
        let node = node.or_else(|| (has("package.json") && !inside_any(dir, &node_roots)).then_some(("package.json", "npm install")));
        if let Some((file, command)) = node {
            setups.push(setup_for(dir, file, command, Some("node_modules"), extras));
        }
        let python = PYTHON_LOCKS.iter().find(|(lock, ..)| has(lock)).copied();
        let python = python.or_else(|| (has("requirements.txt") && environment(extras, dir, ".venv").is_some()).then_some(("requirements.txt", VENV_FROM_REQUIREMENTS, Some(".venv"))));
        if let Some((file, command, env)) = python {
            setups.push(setup_for(dir, file, command, env, extras));
        }
        for (lock, command, env) in OTHER_LOCKS.iter().filter(|(lock, ..)| has(lock)) {
            setups.push(setup_for(dir, lock, command, *env, extras));
        }
    }
    setups
}

pub fn label(setup: &Setup, on_remote: bool) -> String {
    let side = if on_remote { "remote" } else { "local" };
    let dir = if setup.dir.is_empty() { "." } else { setup.dir.as_str() };
    format!("{side}: {dir} → {}", setup.command)
}

fn script(root: &str, setup: &Setup) -> String {
    let folder = shell_quote(&join(root, &setup.dir));
    format!("cd {folder} && \"${{SHELL:-/bin/sh}}\" -lic {} 2>&1 </dev/null", shell_quote(&setup.command))
}

pub fn run_local(root: &Path, setup: &Setup, on_line: &mut dyn FnMut(&str)) -> Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script(&root.display().to_string(), setup));
    run_streaming(without_terminal(cmd), on_line)
}

fn without_terminal(mut cmd: Command) -> Command {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd
}

pub fn run_remote(remote: &Remote, setup: &Setup, on_line: &mut dyn FnMut(&str)) -> Result<()> {
    run_streaming(remote.command(&script(&remote.root_path(), setup)), on_line)
}

const KEPT_LINES: usize = 6;

fn run_streaming(mut cmd: Command, on_line: &mut dyn FnMut(&str)) -> Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("failed to start the command")?;
    let mut last = std::collections::VecDeque::new();
    for line in BufReader::new(child.stdout.take().expect("piped stdout")).lines().map_while(std::io::Result::ok) {
        let line = line.rsplit('\r').next().unwrap_or("").trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        on_line(&line);
        last.push_back(line);
        if last.len() > KEPT_LINES {
            last.pop_front();
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{status}: {}", last.into_iter().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    fn folder(path: &str, local: bool, remote: bool) -> Extra {
        Extra { path: path.into(), folder: true, local, remote }
    }

    #[test]
    fn discovers_one_command_per_tool_and_folder() {
        let tracked = paths(&["v2/package.json", "v2/package-lock.json", "v2/packages/ui/package.json", "game/servers/pyproject.toml", "game/servers/uv.lock", "tools/package.json", "Cargo.lock", "scripts/requirements.txt"]);
        let extras = [folder("v2/node_modules", true, true), folder("game/servers/.venv", true, false), folder("target", true, false)];
        let found = discover(&tracked, &extras);
        let commands: Vec<(&str, &str)> = found.iter().map(|s| (s.dir.as_str(), s.command.as_str())).collect();
        assert_eq!(commands, [("", "cargo build"), ("game/servers", "uv sync"), ("tools", "npm install"), ("v2", "npm ci")]);
        assert!(found[0].local && !found[0].remote);
        let npm = found.iter().find(|s| s.dir == "v2").unwrap();
        assert!(npm.local && npm.remote);
        assert_eq!(npm.reason, "v2/package-lock.json");
        let uv = found.iter().find(|s| s.dir == "game/servers").unwrap();
        assert!(uv.local && !uv.remote);
        assert!(!found.iter().find(|s| s.dir == "tools").unwrap().local);
    }

    #[test]
    fn a_requirements_file_counts_only_with_a_virtualenv_next_to_it() {
        let tracked = paths(&["requirements.txt", "go.sum"]);
        let found = discover(&tracked, &[folder(".venv", false, true)]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].command, VENV_FROM_REQUIREMENTS);
        assert!(!found[0].local && found[0].remote);
        assert_eq!(found[1].command, "go mod download");
        assert!(discover(&paths(&["requirements.txt"]), &[]).is_empty());
    }

    #[test]
    fn runs_in_the_folder_and_reports_its_output() {
        let root = std::env::temp_dir().join(format!("agent-diff-setup-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        let setup = Setup { dir: "app".into(), reason: String::new(), command: "pwd; echo done".into(), local: true, remote: false };
        let mut lines = Vec::new();
        run_local(&root, &setup, &mut |line| lines.push(line.to_string())).unwrap();
        assert!(lines.iter().any(|l| l.ends_with("/app")));
        assert!(lines.iter().any(|l| l == "done"));
        let failing = Setup { command: "echo broken; exit 3".into(), ..setup };
        let error = run_local(&root, &failing, &mut |_| {}).unwrap_err().to_string();
        assert!(error.contains("broken"), "{error}");
    }
}
