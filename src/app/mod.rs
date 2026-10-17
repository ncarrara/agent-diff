//! The GUI: workspace tabs, file list and diff view.

mod commit;
mod definitions;
mod editor;
mod file_list;
mod find_in_file;
mod file_view;
mod factory_reset;
mod fork_graph;
mod options;
mod panes;
mod previews;
mod quick_open;
mod search;
mod session;
mod settings;
mod tab_view;
mod tabs;
mod terminal;
mod theme;
mod window;
mod workspace;
mod git_log;
mod worktree;

use crate::diff::{Hunk, LineKind};
use crate::highlight::{Highlighter, Line};
use crate::engine::{Action, Body, Change, Engine, FileEntry, Origin, Rename, Repo, TextDiff, Texts};
use crate::remote::Remote;
use eframe::egui::{self, Color32, RichText};
use notify::Watcher;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use commit::*;
use definitions::*;
use editor::*;
use file_list::*;
use find_in_file::*;
use file_view::*;
use factory_reset::*;
use fork_graph::*;
use options::*;
use panes::*;
use previews::*;
use quick_open::*;
use search::*;
use session::*;
use settings::*;
use tab_view::*;
use tabs::*;
use terminal::*;
use theme::*;
use window::*;
use workspace::*;
use git_log::*;
use worktree::*;

pub struct AgentDiffApp {
    tabs: Vec<Tab>,
    /// Index in `tabs`; `None` shows the "new workspace" form.
    active: Option<usize>,
    /// The tab whose close confirmation is open.
    confirm_close: Option<usize>,
    form_local: String,
    form_remote: String,
    form_interval: f64,
    form_error: Option<String>,
    /// Settings as last written to disk, to save only when something changed.
    saved_settings: String,
    options: Options,
    highlighter: Highlighter,
    quit: QuitGuard,
    factory_reset: FactoryReset,
    wiped: bool,
}

impl AgentDiffApp {
    pub fn new(ctx: &egui::Context, local: Option<PathBuf>, remote: Option<String>, interval: Option<f64>) -> Self {
        let saved = load_settings();
        let get = |k: &str| saved.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        let on = |k: &str| get(k).is_some_and(|v| v == "true");
        let legacy_view = ListView { tree: on("tree_view"), expand: on("expand_tree"), ..ListView::default() };
        let mut workspaces: Vec<Workspace> = saved
            .iter()
            .filter(|(k, _)| k == "workspace")
            .filter_map(|(_, v)| {
                let mut parts = v.split('\t');
                let (local, remote) = (parts.next()?.to_string(), parts.next()?.to_string());
                let interval = parts.next().and_then(|i| i.parse().ok()).unwrap_or(2.0);
                let sessions = TermTab::parse_list(parts.next().unwrap_or(""));
                let skip_commit = parse_repo_list(parts.next().unwrap_or(""));
                let mut view = ListView::parse(parts.next().unwrap_or(""), parts.next().unwrap_or(""), &legacy_view);
                let forks = Fork::parse_list(parts.next().unwrap_or(""));
                let _former_ignored_folders = parts.next();
                view.opened_gitignored = parse_repo_list(parts.next().unwrap_or("")).into_iter().collect();
                Some(Workspace { local, remote, interval, sessions, forks, skip_commit, view })
            })
            .collect();
        let mut active = get("active").and_then(|a| a.parse().ok());
        // Settings from before tabs held a single workspace.
        if workspaces.is_empty()
            && let (Some(local), Some(remote)) = (get("local"), get("remote"))
            && !local.is_empty()
            && !remote.is_empty()
        {
            let interval = get("interval").and_then(|i| i.parse().ok()).unwrap_or(2.0);
            workspaces.push(Workspace { local, remote, interval, sessions: Vec::new(), forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() });
        }

        // A workspace given on the command line is added (if new) and shown.
        if let (Some(local), Some(remote)) = (local, remote) {
            let local = local.canonicalize().unwrap_or(local).display().to_string();
            let ws = Workspace { local, remote: remote.trim().to_string(), interval: interval.unwrap_or(2.0), sessions: Vec::new(), forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() };
            let i = match workspaces.iter().position(|w| w.local == ws.local && w.remote == ws.remote) {
                Some(i) => {
                    workspaces[i].interval = interval.unwrap_or(workspaces[i].interval);
                    i
                }
                None => {
                    workspaces.push(ws);
                    workspaces.len() - 1
                }
            };
            active = Some(i);
        }

        let tabs: Vec<Tab> = workspaces.into_iter().map(|ws| Tab::open(ws, ctx)).collect();
        let active = if tabs.is_empty() { None } else { Some(active.unwrap_or(0).min(tabs.len() - 1)) };
        let mut app = AgentDiffApp {
            tabs,
            active,
            confirm_close: None,
            form_local: String::new(),
            form_remote: String::new(),
            form_interval: interval.unwrap_or(2.0),
            form_error: None,
            saved_settings: String::new(),
            options: Options::load(&saved),
            highlighter: Highlighter::new(),
            quit: QuitGuard::default(),
            factory_reset: FactoryReset::default(),
            wiped: false,
        };
        app.apply_theme(ctx, app.options.theme);
        app.saved_settings = app.settings_text();
        app
    }

