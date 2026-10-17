use super::*;
use crate::setup::Setup;
use crate::worktree::{self as wt, Extra, NewWorktree, Step};

pub(super) enum WorktreeDialog {
    New(NewForm),
    Remove(RemoveForm),
    DeleteBranch(BranchForm),
}

pub(super) struct BranchForm {
    repo: PathBuf,
    branch: wt::Branch,
    force: bool,
    on_remote: bool,
    error: Option<String>,
}

impl BranchForm {
    pub(super) fn new(repo: PathBuf, branch: wt::Branch) -> Self {
        BranchForm { repo, branch, force: false, on_remote: false, error: None }
    }
}

pub(super) struct NewForm {
    branch: String,
    base: String,
    local: String,
    remote: String,
    paths_edited: bool,
    branches: Vec<String>,
    extras: Vec<(Extra, bool)>,
    setups: Vec<Setup>,
    listing: Option<Receiver<Discovered>>,
}

pub(super) struct Creating {
    plan: NewWorktree,
    steps: Vec<Step>,
    events: Receiver<Creation>,
    result: Option<Result<Vec<String>, String>>,
}

enum Creation {
    Step(Step),
    Finished(Result<Vec<String>, String>),
}

struct Discovered {
    extras: Vec<Extra>,
    setups: Vec<Setup>,
}

impl Creating {
    fn record(&mut self, step: Step) {
        match self.steps.last_mut() {
            Some(last) if last.same_kind(&step) => *last = step,
            _ => self.steps.push(step),
        }
    }

    fn receive(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Creation::Step(step) => self.record(step),
                Creation::Finished(done) => self.result = Some(done),
            }
        }
    }

    pub(super) fn running(&self) -> bool {
        self.result.is_none()
    }

    pub(super) fn branch(&self) -> &str {
        &self.plan.branch
    }
}

pub(super) struct RemoveForm {
    pub(super) local: String,
    remote: String,
    in_a_tab: bool,
    uncommitted: Vec<String>,
    delete_remote: bool,
    can_delete_remote: bool,
}

pub(super) enum WorktreeOutcome {
    Open,
    Closed,
    Started { ws: Workspace, job: Creating },
    Remove { local: String, remote: String, delete_remote: bool },
    RunInTerminal(String),
}

impl WorktreeDialog {
    pub(super) fn new_worktree(ws: &Workspace, ctx: &egui::Context) -> Self {
        let mut form = NewForm {
            branch: String::new(),
            base: String::new(),
            local: String::new(),
            remote: String::new(),
            paths_edited: false,
            branches: wt::branches(Path::new(&ws.local)),
            extras: Vec::new(),
            setups: Vec::new(),
            listing: Some(discover(ws, ctx)),
        };
        form.follow_branch(ws);
        WorktreeDialog::New(form)
    }

    pub(super) fn remove_worktree(local: &str, remote: &str, in_a_tab: bool) -> Self {
        let can_delete_remote = Remote::parse(remote).is_ok_and(|r| wt::remote_in_worktree_folder(&r));
        WorktreeDialog::Remove(RemoveForm {
            local: local.to_string(),
            remote: remote.to_string(),
            in_a_tab,
            uncommitted: wt::uncommitted(Path::new(local)),
            delete_remote: can_delete_remote && in_a_tab,
            can_delete_remote,
        })
    }
}

fn discover(ws: &Workspace, ctx: &egui::Context) -> Receiver<Discovered> {
    let (tx, rx) = mpsc::channel();
    let (local, remote, ctx) = (PathBuf::from(&ws.local), Remote::parse(&ws.remote), ctx.clone());
    std::thread::spawn(move || {
        let extras = match remote {
            Ok(remote) => wt::gitignored_extras(&local, &remote),
            Err(_) => Vec::new(),
        };
        let setups = wt::discover_setup(&local, &extras);
        let _ = tx.send(Discovered { extras, setups });
        ctx.request_repaint();
    });
    rx
}

impl NewForm {
    fn take_listing(&mut self) {
        let Some(Discovered { extras, setups }) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.listing = None;
        self.extras = extras.into_iter().map(|extra| {
            let chosen = !extra.folder;
            (extra, chosen)
        }).collect();
        self.setups = setups;
    }

