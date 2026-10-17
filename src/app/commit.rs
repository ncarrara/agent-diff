use super::*;
use crate::engine::RepoChanges;
use crate::remote::shell_quote;

pub(super) struct CommitDialog {
    changes: Result<Vec<RepoChanges>, String>,
    chosen: Vec<bool>,
    message: String,
}

impl CommitDialog {
    pub(super) fn open(local_root: &str, skipped: &[String]) -> Self {
        let changes = crate::engine::uncommitted(Path::new(local_root)).map_err(|e| format!("{e:#}"));
        let chosen = changes.as_ref().map(|c| c.iter().map(|c| !skipped.contains(&c.repo.path)).collect()).unwrap_or_default();
        CommitDialog { changes, chosen, message: String::new() }
    }

    pub(super) fn remember_unticked(&self, skipped: &mut Vec<String>) {
        let Ok(changes) = &self.changes else { return };
        for (change, on) in changes.iter().zip(&self.chosen) {
            let repo = &change.repo.path;
            match (*on, skipped.contains(repo)) {
                (false, false) => skipped.push(repo.clone()),
                (true, true) => skipped.retain(|r| r != repo),
                _ => {}
            }
        }
    }

    fn chosen_repos(&self) -> Vec<String> {
        let Ok(changes) = &self.changes else { return Vec::new() };
        changes.iter().zip(&self.chosen).filter(|(_, on)| **on).map(|(c, _)| c.repo.path.clone()).collect()
    }

    fn ready(&self) -> bool {
        !self.message.trim().is_empty() && !self.chosen_repos().is_empty()
    }

    pub(super) fn run_in_terminal(&self, state_dir: &str, local_root: &str, push: bool) -> std::io::Result<String> {
        let message_file = Path::new(state_dir).join("commit-message");
        std::fs::write(&message_file, format!("{}\n", self.message.trim()))?;
        Ok(commit_command(&self.chosen_repos(), local_root, &message_file, push))
    }
}

const PUSH_SETTING_UPSTREAM: &str = "{ git rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1 && git push || git push --set-upstream origin HEAD; }";

fn commit_command(repos: &[String], local_root: &str, message_file: &Path, push: bool) -> String {
    let mut repos = repos.to_vec();
    crate::engine::deepest_first(&mut repos);
    let message = shell_quote(&message_file.display().to_string());
    repos
        .iter()
        .map(|repo| {
            let dir = shell_quote(&Path::new(local_root).join(repo).display().to_string());
            let commit = format!("cd {dir} && git commit -a -F {message}");
            if push { format!("{commit} && {PUSH_SETTING_UPSTREAM}") } else { commit }
        })
        .collect::<Vec<_>>()
        .join(" && ")
}

pub(super) enum CommitOutcome {
    Open,
    Cancel,
    Commit { push: bool },
}

pub(super) fn commit_dialog(ui: &egui::Ui, dialog: &mut CommitDialog, local_root: &str) -> CommitOutcome {
    let mut outcome = CommitOutcome::Open;
    let modal = egui::Modal::new(egui::Id::new(("commit", local_root))).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.heading("Commit local changes");
        ui.add_space(6.0);
        match &dialog.changes {
            Err(e) => {
                ui.colored_label(Color32::LIGHT_RED, e);
            }
            Ok(changes) if changes.is_empty() => {
                ui.label("Nothing to commit: every repository is clean.");
            }
            Ok(changes) => repo_list(ui, changes, &mut dialog.chosen),
        }
        ui.add_space(8.0);
        ui.label("Message");
        let edit = ui.add(egui::TextEdit::multiline(&mut dialog.message).desired_rows(3).desired_width(f32::INFINITY).hint_text("What changed and why"));
        if !edit.has_focus() && !edit.lost_focus() && dialog.message.is_empty() {
            edit.request_focus();
        }
        let shortcut = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.add_space(4.0);
        ui.weak("git commit -a in each checked repository, typed into a local shell tab so the pre-commit hooks show their output there: your modified and deleted tracked files, and files already added. Changes still only on the remote are not included: pull them first.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let button = |text: &str, fill| egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(fill);
            let commit = ui.add_enabled(dialog.ready(), button("✔ Commit", Color32::from_rgb(0x2e, 0x8b, 0x46))).on_hover_text("Ctrl+Enter");
            if (commit.clicked() || shortcut) && dialog.ready() {
                outcome = CommitOutcome::Commit { push: false };
            }
            let push_help = "Commit, then git push each committed repository (setting its upstream on origin if it has none)";
            if ui.add_enabled(dialog.ready(), button("⬆ Commit & push", Color32::from_rgb(0x4f, 0x6f, 0xb0))).on_hover_text(push_help).clicked() {
                outcome = CommitOutcome::Commit { push: true };
            }
            if ui.button("Cancel").clicked() {
                outcome = CommitOutcome::Cancel;
            }
            if ui.button("⟳").on_hover_text("Look for changes again").clicked() {
                let mut unticked = Vec::new();
                dialog.remember_unticked(&mut unticked);
                *dialog = CommitDialog { message: std::mem::take(&mut dialog.message), ..CommitDialog::open(local_root, &unticked) };
            }
        });
    });
    if matches!(outcome, CommitOutcome::Open) && modal.should_close() {
        outcome = CommitOutcome::Cancel;
    }
    outcome
}