    /// Open the workspace from the form in a new tab (or switch to it if it
    /// is already open).
    fn add_workspace(&mut self, ctx: &egui::Context) {
        let local = PathBuf::from(self.form_local.trim());
        let local = local.canonicalize().unwrap_or(local).display().to_string();
        let ws = Workspace { local, remote: self.form_remote.trim().to_string(), interval: self.form_interval.max(0.5), sessions: Vec::new(), forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() };
        if let Some(i) = self.tabs.iter().position(|t| t.ws.local == ws.local && t.ws.remote == ws.remote) {
            self.active = Some(i);
            return;
        }
        let tab = Tab::open(ws, ctx);
        match &tab.error {
            Some(e) => self.form_error = Some(e.clone()),
            None => {
                self.form_error = None;
                self.form_local.clear();
                self.form_remote.clear();
                self.tabs.push(tab);
                self.active = Some(self.tabs.len() - 1);
            }
        }
    }
}

impl AgentDiffApp {
    fn notify_finished_commands(&mut self, ctx: &egui::Context) {
        let window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let watched = window_focused && self.active == Some(i) && tab.layout.terminal_shown;
            for message in tab.terminals.poll_finished(&tab.ws.sessions, watched, ctx) {
                if !window_focused {
                    notify_desktop(&format!("agent-diff · {}", tab.ws.title()), &message);
                }
            }
        }
    }

    fn settings_text(&self) -> String {
        let mut text = self.options.settings_text();
        for t in &self.tabs {
            text += &format!(
                "workspace={}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t\t{}\n",
                t.ws.local,
                t.ws.remote,
                t.ws.interval,
                TermTab::list_to_string(&t.ws.sessions),
                repo_list_to_string(&t.ws.skip_commit),
                t.ws.view.flags(),
                t.ws.view.flipped_text(),
                Fork::list_to_string(&t.ws.forks),
                repo_list_to_string(&t.ws.view.opened_gitignored_list())
            );
        }
        if let Some(a) = self.active {
            text += &format!("active={a}\n");
        }
        text
    }

    fn wipe_and_quit(&mut self, ctx: &egui::Context) {
        let remotes: Vec<String> = self.tabs.iter().map(|t| t.ws.remote.clone()).collect();
        self.tabs.clear();
        self.active = None;
        self.wiped = true;
        for error in wipe_app_data(&remotes) {
            eprintln!("factory reset: could not delete {error}");
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn save_settings_if_changed(&mut self) {
        if self.wiped {
            return;
        }
        let text = self.settings_text();
        if text != self.saved_settings
            && let Some(f) = settings_file()
        {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            if std::fs::write(f, &text).is_ok() {
                self.saved_settings = text;
            }
        }
    }
}

impl AgentDiffApp {
    fn apply_theme(&mut self, ctx: &egui::Context, index: usize) {
        let t = &THEMES[index.min(THEMES.len() - 1)];
        self.options.theme = index;
        self.highlighter.set_theme(t.syntax);
        for tab in &mut self.tabs {
            tab.hl = None;
            tab.hl_job = None;
        }
        let (pref, mut visuals) =
            if t.dark { (egui::Theme::Dark, egui::Visuals::dark()) } else { (egui::Theme::Light, egui::Visuals::light()) };
        if let Some(bg) = self.highlighter.background() {
            visuals.panel_fill = bg;
            visuals.window_fill = bg;
        }
        ctx.set_theme(pref);
        ctx.set_visuals_of(pref, visuals);
        ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, palette_for(t.dark)));
    }
}
const CLAUDE: Color32 = Color32::from_rgb(0xd9, 0x8a, 0x5b);
const YOU: Color32 = Color32::from_rgb(0x6c, 0xa8, 0xe8);
const BOTH: Color32 = Color32::from_rgb(0xe0, 0xc0, 0x40);