    fn remote_on_same_host(&self, ws: &Workspace) -> bool {
        matches!((Remote::parse(&ws.remote), Remote::parse(self.remote.trim())), (Ok(source), Ok(destination)) if source.same_host(&destination))
    }

    fn chosen_extras(&self, ws: &Workspace) -> Vec<Extra> {
        let same_host = self.remote_on_same_host(ws);
        self.extras
            .iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(extra, _)| Extra { remote: extra.remote && same_host, ..extra.clone() })
            .collect()
    }

    fn follow_branch(&mut self, ws: &Workspace) {
        if self.paths_edited {
            return;
        }
        let folder = wt::folder_name(&self.branch);
        let folder = if folder.is_empty() { "<branch>".to_string() } else { folder };
        self.local = wt::sibling_path(&ws.local, &folder);
        self.remote = wt::sibling_remote(&ws.remote, &folder);
    }

    fn plan(&self, ws: &Workspace) -> NewWorktree {
        NewWorktree {
            repo: PathBuf::from(&ws.local),
            branch: self.branch.trim().to_string(),
            base: self.base.trim().to_string(),
            local: PathBuf::from(self.local.trim()),
            remote: self.remote.trim().to_string(),
            source_remote: ws.remote.clone(),
            extras: self.chosen_extras(ws),
            setups: self.setups.iter().filter(|s| s.local || s.remote).cloned().collect(),
            workspaces_dir: crate::engine::workspaces_dir(),
        }
    }

    fn start(&self, ws: &Workspace, ctx: &egui::Context) -> (Workspace, Creating) {
        let plan = self.plan(ws);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut report = |step: Step| {
                let _ = tx.send(Creation::Step(step));
                ctx.request_repaint();
            };
            let created = wt::create(&plan, &mut report).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Creation::Finished(created));
            ctx.request_repaint();
        });
        let sessions = vec![TermTab::new(TermKind::Claude)];
        let created = Workspace { local: self.local.trim().to_string(), remote: self.remote.trim().to_string(), interval: ws.interval, sessions, forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() };
        (created, Creating { plan: self.plan(ws), steps: Vec::new(), events: rx, result: None })
    }
}

impl Tab {
    pub(super) fn creating(ws: Workspace, job: Creating) -> Self {
        let mut tab = Tab::new(ws);
        tab.creating = Some(job);
        tab
    }

    pub(super) fn creation_running(&self) -> bool {
        self.creating.as_ref().is_some_and(Creating::running)
    }

    pub(super) fn poll_creation(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.creating else { return };
        job.receive();
        if let (Some(Err(e)), None) = (&job.result, &self.error) {
            self.error = Some(e.clone());
        }
        let Some(Ok(setup_failures)) = &job.result else { return };
        let setup_failures = setup_failures.clone();
        self.creating = None;
        let local = PathBuf::from(&self.ws.local);
        self.ws.local = local.canonicalize().unwrap_or(local).display().to_string();
        self.linked_worktree = wt::is_linked(Path::new(&self.ws.local));
        self.start(ctx);
        if let Some(session) = &self.session
            && !setup_failures.is_empty()
        {
            session.shared.lock().unwrap().message = Some((true, format!("Worktree created, but its setup failed: {}", setup_failures.join("; "))));
        }
    }
}

pub(super) fn creating_tab_ui(tab: &Tab, ui: &mut egui::Ui) -> bool {
    let Some(job) = &tab.creating else { return false };
    let mut close = false;
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading(tab.ws.title());
        ui.monospace(format!("local:  {}\nremote: {}", tab.ws.local, tab.ws.remote));
    });
    egui::Window::new(format!("🌿 Creating the worktree {}", job.branch()))
        .id(egui::Id::new(("creating-worktree", &tab.ws.local)))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.set_width(560.0);
            creation_steps(ui, job);
            match &job.result {
                None => {
                    ui.add_space(6.0);
                    ui.weak("It goes on in the background: other workspace tabs can be used meanwhile.");
                }
                Some(Err(e)) => {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::LIGHT_RED, e);
                    close = ui.button("× Close this tab").clicked();
                }
                Some(Ok(_)) => {}
            }
        });
    close
}

