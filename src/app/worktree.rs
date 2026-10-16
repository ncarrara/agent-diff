use super::*;
use crate::worktree::{self as wt, NewWorktree};

pub(super) enum WorktreeDialog {
    New(NewForm),
    Remove(RemoveForm),
}

pub(super) struct NewForm {
    branch: String,
    base: String,
    local: String,
    remote: String,
    paths_edited: bool,
    branches: Vec<String>,
    error: Option<String>,
    job: Option<Receiver<Result<(), String>>>,
}

pub(super) struct RemoveForm {
    uncommitted: Vec<String>,
    delete_remote: bool,
    can_delete_remote: bool,
}

pub(super) enum WorktreeOutcome {
    Open,
    Closed,
    Created(Workspace),
    Remove { delete_remote: bool },
}

impl WorktreeDialog {
    pub(super) fn new_worktree(ws: &Workspace) -> Self {
        let mut form = NewForm {
            branch: String::new(),
            base: String::new(),
            local: String::new(),
            remote: String::new(),
            paths_edited: false,
            branches: wt::branches(Path::new(&ws.local)),
            error: None,
            job: None,
        };
        form.follow_branch(ws);
        WorktreeDialog::New(form)
    }

    pub(super) fn remove_worktree(ws: &Workspace) -> Self {
        let can_delete_remote = Remote::parse(&ws.remote).is_ok_and(|r| wt::remote_in_worktree_folder(&r));
        WorktreeDialog::Remove(RemoveForm { uncommitted: wt::uncommitted(Path::new(&ws.local)), delete_remote: can_delete_remote, can_delete_remote })
    }
}

impl NewForm {
    fn follow_branch(&mut self, ws: &Workspace) {
        if self.paths_edited {
            return;
        }
        let folder = wt::folder_name(&self.branch);
        let folder = if folder.is_empty() { "<branch>".to_string() } else { folder };
        self.local = wt::sibling_path(&ws.local, &folder);
        self.remote = wt::sibling_remote(&ws.remote, &folder);
    }

    fn start(&mut self, ws: &Workspace, ctx: &egui::Context) {
        let plan = NewWorktree {
            repo: PathBuf::from(&ws.local),
            branch: self.branch.trim().to_string(),
            base: self.base.trim().to_string(),
            local: PathBuf::from(self.local.trim()),
            remote: self.remote.trim().to_string(),
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(wt::create(&plan).map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.error = None;
        self.job = Some(rx);
    }

    fn finished(&mut self, ws: &Workspace) -> Option<Workspace> {
        let result = self.job.as_ref()?.try_recv().ok()?;
        self.job = None;
        match result {
            Ok(()) => {
                let local = PathBuf::from(self.local.trim());
                let local = local.canonicalize().unwrap_or(local).display().to_string();
                let sessions = vec![TermTab::new(TermKind::Claude)];
                Some(Workspace { local, remote: self.remote.trim().to_string(), interval: ws.interval, sessions, forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() })
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
}

pub(super) fn worktree_dialog_ui(ctx: &egui::Context, dialog: &mut WorktreeDialog, ws: &Workspace, pending: usize, unsaved: Option<&str>) -> WorktreeOutcome {
    match dialog {
        WorktreeDialog::New(form) => new_worktree_ui(ctx, form, ws),
        WorktreeDialog::Remove(form) => remove_worktree_ui(ctx, form, ws, pending, unsaved),
    }
}

fn new_worktree_ui(ctx: &egui::Context, form: &mut NewForm, ws: &Workspace) -> WorktreeOutcome {
    if let Some(created) = form.finished(ws) {
        return WorktreeOutcome::Created(created);
    }
    let running = form.job.is_some();
    let (mut create, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new("new-worktree")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("🌿 New worktree");
        ui.add_space(4.0);
        ui.label("A git worktree of this repository on its own branch, copied to its own remote folder, opened in a new workspace tab with its own Claude.");
        ui.add_space(8.0);
        ui.add_enabled_ui(!running, |ui| new_worktree_fields(ui, form, ws));
        ui.add_space(6.0);
        ui.weak("An existing branch (local, or a single remote one) is checked out; a new one starts from the base. The remote folder gets the tracked files, without .git.");
        if let Some(e) = &form.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !running && !form.branch.trim().is_empty() && !form.local.trim().is_empty() && !form.remote.trim().is_empty();
            create = ui.add_enabled(ready, egui::Button::new("🌿 Create")).clicked();
            cancel = ui.add_enabled(!running, egui::Button::new("Cancel")).clicked();
            if running {
                ui.spinner();
                ui.label("creating the worktree and copying it to the remote…");
            }
        });
    });
    if create {
        form.start(ws, ctx);
    }
    if !running && (cancel || modal.should_close()) {
        return WorktreeOutcome::Closed;
    }
    WorktreeOutcome::Open
}

fn new_worktree_fields(ui: &mut egui::Ui, form: &mut NewForm, ws: &Workspace) {
    egui::Grid::new("new-worktree-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Branch");
        ui.horizontal(|ui| {
            let typed = ui.add(egui::TextEdit::singleline(&mut form.branch).hint_text("feat/my-idea").desired_width(300.0)).changed();
            let picked = branch_picker(ui, form);
            if typed || picked {
                form.follow_branch(ws);
            }
        });
        ui.end_row();
        ui.label("Base");
        ui.add(egui::TextEdit::singleline(&mut form.base).hint_text("HEAD (for a new branch)").desired_width(300.0));
        ui.end_row();
        ui.label("Local folder");
        form.paths_edited |= ui.add(egui::TextEdit::singleline(&mut form.local).desired_width(420.0)).changed();
        ui.end_row();
        ui.label("Remote folder");
        form.paths_edited |= ui.add(egui::TextEdit::singleline(&mut form.remote).desired_width(420.0)).changed();
        ui.end_row();
    });
}

