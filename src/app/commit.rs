use super::*;
use crate::engine::RepoChanges;

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

    fn command(&self, push: bool) -> Cmd {
        Cmd::Commit { repos: self.chosen_repos(), message: self.message.trim().to_string(), push }
    }
}

pub(super) enum CommitOutcome {
    Open,
    Cancel,
    Commit(Cmd),
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
        ui.weak("git commit -a in each checked repository: your modified and deleted tracked files, and files already added. Changes still only on the remote are not included: pull them first.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let button = |text: &str, fill| egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(fill);
            let commit = ui.add_enabled(dialog.ready(), button("✔ Commit", Color32::from_rgb(0x2e, 0x8b, 0x46))).on_hover_text("Ctrl+Enter");
            if (commit.clicked() || shortcut) && dialog.ready() {
                outcome = CommitOutcome::Commit(dialog.command(false));
            }
            let push_help = "Commit, then git push each committed repository (setting its upstream on origin if it has none)";
            if ui.add_enabled(dialog.ready(), button("⬆ Commit & push", Color32::from_rgb(0x4f, 0x6f, 0xb0))).on_hover_text(push_help).clicked() {
                outcome = CommitOutcome::Commit(dialog.command(true));
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