pub(super) fn worktree_dialog_ui(ctx: &egui::Context, dialog: &mut WorktreeDialog, ws: &Workspace, pending: usize, unsaved: Option<&str>) -> WorktreeOutcome {
    match dialog {
        WorktreeDialog::New(form) => new_worktree_ui(ctx, form, ws),
        WorktreeDialog::Remove(form) => remove_worktree_ui(ctx, form, pending, unsaved),
        WorktreeDialog::DeleteBranch(form) => delete_branch_ui(ctx, form),
    }
}

fn new_worktree_ui(ctx: &egui::Context, form: &mut NewForm, ws: &Workspace) -> WorktreeOutcome {
    form.take_listing();
    let (mut create, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new("new-worktree")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("🌿 New worktree");
        ui.add_space(4.0);
        ui.label("A git worktree of this repository on its own branch, copied to its own remote folder, opened in a new workspace tab with its own Claude.");
        ui.add_space(8.0);
        new_worktree_fields(ui, form, ws);
        ui.add_space(6.0);
        extras_choice(ui, form, ws);
        ui.add_space(6.0);
        setup_choice(ui, form);
        ui.add_space(6.0);
        ui.weak("An existing branch (local, or a single remote one) is checked out; a new one starts from the base. The remote folder gets the tracked files, without .git. It is created in the background, in its new tab.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !form.branch.trim().is_empty() && !form.local.trim().is_empty() && !form.remote.trim().is_empty();
            create = ui.add_enabled(ready, egui::Button::new("🌿 Create")).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if create {
        let (ws, job) = form.start(ws, ctx);
        return WorktreeOutcome::Started { ws, job };
    }
    if cancel || modal.should_close() {
        return WorktreeOutcome::Closed;
    }
    WorktreeOutcome::Open
}

fn creation_steps(ui: &mut egui::Ui, job: &Creating) {
    for (i, step) in job.steps.iter().enumerate() {
        let last = i + 1 == job.steps.len();
        let failed = last && matches!(job.result, Some(Err(_)));
        ui.horizontal(|ui| {
            step_mark(ui, step, last && job.running(), failed);
            ui.label(step.describe(&job.plan));
        });
        if let (true, Step::CopyingToRemote { fraction, .. }) = (last && job.running(), step) {
            ui.add(egui::ProgressBar::new(*fraction));
        }
    }
    if job.steps.is_empty() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("starting…");
        });
    }
}

fn step_mark(ui: &mut egui::Ui, step: &Step, current: bool, failed: bool) {
    let finished = matches!(step, Step::RunningSetup { outcome: Some(_), .. });
    if failed || matches!(step, Step::RunningSetup { outcome: Some(false), .. }) {
        ui.label(RichText::new("✘").color(Color32::LIGHT_RED));
    } else if current && !finished {
        ui.spinner();
    } else {
        ui.label(RichText::new("✔").color(Color32::from_rgb(0x4c, 0xc0, 0x6a)));
    }
}

const EXTRAS_HEIGHT: f32 = 160.0;
const SETUPS_HEIGHT: f32 = 160.0;

fn setup_choice(ui: &mut egui::Ui, form: &mut NewForm) {
    ui.label(RichText::new("Set up (found from the lockfiles)").strong())
        .on_hover_text("Run in the new folder, locally and / or on the remote, after the copy, in a login shell. Ticked where this checkout already has the environment (node_modules, .venv…). A failed command does not undo the worktree.");
    if form.listing.is_some() {
        return;
    }
    if form.setups.is_empty() {
        ui.weak("No lockfile found.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("worktree-setups").max_height(SETUPS_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("worktree-setups-grid").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
            for setup in &mut form.setups {
                ui.checkbox(&mut setup.local, "local");
                ui.checkbox(&mut setup.remote, "remote");
                ui.monospace(&setup.reason);
                ui.add(egui::TextEdit::singleline(&mut setup.command).font(egui::TextStyle::Monospace).desired_width(260.0));
                ui.end_row();
            }
        });
    });
}