fn repo_list(ui: &mut egui::Ui, changes: &[RepoChanges], chosen: &mut [bool]) {
    egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
        for (change, on) in changes.iter().zip(chosen.iter_mut()) {
            let name = if change.repo.path.is_empty() { "(workspace root)" } else { &change.repo.path };
            let branch = change.repo.branch.as_deref().map(|b| format!("  🌿 {b}")).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.checkbox(on, RichText::new(format!("📦 {name}")).color(REPO).strong());
                ui.weak(format!("{branch}  · {} file(s)", change.files.len()));
            });
            egui::CollapsingHeader::new("files").id_salt(("commit-files", name)).show(ui, |ui| {
                for (code, path) in &change.files {
                    ui.horizontal(|ui| {
                        ui.monospace(RichText::new(format!("{code:>2}")).color(status_color(code)));
                        ui.monospace(path);
                    });
                }
            });
        }
    });
}

fn status_color(code: &str) -> Color32 {
    match code.chars().find(|c| *c != ' ') {
        Some('A') => change_color(Some(Change::Added)),
        Some('D') => change_color(Some(Change::Deleted)),
        Some('R') => change_color(Some(Change::Renamed)),
        _ => change_color(Some(Change::Modified)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(repos: &[&str], chosen: &[bool]) -> CommitDialog {
        let changes = repos
            .iter()
            .map(|r| RepoChanges { repo: Repo { path: r.to_string(), branch: None }, files: vec![("M".into(), format!("{r}/a"))] })
            .collect();
        CommitDialog { changes: Ok(changes), chosen: chosen.to_vec(), message: String::new() }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn the_typed_command_commits_nested_repositories_first_and_pushes() {
        let root = std::env::temp_dir().join(format!("agent-diff-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let local = root.join("my 'work'");
        for repo in ["", "lib"] {
            let dir = local.join(repo);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            git(&dir, &["config", "user.name", "t"]);
            git(&dir, &["config", "user.email", "t@t"]);
            std::fs::write(dir.join("a.txt"), "1\n").unwrap();
            git(&dir, &["add", "a.txt"]);
            git(&dir, &["commit", "-qm", "init"]);
            std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        }
        std::fs::write(local.join(".gitignore"), "lib/\n").unwrap();
        git(&root, &["init", "-q", "--bare", "origin.git"]);
        git(&local.join("lib"), &["remote", "add", "origin", root.join("origin.git").to_str().unwrap()]);
        let message = root.join("message");
        std::fs::write(&message, "lib's work\n\nwith a body\n").unwrap();

        let repos = vec![String::new(), "lib".to_string()];
        let command = commit_command(&repos, &local.display().to_string(), &message, true);
        assert!(command.find("/lib'").unwrap() < command.rfind("git commit").unwrap());
        let lib_only = commit_command(&repos[1..], &local.display().to_string(), &message, true);
        let ran = std::process::Command::new("bash").arg("-c").arg(&lib_only).output().unwrap();
        assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(git(&root.join("origin.git"), &["log", "-1", "--format=%B", "main"]).trim(), "lib's work\n\nwith a body");
        std::fs::write(local.join("lib/a.txt"), "3\n").unwrap();
        assert!(std::process::Command::new("bash").arg("-c").arg(&lib_only).status().unwrap().success());
        assert_eq!(git(&root.join("origin.git"), &["rev-list", "--count", "main"]).trim(), "3");
        let root_without_origin = commit_command(&repos[..1], &local.display().to_string(), &message, true);
        assert!(!std::process::Command::new("bash").arg("-c").arg(&root_without_origin).status().unwrap().success());
    }

    #[test]
    fn unticked_repositories_are_remembered() {
        let mut skipped = vec!["old".to_string(), "web".to_string()];
        dialog(&["", "api", "web"], &[false, true, true]).remember_unticked(&mut skipped);
        assert_eq!(skipped, ["old", ""]);
        let reopened = dialog(&["", "api"], &[true, true]);
        let chosen: Vec<bool> = ["", "api"].iter().map(|r| !skipped.contains(&r.to_string())).collect();
        assert_eq!(chosen, [false, true]);
        assert_eq!(reopened.chosen_repos(), ["", "api"]);
    }

    #[test]
    fn skipped_list_is_saved_and_read_back() {
        let repos = vec![String::new(), "a, b".to_string(), "libs/x".to_string()];
        let text = repo_list_to_string(&repos);
        assert!(!text.contains('\t'));
        assert_eq!(parse_repo_list(&text), repos);
        assert!(parse_repo_list("").is_empty());
    }
}