impl eframe::App for AgentDiffApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tab_bar(ui);
        for tab in &mut self.tabs {
            tab.poll_creation(ui.ctx());
        }
        let mut close_creating = None;
        match self.active {
            Some(i) => {
                let tab = &mut self.tabs[i];
                tab.take_failed_session();
                if tab.creating.is_some() {
                    close_creating = creating_tab_ui(tab, ui).then_some(i);
                } else if tab.session.is_some() {
                    tab_ui(tab, ui, &self.highlighter, &mut self.options);
                } else {
                    failed_tab_ui(tab, ui);
                }
            }
            None => self.setup_ui(ui),
        }
        if let Some(i) = close_creating {
            self.close_tab(i);
        }
        if let Some(i) = self.active
            && let Some(path) = self.tabs[i].git_log.take_remove_request()
        {
            self.ask_to_remove_worktree(i, &path);
        }
        if let Some(i) = self.active
            && let Some((repo, branch)) = self.tabs[i].git_log.take_branch_request()
        {
            self.tabs[i].worktree = Some(WorktreeDialog::DeleteBranch(BranchForm::new(repo, branch)));
        }
        resize_edges(ui.ctx());
        self.notify_finished_commands(ui.ctx());
        let running: Vec<(String, String)> = self.tabs.iter().filter_map(|t| Some((t.ws.title(), t.running_action()?))).collect();
        guard_quit(ui.ctx(), &mut self.quit, &running);
        if let ResetChoice::Confirmed = self.factory_reset.dialog(ui.ctx(), &running) {
            self.wipe_and_quit(ui.ctx());
        }
        if let Some(i) = self.active
            && self.tabs[i].editing.is_some()
        {
            self.edit_dialog(ui, i);
        }
        if let Some(i) = self.active
            && self.tabs[i].worktree.is_some()
        {
            self.worktree_dialog(ui.ctx(), i);
        }
        self.save_settings_if_changed();
        // Keep the "synced Ns ago" label ticking.
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
}