fn extras_choice(ui: &mut egui::Ui, form: &mut NewForm, ws: &Workspace) {
    ui.label(RichText::new("Also copy (gitignored, never synced)").strong())
        .on_hover_text("Each side gets its own copy: your local files into the new local worktree, the remote's into the new remote folder. An entry already there is left alone.");
    if form.listing.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("listing the gitignored files, locally and on the remote…");
        });
        return;
    }
    if form.extras.is_empty() {
        ui.weak("Nothing is gitignored.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("worktree-extras").max_height(EXTRAS_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
        for (extra, chosen) in &mut form.extras {
            let icon = if extra.folder { "📁" } else { "📄" };
            let sides = match (extra.local, extra.remote) {
                (true, true) => "local, remote",
                (true, false) => "local",
                _ => "remote",
            };
            ui.checkbox(chosen, format!("{icon} {}  ({sides})", extra.path));
        }
    });
    ui.horizontal(|ui| {
        if ui.small_button("files only").clicked() {
            form.extras.iter_mut().for_each(|(extra, chosen)| *chosen = !extra.folder);
        }
        if ui.small_button("none").clicked() {
            form.extras.iter_mut().for_each(|(_, chosen)| *chosen = false);
        }
    });
    let remote_chosen = form.extras.iter().any(|(extra, chosen)| *chosen && extra.remote);
    if remote_chosen && !form.remote_on_same_host(ws) {
        ui.colored_label(Color32::from_rgb(0xe0, 0xa0, 0x40), "The new remote folder is on another host: the remote entries are not copied.");
    }
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

fn remove_worktree_ui(ctx: &egui::Context, form: &mut RemoveForm, pending: usize, unsaved: Option<&str>) -> WorktreeOutcome {
    let (mut remove, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new("remove-worktree")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading(format!("Remove the worktree {}?", form.local));
        ui.add_space(6.0);
        let tab = if form.in_a_tab { "Its tab is closed (terminals ended, sync data deleted) and the" } else { "The" };
        ui.label(format!("{tab} local worktree is removed with git worktree remove, typed into a new local shell tab so you see how it goes. Its branch and commits are kept."));
        removal_warnings(ui, form, pending, unsaved);
        ui.add_space(6.0);
        let label = format!("Also delete the remote folder {}", form.remote);
        if !form.in_a_tab {
            ui.weak("Not open in a tab: the remote folder is guessed from the folder naming (<project>.wt/<name>).");
        }
        ui.add_enabled(form.can_delete_remote, egui::Checkbox::new(&mut form.delete_remote, label))
            .on_disabled_hover_text(format!("Only a remote folder inside a {} folder is deleted from here", wt::FOLDER_SUFFIX));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            remove = ui.button(RichText::new("🗑 Remove worktree").color(Color32::LIGHT_RED)).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if remove {
        return WorktreeOutcome::Remove { local: form.local.clone(), remote: form.remote.clone(), delete_remote: form.delete_remote };
    }
    if cancel || modal.should_close() {
        return WorktreeOutcome::Closed;
    }
    WorktreeOutcome::Open
}

fn delete_branch_ui(ctx: &egui::Context, form: &mut BranchForm) -> WorktreeOutcome {
    let (mut delete, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::new("delete-branch")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.heading(format!("Delete the branch {}?", form.branch.name));
        ui.add_space(6.0);
        ui.label("git branch -d, typed into a new local shell tab. It refuses a branch not merged yet, unless forced.");
        ui.checkbox(&mut form.force, "Force (git branch -D): delete it even if it is not merged, its own commits are lost");
        match form.branch.remote_and_name() {
            Some((remote, name)) => {
                ui.checkbox(&mut form.on_remote, format!("Also delete it on {remote} (git push {remote} --delete {name})"));
            }
            None => {
                ui.weak("It has no upstream branch: nothing to delete on a remote.");
            }
        }
        if let Some(e) = &form.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            delete = ui.button(RichText::new("🗑 Delete branch").color(Color32::LIGHT_RED)).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if delete {
        match wt::branch_deletion(&form.repo, &form.branch, form.force, form.on_remote) {
            Ok(command) => return WorktreeOutcome::RunInTerminal(command),
            Err(e) => form.error = Some(format!("{e:#}")),
        }
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
            tab.worktree = Some(WorktreeDialog::new_worktree(&tab.ws, ui.ctx()));
            ui.close();
        }
        let remove = ui.add_enabled(tab.linked_worktree, egui::Button::new("🗑 Remove this worktree…"));
        if remove.on_disabled_hover_text("This workspace is not a linked git worktree").clicked() {
            tab.worktree = Some(WorktreeDialog::remove_worktree(&tab.ws.local, &tab.ws.remote, true));
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