fn branch_picker(ui: &mut egui::Ui, form: &mut NewForm) -> bool {
    let mut picked = None;
    egui::ComboBox::from_id_salt("worktree-branches").selected_text("existing…").height(320.0).show_ui(ui, |ui| {
        for branch in &form.branches {
            if ui.selectable_label(false, branch).clicked() {
                picked = Some(local_branch_name(branch));
            }
        }
    });
    let Some(branch) = picked else { return false };
    form.branch = branch;
    true
}

fn local_branch_name(listed: &str) -> String {
    match listed.split_once('/') {
        Some((remote, rest)) if remote == "origin" => rest.to_string(),
        _ => listed.to_string(),
    }
}

fn remove_worktree_ui(ctx: &egui::Context, form: &mut RemoveForm, ws: &Workspace, pending: usize, unsaved: Option<&str>) -> WorktreeOutcome {
    let (mut remove, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new("remove-worktree")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading(format!("Remove the worktree {}?", ws.title()));
        ui.add_space(6.0);
        ui.label(format!("Its tab is closed (terminals ended, sync data deleted) and the local worktree {} is removed with git worktree remove. Its branch and commits are kept.", ws.local));
        removal_warnings(ui, form, pending, unsaved);
        ui.add_space(6.0);
        let label = format!("Also delete the remote folder {}", ws.remote);
        ui.add_enabled(form.can_delete_remote, egui::Checkbox::new(&mut form.delete_remote, label))
            .on_disabled_hover_text(format!("Only a remote folder inside a {} folder is deleted from here", wt::FOLDER_SUFFIX));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            remove = ui.button(RichText::new("🗑 Remove worktree").color(Color32::LIGHT_RED)).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if remove {
        return WorktreeOutcome::Remove { delete_remote: form.delete_remote };
    }
    if cancel || modal.should_close() {
        return WorktreeOutcome::Closed;
    }
    WorktreeOutcome::Open
}

const SHOWN_UNCOMMITTED: usize = 8;

fn removal_warnings(ui: &mut egui::Ui, form: &RemoveForm, pending: usize, unsaved: Option<&str>) {
    if !form.uncommitted.is_empty() {
        ui.add_space(6.0);
        ui.colored_label(Color32::LIGHT_RED, format!("{} uncommitted change(s) in the worktree are lost:", form.uncommitted.len()));
        for line in form.uncommitted.iter().take(SHOWN_UNCOMMITTED) {
            ui.monospace(line);
        }
        if form.uncommitted.len() > SHOWN_UNCOMMITTED {
            ui.weak(format!("and {} more", form.uncommitted.len() - SHOWN_UNCOMMITTED));
        }
    }
    if pending > 0 && form.delete_remote {
        ui.colored_label(Color32::LIGHT_RED, format!("{pending} file(s) still differ between local and remote: what Claude changed and you did not pull is lost with the remote folder."));
    }
    if let Some(path) = unsaved {
        ui.colored_label(Color32::LIGHT_RED, format!("Your unsaved changes to {path} are lost."));
    }
}

pub(super) fn worktree_menu(ui: &mut egui::Ui, tab: &mut Tab) {
    ui.menu_button("🌿", |ui| {
        if ui.button("🌿 New worktree…").on_hover_text("Work on another branch in parallel: a git worktree, its own remote folder and its own Claude, in a new tab").clicked() {
            tab.worktree = Some(WorktreeDialog::new_worktree(&tab.ws));
            ui.close();
        }
        let remove = ui.add_enabled(tab.linked_worktree, egui::Button::new("🗑 Remove this worktree…"));
        if remove.on_disabled_hover_text("This workspace is not a linked git worktree").clicked() {
            tab.worktree = Some(WorktreeDialog::remove_worktree(&tab.ws));
            ui.close();
        }
    })
    .response
    .on_hover_text("Git worktrees: branches worked on in parallel, each in its own tab");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_local_name_of_an_origin_branch() {
        assert_eq!(local_branch_name("origin/feat/x"), "feat/x");
        assert_eq!(local_branch_name("feat/x"), "feat/x");
        assert_eq!(local_branch_name("upstream/feat"), "upstream/feat");
    }
}