impl AgentDiffApp {
    /// Workspace tabs, plus the global display options.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        let mut new_theme = None;
        let mut colors_changed = false;
        let mut tab_move = None;
        egui::Panel::top("tabs").show(ui, |ui| {
            let bar = ui
                .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(6.0);
                    ui.horizontal_top(|ui| {
                        let width = (ui.available_width() - BUTTONS_WIDTH).max(0.0);
                        let wrap = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                        ui.allocate_ui_with_layout(egui::vec2(width, 0.0), wrap, |ui| {
                            for (i, tab) in self.tabs.iter().enumerate() {
                                let badge = match (tab.pending(), &tab.error) {
                                    (_, Some(_)) => " ⚠".to_string(),
                                    (Some(0), _) => " ✔".to_string(),
                                    (Some(n), _) => format!(" ({n})"),
                                    (None, _) => String::new(),
                                };
                                let branch = tab.branch().map(|b| format!(" · {b}")).unwrap_or_default();
                                let text = RichText::new(format!("{}{branch}{badge}", tab.ws.title())).strong();
                                let hover = format!("{}\n⇄ {}", tab.ws.local, tab.ws.remote);
                                let activity = if tab.creation_running() { TermActivity::Working } else { tab.terminals.activity() };
                                let label = |ui: &mut egui::Ui| {
                                    tab_label(ui, text);
                                    terminal_activity_mark(ui, activity);
                                };
                                let shown = closable_tab(ui, self.active == Some(i), label, Some("Close this workspace and reset it (its sync state is deleted)"));
                                if let Some(moved) = dragged_onto(ui, egui::Id::new("workspace-tabs"), i, &shown.response) {
                                    tab_move = Some(moved);
                                }
                                if shown.response.on_hover_text(format!("{hover}\ndrag to reorder")).clicked() {
                                    self.active = Some(i);
                                }
                                if shown.close {
                                    self.confirm_close = Some(i);
                                }
                            }
                            if ui.selectable_label(self.active.is_none(), "➕ Workspace").clicked() {
                                self.active = None;
                            }
                            ui.separator();
                            let change = options_menu(ui, &mut self.options);
                            new_theme = change.theme;
                            colors_changed = change.syntax_colors;
                            if change.factory_reset {
                                self.factory_reset.ask();
                            }
                        });
                        window_buttons(ui);
                    });
                    ui.add_space(4.0);
                })
                .response;
            title_bar(ui.ctx(), &bar);
        });
        if let Some(theme) = new_theme {
            self.apply_theme(ui.ctx(), theme);
        }
        if let Some(moved) = tab_move {
            move_tab(&mut self.tabs, moved);
            self.active = self.active.map(|i| index_after_move(i, moved));
            self.confirm_close = self.confirm_close.map(|i| index_after_move(i, moved));
        }
        if colors_changed {
            for tab in &mut self.tabs {
                tab.hl = None;
                tab.hl_job = None;
            }
        }
        if let Some(i) = self.confirm_close
            && let Some(yes) = confirm(
                ui,
                egui::Id::new("confirm-close"),
                &format!("Close {}?", self.tabs[i].ws.title()),
                &format!(
                    "Its sync stops, its terminals are ended and it is reset: its mirror, baseline, backups, pushes still waiting and saved shell output are deleted. The local files, the remote files and Claude's conversations on the remote are not touched; opening it again starts from a fresh baseline.{}",
                    match &self.tabs[i].edit {
                        Some(ed) if ed.dirty() => format!("\n\nYour unsaved changes to {} are lost.", ed.path),
                        _ => String::new(),
                    }
                ),
                "× Close",
            )
        {
            self.confirm_close = None;
            close = yes.then_some(i);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }

    fn close_tab(&mut self, i: usize) {
        let places = workspace_data_places(&self.tabs[i].ws);
        self.tabs.remove(i);
        let errors = wipe_workspace_data(&places);
        self.active = self.active_after_removing(i);
        self.report_close_errors(&errors);
    }

    fn active_after_removing(&self, i: usize) -> Option<usize> {
        match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) => Some(a.min(self.tabs.len() - 1)),
            None => None,
        }
    }

    fn tab_of_folder(&self, folder: &Path) -> Option<usize> {
        let canonical = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
        self.tabs.iter().position(|t| Path::new(&t.ws.local) == folder || Path::new(&t.ws.local) == canonical)
    }

    fn ask_to_remove_worktree(&mut self, i: usize, path: &Path) {
        let dialog = match self.tab_of_folder(path) {
            Some(j) => WorktreeDialog::remove_worktree(&self.tabs[j].ws.local, &self.tabs[j].ws.remote, true),
            None => {
                let folder = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let remote = crate::worktree::sibling_remote(&self.tabs[i].ws.remote, &folder);
                WorktreeDialog::remove_worktree(&path.display().to_string(), &remote, false)
            }
        };
        self.tabs[i].worktree = Some(dialog);
    }

    fn worktree_dialog(&mut self, ctx: &egui::Context, i: usize) {
        let target = match &self.tabs[i].worktree {
            Some(WorktreeDialog::Remove(form)) => self.tab_of_folder(Path::new(&form.local)),
            _ => Some(i),
        };
        let pending = target.and_then(|j| self.tabs[j].pending()).unwrap_or(0);
        let unsaved = target.and_then(|j| self.tabs[j].edit.as_ref().filter(|ed| ed.dirty()).map(|ed| ed.path.clone()));
        let tab = &mut self.tabs[i];
        let Some(dialog) = tab.worktree.as_mut() else { return };
        let outcome = worktree_dialog_ui(ctx, dialog, &tab.ws, pending, unsaved.as_deref());
        match outcome {
            WorktreeOutcome::Open => {}
            WorktreeOutcome::Closed => self.tabs[i].worktree = None,
            WorktreeOutcome::Started { ws, job } => {
                self.tabs[i].worktree = None;
                self.tabs.insert(i + 1, Tab::creating(ws, job));
                self.active = Some(i + 1);
            }
            WorktreeOutcome::RunInTerminal(command) => {
                let tab = &mut self.tabs[i];
                tab.worktree = None;
                tab.terminals.run_in_new_shell(&mut tab.ws.sessions, TermKind::Local, command);
                self.options.layout.terminal_shown = true;
            }
            WorktreeOutcome::Remove { local, remote, delete_remote } => {
                self.tabs[i].worktree = None;
                self.remove_worktree(local, remote, delete_remote);
            }
        }
    }

    fn remove_worktree(&mut self, local: String, remote: String, delete_remote: bool) {
        let planned = Remote::parse(&remote).and_then(|r| crate::worktree::removal(Path::new(&local), &r, delete_remote));
        let removal = match planned {
            Ok(removal) => removal,
            Err(e) => return self.tell_active(true, format!("The worktree {local} is not removed: {e:#}")),
        };
        match self.tab_of_folder(Path::new(&local)) {
            Some(j) => self.close_tab(j),
            None => {
                let places = Remote::parse(&remote).map(|r| crate::engine::workspace_state_dirs(&local, &r)).unwrap_or_default();
                let errors = wipe_workspace_data(&places);
                self.report_close_errors(&errors);
            }
        }
        let Some(host) = self.tab_of_folder(&removal.main).or(self.active) else {
            return eprintln!("no workspace tab left to run: {}", removal.command);
        };
        let tab = &mut self.tabs[host];
        tab.terminals.run_in_new_shell(&mut tab.ws.sessions, TermKind::Local, removal.command);
        self.options.layout.terminal_shown = true;
        self.active = Some(host);
    }

    fn tell_active(&self, error: bool, text: String) {
        match self.active.and_then(|a| self.tabs[a].session.as_ref()) {
            Some(session) => session.shared.lock().unwrap().message = Some((error, text)),
            None => eprintln!("{text}"),
        }
    }

    fn report_close_errors(&self, errors: &[String]) {
        let shown = self.active.and_then(|a| self.tabs[a].session.as_ref());
        if let Some(session) = shown
            && !errors.is_empty()
        {
            session.shared.lock().unwrap().message = Some((true, format!("Workspace closed, but could not delete {}", errors.join(", "))));
        }
    }

    /// The "edit workspace" dialog of tab `i`. Saving restarts the session
    /// with the new settings; an invalid setting leaves the old one running.
    fn edit_dialog(&mut self, ui: &mut egui::Ui, i: usize) {
        let ctx = ui.ctx().clone();
        let mut save = false;
        let mut cancel = false;
        let mut reset = false;
        let running = self.tabs[i].running_action();
        let ws = self.tabs[i].ws.clone();
        let form = self.tabs[i].editing.as_mut().unwrap();
        let modal = egui::Modal::new(egui::Id::new(("edit-workspace", i))).show(&ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Edit workspace");
            ui.add_space(8.0);
            egui::Grid::new("edit-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Local workspace");
                ui.add(egui::TextEdit::singleline(&mut form.local).desired_width(380.0));
                ui.end_row();
                ui.label("Remote workspace");
                ui.add(egui::TextEdit::singleline(&mut form.remote).hint_text("user@host:/path/to/project").desired_width(380.0));
                ui.end_row();
                ui.label("Poll interval (s)");
                ui.add(egui::DragValue::new(&mut form.interval).range(0.5..=60.0).speed(0.1));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.weak("Changing a path starts from a fresh baseline for that pair (the current local files).");
            if let Some(e) = &form.error {
                ui.colored_label(Color32::LIGHT_RED, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                save = ui.button("Save").clicked();
                cancel = ui.button("Cancel").clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(form.asking_reset, "🗑 Reset this workspace…").on_hover_text("Delete this workspace's sync data and saved settings, and start it again fresh").clicked() {
                        form.asking_reset = !form.asking_reset;
                    }
                });
            });
            if form.asking_reset {
                ui.separator();
                reset = workspace_reset_ui(ui, &ws, running.as_deref());
            }
        });
        if reset {
            self.reset_workspace(i, &ctx);
            return;
        }
        if cancel || modal.should_close() {
            self.tabs[i].editing = None;
            return;
        }
        if !save {
            return;
        }

        let form = self.tabs[i].editing.as_mut().unwrap();
        let local = PathBuf::from(form.local.trim());
        let local = local.canonicalize().unwrap_or(local).display().to_string();
        let remote = form.remote.trim().to_string();
        let interval = form.interval.max(0.5);
        // Claude's conversations belong to the remote workspace.
        let same_remote = remote == self.tabs[i].ws.remote;
        let sessions = if same_remote { self.tabs[i].ws.sessions.clone() } else { Vec::new() };
        let forks = if same_remote { self.tabs[i].ws.forks.clone() } else { Vec::new() };
        let skip_commit = if local == self.tabs[i].ws.local { self.tabs[i].ws.skip_commit.clone() } else { Vec::new() };
        let view = self.tabs[i].ws.view.clone();
        let ws = Workspace { local, remote, interval, sessions, forks, skip_commit, view };
        if self.tabs.iter().enumerate().any(|(j, t)| j != i && t.ws.local == ws.local && t.ws.remote == ws.remote) {
            self.tabs[i].editing.as_mut().unwrap().error = Some("This workspace is already open in another tab.".into());
            return;
        }
        // Validate before replacing the running session.
        match start_workspace_session(&ws, &ctx) {
            Ok(session) => {
                let tab = &mut self.tabs[i];
                // Dropping the old session stops its watcher and sync thread.
                tab.session = Some(session);
                if ws.local != tab.ws.local {
                    tab.edit = None;
                }
                if ws.remote != tab.ws.remote {
                    tab.terminals = Terminals::default();
                } else if ws.local != tab.ws.local {
                    tab.terminals.end(&tab.ws.sessions, TermKind::Local);
                }
                tab.linked_worktree = crate::worktree::is_linked(Path::new(&ws.local));
                tab.ws = ws;
                tab.error = None;
                tab.editing = None;
                tab.selected = None;
                tab.order.clear();
                tab.jumped_to = None;
                tab.hl = None;
                tab.hl_job = None;
            }
            Err(e) => self.tabs[i].editing.as_mut().unwrap().error = Some(format!("{e:#}")),
        }
    }

    fn reset_workspace(&mut self, i: usize, ctx: &egui::Context) {
        let old = &self.tabs[i].ws;
        let places = workspace_data_places(old);
        let ws = Workspace { local: old.local.clone(), remote: old.remote.clone(), interval: old.interval, sessions: Vec::new(), forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() };
        self.tabs[i].session = None;
        self.tabs[i].terminals = Terminals::default();
        let errors = wipe_workspace_data(&places);
        self.tabs[i] = Tab::open(ws, ctx);
        if let Some(session) = &self.tabs[i].session {
            let message = if errors.is_empty() { (false, "Workspace reset to factory settings".to_string()) } else { (true, format!("Workspace reset, but could not delete {}", errors.join(", "))) };
            session.shared.lock().unwrap().message = Some(message);
        }
    }

    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Add a workspace");
            ui.label("Watch a remote workspace where Claude is working, review its changes and pick what to pull.");
            ui.add_space(12.0);
            egui::Grid::new("setup").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Local workspace");
                ui.add(egui::TextEdit::singleline(&mut self.form_local).hint_text("/home/me/project").desired_width(420.0));
                ui.end_row();
                ui.label("Remote workspace");
                ui.add(egui::TextEdit::singleline(&mut self.form_remote).hint_text("user@host:/path/to/project").desired_width(420.0));
                ui.end_row();
                ui.label("Poll interval (s)");
                ui.add(egui::DragValue::new(&mut self.form_interval).range(0.5..=60.0).speed(0.1));
                ui.end_row();
            });
            ui.add_space(8.0);
            if ui.button("Start watching").clicked() {
                let ctx = ui.ctx().clone();
                self.add_workspace(&ctx);
            }
            if let Some(e) = &self.form_error {
                ui.colored_label(Color32::LIGHT_RED, e);
            }
            ui.add_space(12.0);
            ui.weak("The local workspace must be, or contain, git repositories: only their tracked files, and new files from the remote, are synced.");
            ui.weak("The remote is accessed with rsync over ssh (key-based auth required). A plain directory path also works.");
            ui.weak("On first start, the current local workspace becomes the baseline: every difference is attributed to the remote.");
            ui.weak("Workspaces are saved and reopened at the next start.");
        });
    }
}
