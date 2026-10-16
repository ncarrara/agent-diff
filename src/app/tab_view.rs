use super::*;

mod explorer;
use explorer::*;

struct Frame {
    local: String,
    remote: String,
    state_dir: String,
    mirror: PathBuf,
    shared: Arc<Mutex<Shared>>,
    entries: Arc<Vec<FileEntry>>,
    synced: Arc<Vec<FileEntry>>,
    repos: Arc<Vec<Repo>>,
    untracked: Arc<std::collections::BTreeSet<String>>,
    gitignored: Arc<std::collections::BTreeSet<String>>,
    gitignored_entries: Arc<Vec<FileEntry>>,
    gitignored_dirs: Arc<std::collections::BTreeSet<String>>,
    empty_folders: Arc<std::collections::BTreeSet<String>>,
    last_fetch: Option<SystemTime>,
    fetch_error: Option<String>,
    message: Option<(bool, String)>,
    busy: bool,
    progress: Option<Progress>,
    activity: Option<String>,
    working_a_while: bool,
    cmds: Vec<Cmd>,
}

impl Frame {
    fn of(session: &Session) -> Self {
        let s = session.shared.lock().unwrap();
        Frame {
            local: session.local.clone(),
            remote: session.remote.clone(),
            state_dir: session.state_dir.clone(),
            mirror: Path::new(&session.state_dir).join("mirror"),
            shared: session.shared.clone(),
            entries: s.entries.clone(),
            synced: s.synced.clone(),
            repos: s.repos.clone(),
            untracked: s.untracked.clone(),
            gitignored: s.gitignored.clone(),
            gitignored_entries: s.gitignored_entries.clone(),
            gitignored_dirs: s.gitignored_dirs.clone(),
            empty_folders: s.empty_folders.clone(),
            last_fetch: s.last_fetch,
            fetch_error: s.fetch_error.clone(),
            message: s.message.clone(),
            busy: s.busy,
            progress: s.progress.clone(),
            activity: s.activity.clone(),
            working_a_while: false,
            cmds: Vec::new(),
        }
    }

    fn report_error(&self, error: String) {
        self.shared.lock().unwrap().message = Some((true, error));
    }

    fn entry(&self, path: &str) -> Option<&FileEntry> {
        self.entries.iter().chain(self.gitignored_entries.iter()).chain(self.synced.iter()).find(|e| e.path == path)
    }

    fn searchable(&self) -> Vec<(String, PathBuf)> {
        let files = self.entries.iter().chain(self.gitignored_entries.iter()).chain(self.synced.iter());
        search_files(files, &self.local, &self.mirror)
    }

    fn search_scope(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        (self.untracked.len(), self.gitignored.len()).hash(&mut hasher);
        hasher.finish()
    }

    fn place(&self) -> FilePlace<'_> {
        FilePlace { local_root: &self.local, mirror: &self.mirror, remote: &self.remote }
    }
}

pub(super) fn tab_ui(tab: &mut Tab, ui: &mut egui::Ui, highlighter: &Highlighter, options: &mut Options) {
    let syntax_colors = options.syntax_colors;
    adopt_layout(tab, &options.layout);
    tab.git_log.adopt(options.git_poll);
    let Options { full_view, .. } = options;
    let mut terminal_shown = tab.layout.terminal_shown;
    let show_terminal = &mut terminal_shown;
    let mut f = Frame::of(tab.session.as_ref().unwrap());
    f.working_a_while = working_a_while(ui.ctx(), &f);
    reveal_created_file(tab, &mut f);
    let terminal_focused = tab.terminals.has_focus(&tab.ws.sessions, ui.ctx());
    let shortcut = shortcut_pressed(ui.ctx(), terminal_focused);
    close_quick_open_on_escape(ui.ctx(), tab);
    if !picker_in_terminal_window(tab)
        && let Some(picker) = &mut tab.new_terminal
    {
        let outcome = picker.keys(ui.ctx());
        finish_new_terminal(ui.ctx(), tab, outcome, show_terminal);
    }

    follow_list_order(tab, &f.entries);
    let edit = top_bar(ui, tab, &mut f, show_terminal);
    status_bar(ui, &f);
    let terminal_out = is_detached(tab, Pane::Terminal);
    if tab.panes.zoomed.is_none() && !terminal_out {
        terminal_area(ui, tab, show_terminal);
    }
    apply_shortcut(ui.ctx(), tab, &f, shortcut, terminal_focused, show_terminal);
    update_searches(ui.ctx(), tab, &f);
    ui.ctx().data_mut(|d| d.remove::<Option<String>>(egui::Id::new(HOVERED_WORD_ID)));

    let mut acts = ListActions::default();
    leave_editor_for_selection(tab);
    if !tab.terminals.maximized || terminal_out {
        let zoomed = tab.panes.zoomed;
        let here = |pane| !is_detached(tab, pane) && zoomed.is_none_or(|z| z == pane);
        let (files_here, changes_here, main_here) = (here(Pane::Files), here(Pane::Changes), here(Pane::Main));
        if files_here {
            explorer_panel(ui, tab, &mut f, &mut acts, ExplorerKind::Files, !main_here && !changes_here);
        }
        if changes_here {
            explorer_panel(ui, tab, &mut f, &mut acts, ExplorerKind::Changes, !main_here);
        }
        if main_here {
            egui::CentralPanel::default().show(ui, |ui| main_panel(ui, tab, &mut f, highlighter, syntax_colors, full_view, show_terminal));
        } else if !files_here && !changes_here {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.weak("The panels are in their own windows: ⧉ there, or closing one, puts it back here."));
            });
        }
    }
    let view = ViewArgs { highlighter, syntax_colors, full_view, show_terminal };
    pane_windows(ui, tab, &mut f, &mut acts, view);
    options.git_poll = tab.git_log.poll();
    if std::mem::take(&mut tab.terminals.window_toggle) {
        toggle_window(tab, Pane::Terminal);
    }

    definition_results(ui.ctx(), tab, &f);
    quick_open_window(ui.ctx(), tab, &f);
    new_terminal_window(ui.ctx(), tab, show_terminal);
    bulk_confirmation(ui, tab, &mut f);
    list_actions(ui, tab, &mut f, acts);
    rename_and_new_file_dialogs(ui, tab, &mut f);
    commit_and_delete_dialogs(ui, tab, &mut f);
    delete_folder_dialog(ui, tab, &mut f);

    if let Some(session) = &tab.session {
        for c in f.cmds {
            let _ = session.tx.send(c);
        }
    }
    if edit {
        tab.open_editor();
    }
    tab.panes.paint(ui.ctx());
    remember_layout(tab, terminal_shown);
    options.layout = tab.layout.clone();
}

fn adopt_layout(tab: &mut Tab, layout: &Layout) {
    tab.layout = layout.clone();
    tab.terminals.maximized = layout.terminal_maximized;
    tab.panes.zoomed = layout.zoomed;
}

fn is_detached(tab: &Tab, pane: Pane) -> bool {
    tab.layout.detached.contains(&pane)
}

fn toggle_window(tab: &mut Tab, pane: Pane) {
    let detached = &mut tab.layout.detached;
    match detached.iter().position(|p| *p == pane) {
        Some(at) => {
            detached.remove(at);
        }
        None => {
            detached.push(pane);
            if tab.panes.zoomed == Some(pane) {
                tab.panes.zoomed = None;
            }
            if pane == Pane::Terminal {
                tab.terminals.maximized = false;
            }
        }
    }
}

struct ViewArgs<'a> {
    highlighter: &'a Highlighter,
    syntax_colors: bool,
    full_view: &'a mut bool,
    show_terminal: &'a mut bool,
}

fn window_id(pane: Pane) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("pane-window", pane_name(pane)))
}

fn window_size(pane: Pane) -> egui::Vec2 {
    match pane {
        Pane::Files => egui::vec2(420.0, 760.0),
        Pane::Changes => egui::vec2(420.0, 760.0),
        Pane::Main => egui::vec2(1000.0, 760.0),
        Pane::Terminal => egui::vec2(1000.0, 520.0),
    }
}

fn pane_windows(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, acts: &mut ListActions, view: ViewArgs) {
    let ViewArgs { highlighter, syntax_colors, full_view, show_terminal } = view;
    for pane in tab.layout.detached.clone() {
        let id = window_id(pane);
        let builder = egui::ViewportBuilder::default().with_title(format!("{} · {}", pane_name(pane), tab.ws.title())).with_inner_size(window_size(pane));
        let closed = ui.ctx().show_viewport_immediate(id, builder, |ui, _| {
            match pane {
                Pane::Files => explorer_panel(ui, tab, f, acts, ExplorerKind::Files, true),
                Pane::Changes => explorer_panel(ui, tab, f, acts, ExplorerKind::Changes, true),
                Pane::Main => {
                    egui::CentralPanel::default().show(ui, |ui| main_panel(ui, tab, f, highlighter, syntax_colors, full_view, show_terminal));
                }
                Pane::Terminal => {
                    new_terminal_in_window(ui.ctx(), tab, show_terminal);
                    egui::CentralPanel::default().show(ui, |ui| {
                        terminal_panel(&mut tab.terminals, &mut tab.ws.sessions, &mut tab.ws.forks, (&tab.ws.local, &tab.ws.remote), ui, show_terminal);
                    });
                }
            }
            ui.ctx().input(|i| i.viewport().close_requested())
        });
        tab.panes.forget(pane);
        if closed {
            toggle_window(tab, pane);
        }
    }
}

fn remember_layout(tab: &mut Tab, terminal_shown: bool) {
    let layout = &mut tab.layout;
    layout.terminal_shown = terminal_shown;
    layout.terminal_maximized = tab.terminals.maximized;
    layout.zoomed = tab.panes.zoomed;
}

fn panel_id(name: &str) -> egui::Id {
    egui::Id::new(name)
}

fn follow_list_order(tab: &mut Tab, entries: &[FileEntry]) {
    let order: Vec<String> = LIST_ORDER
        .iter()
        .flat_map(|&o| sorted_group(entries, o, tab.ws.view.changes_tree))
        .filter(|e| matches_filter(&e.path, &tab.explorers[ExplorerKind::Changes.index()].filter))
        .map(|e| e.path.clone())
        .collect();
    if tab.file_tabs.pinned.is_none()
        && let Some(sel) = &tab.selected
        && !order.contains(sel)
        && let Some(pos) = tab.order.iter().position(|p| p == sel)
    {
        let (before, after) = tab.order.split_at(pos);
        tab.selected = after.iter().chain(before.iter().rev()).find(|p| order.contains(p)).cloned();
    }
    tab.order = order;
}

fn top_bar(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, show_terminal: &mut bool) -> bool {
    let mut edit = false;
    egui::Panel::top("top").show(ui, |ui| {
        ui.add_space(4.0);
        workspace_paths(ui, f);
        ui.horizontal_wrapped(|ui| {
            commit_button(ui, tab, f);
            edit = workspace_buttons(ui, f);
            worktree_menu(ui, tab);
            terminal_menu(ui, tab, show_terminal);
            progress_indicator(ui, f);
        });
        ui.add_space(4.0);
    });
    edit
}

fn workspace_paths(ui: &mut egui::Ui, f: &Frame) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("local").color(YOU).strong());
        ui.monospace(&f.local);
        ui.label("⇄");
        ui.label(RichText::new("remote").color(CLAUDE).strong());
        ui.monospace(&f.remote);
    });
}

fn commit_button(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame) {
    if ui.add_enabled(!f.busy, egui::Button::new("✔ Commit…")).on_hover_text("Commit your local changes in the git repositories of the workspace").clicked() {
        tab.commit = Some(CommitDialog::open(&tab.ws.local, &tab.ws.skip_commit));
    }
}

const RESET_BASE_HELP: &str = "Treat local as reference: use your current local files as the starting point: every remaining difference \
     is then listed as Claude's (\"Changed on remote\").\n\n\
     Use it when too many files show as \"Changed on both sides\" (e.g. after syncing \
     by other means), or to start a new task and see only what Claude does from now on.\n\n\
     No file is modified, locally or on the remote: only how differences are classified.";

fn workspace_buttons(ui: &mut egui::Ui, f: &mut Frame) -> bool {
    if ui.button("⟳").on_hover_text("Refresh: fetch the remote now and compare again").clicked() {
        f.cmds.push(Cmd::Refresh);
    }
    let edit = ui.button("⚙").on_hover_text("Edit this workspace: its paths or poll interval").clicked();
    if ui.button("📌").on_hover_text(RESET_BASE_HELP).clicked() {
        f.cmds.push(Cmd::ResetBase);
    }
    edit
}

fn terminal_menu(ui: &mut egui::Ui, tab: &mut Tab, show_terminal: &mut bool) {
    ui.menu_button(RichText::new("⌨").strong(), |ui| {
        let kinds = [
            (TermKind::Claude, "✳ Claude", format!("Claude in {}", tab.ws.remote)),
            (TermKind::Remote, "🖧 Remote shell", format!("A shell in {}", tab.ws.remote)),
            (TermKind::Local, "🖥 Local shell", format!("A shell in {}", tab.ws.local)),
        ];
        for (kind, text, help) in kinds {
            if ui.button(RichText::new(text).color(kind_color(kind, ui))).on_hover_text(help).clicked() {
                tab.terminals.show_kind(&mut tab.ws.sessions, kind);
                *show_terminal = true;
            }
        }
        ui.separator();
        let toggle = if *show_terminal { "Hide the terminals" } else { "Show the terminals" };
        if ui.button(toggle).on_hover_text("Hidden terminals keep running").clicked() {
            *show_terminal = !*show_terminal;
        }
    })
    .response
    .on_hover_text("Terminals at the bottom of the window: Claude, or a shell on the remote or local side");
}

const SHOW_WORK_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
const DISCREET_SPINNER_SIZE: f32 = 10.0;

fn working_a_while(ctx: &egui::Context, f: &Frame) -> bool {
    let id = egui::Id::new(("working-since", &f.state_dir));
    let working = f.busy || f.progress.is_some() || f.activity.is_some();
    if !working {
        ctx.data_mut(|d| d.remove::<std::time::Instant>(id));
        return false;
    }
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, std::time::Instant::now));
    let elapsed = since.elapsed();
    if elapsed < SHOW_WORK_AFTER {
        ctx.request_repaint_after(SHOW_WORK_AFTER - elapsed);
        return false;
    }
    true
}

fn discreet_spinner(ui: &mut egui::Ui) {
    let color = ui.visuals().weak_text_color();
    ui.add(egui::Spinner::new().size(DISCREET_SPINNER_SIZE).color(color));
}

fn progress_indicator(ui: &mut egui::Ui, f: &Frame) {
    if !f.working_a_while {
        return;
    }
    if f.busy {
        discreet_spinner(ui);
    }
    if let Some(p) = &f.progress {
        let fill = ui.visuals().weak_text_color().gamma_multiply(0.6);
        let hover = format!("{} · {:.0}%", p.step, p.fraction * 100.0);
        ui.add(egui::ProgressBar::new(p.fraction).desired_width(80.0).desired_height(3.0).fill(fill)).on_hover_text(hover);
    }
}

fn status_bar(ui: &mut egui::Ui, f: &Frame) {
    egui::Panel::bottom("status").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            match (&f.fetch_error, f.last_fetch) {
                (Some(e), _) => ui.colored_label(Color32::LIGHT_RED, format!("remote sync error: {e}")),
                (None, Some(t)) => ui.weak(format!("remote synced {}s ago · {} file(s) differ", t.elapsed().map(|d| d.as_secs()).unwrap_or(0), f.entries.len())),
                (None, None) => ui.weak("fetching the remote… (local files only until the first fetch is done)"),
            };
            if let Some(activity) = f.activity.as_ref().filter(|_| f.working_a_while) {
                ui.separator();
                discreet_spinner(ui);
                ui.label(RichText::new(activity).small().weak());
            }
            if let Some((is_err, m)) = &f.message {
                ui.separator();
                ui.colored_label(if *is_err { Color32::LIGHT_RED } else { Color32::LIGHT_GREEN }, m);
                if ui.small_button("📋").on_hover_text("Copy this message").clicked() {
                    ui.ctx().copy_text(m.clone());
                }
                if ui.small_button("×").on_hover_text("Clear this message").clicked() {
                    f.shared.lock().unwrap().message = None;
                }
            }
        });
        ui.weak(format!("state & backups of overwritten local files: {}", f.state_dir));
    });
}

fn terminal_area(ui: &mut egui::Ui, tab: &mut Tab, show_terminal: &mut bool) {
    if !*show_terminal {
        tab.terminals.maximized = false;
    }
    let (shown, maximized) = (*show_terminal, tab.terminals.maximized);
    let (terminal_id, bar_id, height) = (panel_id("terminal"), panel_id("terminal-bar"), tab.layout.terminal_height);
    let mut panel = |ui: &mut egui::Ui| {
        tab.panes.record(Pane::Terminal, ui.max_rect());
        terminal_panel(&mut tab.terminals, &mut tab.ws.sessions, &mut tab.ws.forks, (&tab.ws.local, &tab.ws.remote), ui, show_terminal);
    };
    match (shown, maximized) {
        (true, true) => {
            egui::CentralPanel::default().show(ui, |ui| panel(ui));
        }
        (true, false) => {
            let shown = egui::Panel::bottom(terminal_id).resizable(true).default_size(height).min_size(120.0).show(ui, |ui| panel(ui));
            tab.layout.terminal_height = shown.response.rect.height().round();
        }
        (false, _) => {
            egui::Panel::bottom(bar_id).resizable(false).show(ui, |ui| panel(ui));
        }
    }
}

fn focused_explorer(ctx: &egui::Context, tab: &Tab) -> Option<ExplorerKind> {
    ExplorerKind::ALL.into_iter().find(|&kind| ctx.memory(|m| m.has_focus(filter_id(tab, kind)) || m.has_focus(search_id(tab, kind))))
}

fn pane_with_keyboard(ctx: &egui::Context, tab: &Tab, in_terminal: bool) -> Pane {
    if in_terminal {
        Pane::Terminal
    } else if let Some(kind) = focused_explorer(ctx, tab) {
        kind.pane()
    } else if let Some(pane) = tab.panes.current.filter(|p| matches!(p, Pane::Files | Pane::Changes)) {
        pane
    } else {
        Pane::Main
    }
}

fn explorer_of(pane: Pane) -> Option<ExplorerKind> {
    ExplorerKind::ALL.into_iter().find(|kind| kind.pane() == pane)
}

fn selected_folder(selected: Option<&str>) -> String {
    selected.and_then(|path| path.rsplit_once('/')).map(|(folder, _)| format!("{folder}/")).unwrap_or_default()
}

fn clicked_folder_id(tab: &Tab) -> egui::Id {
    egui::Id::new(("clicked-folder", &tab.ws.local, &tab.ws.remote))
}

fn remember_clicked_folder(ctx: &egui::Context, tab: &Tab, folder: String) {
    ctx.data_mut(|d| d.insert_temp(clicked_folder_id(tab), (folder, tab.selected.clone())));
}

pub(super) fn new_file_folder(ctx: &egui::Context, tab: &Tab) -> String {
    match ctx.data(|d| d.get_temp::<(String, Option<String>)>(clicked_folder_id(tab))) {
        Some((folder, selected_then)) if selected_then == tab.selected => folder,
        _ => selected_folder(tab.selected.as_deref()),
    }
}

fn apply_shortcut(ctx: &egui::Context, tab: &mut Tab, f: &Frame, shortcut: Option<Shortcut>, terminal_focused: bool, show_terminal: &mut bool) {
    let Some(shortcut) = shortcut else { return };
    ctx.request_repaint();
    match shortcut {
        Shortcut::GoTo(pane) => {
            let editing = tab.edit.as_ref().map(|ed| egui::Id::new(("file-edit", &ed.path)));
            let focus = explorer_of(pane).map(|kind| filter_id(tab, kind)).or(editing);
            bring_pane(ctx, tab, pane, focus, show_terminal);
        }
        Shortcut::SearchInFiles => {
            let kind = explorer_of(pane_with_keyboard(ctx, tab, terminal_focused)).unwrap_or(ExplorerKind::Files);
            let id = search_id(tab, kind);
            bring_pane(ctx, tab, kind.pane(), Some(id), show_terminal);
        }
        Shortcut::Zoom => match pane_with_keyboard(ctx, tab, terminal_focused) {
            Pane::Terminal => tab.terminals.maximized = !tab.terminals.maximized,
            pane => tab.panes.toggle_zoom(pane),
        },
        Shortcut::Rename => {
            if let Some(e) = tab.selected.as_deref().and_then(|path| f.entry(path)) {
                tab.rename_file = Some((e.clone(), e.path.clone()));
            }
        }
        Shortcut::NewFile => tab.new_file = Some(NewFile::in_folder(new_file_folder(ctx, tab))),
        Shortcut::GoToDefinition => {
            let in_editor = tab.edit.as_ref().filter(|ed| ed.has_focus(ctx));
            let word = match in_editor {
                Some(ed) => ed.word_at_cursor(ctx),
                None => ctx.data(|d| d.get_temp::<Option<String>>(egui::Id::new(HOVERED_WORD_ID))).flatten(),
            };
            match word {
                Some(word) => look_for_definition(ctx, tab, f, word),
                None => f.report_error("Ctrl+B: put the cursor (in the editor) or the mouse (in the diff) on a name to go to its definition".into()),
            }
        }
        Shortcut::Back => go_back(tab),
        Shortcut::QuickOpen => tab.quick_open = Some(QuickOpen::open()),
        Shortcut::FindInFile => match &mut tab.find {
            Some(find) => find.refocus(),
            None => tab.find = tab.selected.clone().map(FindInFile::open),
        },
        Shortcut::NewTerminal => {
            ctx.memory_mut(|m| m.stop_text_input());
            tab.new_terminal = Some(tab.terminals.picker(&tab.ws.sessions));
        }
    }
}

fn look_for_definition(ctx: &egui::Context, tab: &mut Tab, f: &Frame, word: String) {
    let files = f.searchable();
    tab.definition.look_for(ctx, word, files);
}

fn current_place(ctx: &egui::Context, tab: &Tab) -> Option<Place> {
    match &tab.edit {
        Some(ed) => Some(Place { path: ed.path.clone(), line: ed.cursor_line(ctx) }),
        None => Some(Place { path: tab.selected.clone()?, line: None }),
    }
}

fn go_to_definition(ctx: &egui::Context, tab: &mut Tab, definition: &Definition) {
    if let Some(from) = current_place(ctx, tab) {
        tab.definition.go(from, definition);
    } else {
        tab.definition.jump = Some((definition.path.clone(), definition.line));
    }
    tab.file_tabs.open(definition.path.clone(), &mut tab.selected);
}

fn go_back(tab: &mut Tab) {
    let Some(place) = tab.definition.back.pop() else { return };
    tab.definition.jump = place.line.map(|line| (place.path.clone(), line));
    tab.selected = Some(place.path);
}

fn definition_results(ctx: &egui::Context, tab: &mut Tab, f: &Frame) {
    if let Some(word) = ctx.data_mut(|d| d.remove_temp::<Option<String>>(egui::Id::new(CLICKED_WORD_ID))).flatten() {
        look_for_definition(ctx, tab, f, word);
    }
    match tab.definition.poll() {
        Some(Found::One(definition)) => go_to_definition(ctx, tab, &definition),
        Some(Found::Nothing(name)) => f.report_error(format!("No definition of {name} found")),
        Some(Found::Several) | None => {}
    }
    if let Some(definition) = choices_window(ctx, &mut tab.definition.choices) {
        go_to_definition(ctx, tab, &definition);
    }
}

fn new_terminal_window(ctx: &egui::Context, tab: &mut Tab, show_terminal: &mut bool) {
    if picker_in_terminal_window(tab) {
        return;
    }
    let Some(picker) = &mut tab.new_terminal else { return };
    let outcome = picker.ui(ctx, &tab.ws.local, &tab.ws.remote);
    finish_new_terminal(ctx, tab, outcome, show_terminal);
}

fn picker_in_terminal_window(tab: &Tab) -> bool {
    tab.new_terminal.as_ref().is_some_and(|picker| picker.in_terminal_window) && is_detached(tab, Pane::Terminal)
}

fn new_terminal_in_window(ctx: &egui::Context, tab: &mut Tab, show_terminal: &mut bool) {
    if new_terminal_pressed(ctx) {
        ctx.memory_mut(|m| m.stop_text_input());
        let mut picker = tab.terminals.picker(&tab.ws.sessions);
        picker.in_terminal_window = true;
        tab.new_terminal = Some(picker);
    }
    if !picker_in_terminal_window(tab) {
        return;
    }
    let Some(picker) = &mut tab.new_terminal else { return };
    let outcome = match picker.keys(ctx) {
        PickerOutcome::Open => picker.ui(ctx, &tab.ws.local, &tab.ws.remote),
        outcome => outcome,
    };
    finish_new_terminal(ctx, tab, outcome, show_terminal);
}

fn finish_new_terminal(ctx: &egui::Context, tab: &mut Tab, outcome: PickerOutcome, show_terminal: &mut bool) {
    match outcome {
        PickerOutcome::Open => {}
        PickerOutcome::Closed => tab.new_terminal = None,
        PickerOutcome::Chosen(kind) => {
            tab.new_terminal = None;
            tab.terminals.open_new(&mut tab.ws.sessions, kind);
            bring_pane(ctx, tab, Pane::Terminal, None, show_terminal);
        }
    }
}

fn close_quick_open_on_escape(ctx: &egui::Context, tab: &mut Tab) {
    if tab.quick_open.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        tab.quick_open = None;
    }
}

fn quick_open_window(ctx: &egui::Context, tab: &mut Tab, f: &Frame) {
    let Some(quick) = &mut tab.quick_open else { return };
    let paths: Vec<String> = f.entries.iter().chain(f.gitignored_entries.iter()).chain(f.synced.iter()).map(|e| e.path.clone()).collect();
    let (close, picked) = quick.ui(ctx, &paths, || f.searchable());
    if close {
        tab.quick_open = None;
    }
    match picked {
        Some(Choice::File(path)) => {
            if let Some(from) = current_place(ctx, tab) {
                tab.definition.back.push(from);
            }
            tab.selected = Some(path);
        }
        Some(Choice::Line(hit)) => go_to_definition(ctx, tab, &Definition { path: hit.path, line: hit.line, text: hit.text }),
        None => {}
    }
}

fn bring_pane(ctx: &egui::Context, tab: &mut Tab, pane: Pane, focus: Option<egui::Id>, show_terminal: &mut bool) {
    if is_detached(tab, pane) {
        ctx.send_viewport_cmd_to(window_id(pane), egui::ViewportCommand::Focus);
    }
    go_to_pane(&mut tab.terminals, &mut tab.panes, pane, focus, show_terminal, ctx);
}

fn go_to_pane(terminals: &mut Terminals, panes: &mut Panes, pane: Pane, focus: Option<egui::Id>, show_terminal: &mut bool, ctx: &egui::Context) {
    if pane == Pane::Terminal {
        *show_terminal = true;
        terminals.focus_shown();
    } else {
        terminals.maximized = false;
        ctx.memory_mut(|m| match focus {
            Some(id) => m.request_focus(id),
            None => {
                if let Some(focused) = m.focused() {
                    m.surrender_focus(focused);
                }
            }
        });
    }
    panes.arrived(pane);
    ctx.request_repaint();
}

fn leave_editor_for_selection(tab: &mut Tab) {
    tab.file_tabs.follow_list(&tab.selected);
    if let Some(ed) = &mut tab.edit
        && tab.selected.as_deref() != Some(ed.path.as_str())
    {
        if ed.leave_for(tab.selected.clone()) {
            tab.edit = None;
        } else {
            tab.selected = Some(ed.path.clone());
        }
    }
}

const KEYSTORE_EXTENSIONS: &[&str] = &["p12", "pfx", "jks", "keystore"];
const SSH_KEY_NAMES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_ecdsa_sk", "id_ed25519_sk"];

fn holds_private_key(e: &FileEntry) -> bool {
    let name = e.path.rsplit('/').next().unwrap_or(&e.path);
    let extension = name.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
    if SSH_KEY_NAMES.contains(&name) || KEYSTORE_EXTENSIONS.contains(&extension.as_str()) {
        return true;
    }
    let Body::Text(texts) = &e.body else { return false };
    let TextDiff { local, remote, .. } = texts.diff();
    [local, remote].iter().any(|text| text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----"))
}

fn revealed_secret_id(path: &str) -> egui::Id {
    egui::Id::new(("revealed-secret", path))
}

fn secret_revealed(ctx: &egui::Context, path: &str) -> bool {
    ctx.data(|d| d.get_temp::<bool>(revealed_secret_id(path))).unwrap_or(false)
}

fn secret_spoiler(ui: &mut egui::Ui, path: &str) {
    ui.add_space(32.0);
    ui.vertical_centered(|ui| {
        ui.heading("🔒 Private key");
        ui.weak(format!("{path} holds a private key: its content stays hidden."));
        ui.add_space(8.0);
        if ui.button("👁 Show").clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(revealed_secret_id(path), true));
        }
    });
}

fn selected_entry(tab: &mut Tab, f: &Frame) -> Option<FileEntry> {
    let path = tab.selected.as_ref()?;
    f.entries.iter().find(|e| &e.path == path).cloned().or_else(|| {
        let e = f.gitignored_entries.iter().chain(f.synced.iter()).find(|e| &e.path == path)?;
        Some(unchanged_view(&mut tab.unchanged, e, &f.local))
    })
}

fn main_panel(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, highlighter: &Highlighter, syntax_colors: bool, full_view: &mut bool, show_terminal: &mut bool) {
    tab.panes.record(Pane::Main, ui.max_rect());
    let bar = file_tabs_bar(ui, &mut tab.file_tabs, &mut tab.selected);
    if bar.window {
        toggle_window(tab, Pane::Main);
    }
    let found = selected_entry(tab, f);
    if let Some(entry) = found.as_ref().filter(|e| holds_private_key(e)) {
        if !secret_revealed(ui.ctx(), &entry.path) {
            secret_spoiler(ui, &entry.path);
            return;
        }
        if ui.small_button("🔒 Hide").on_hover_text("Hide this private key again").clicked() {
            ui.ctx().data_mut(|d| d.remove::<bool>(revealed_secret_id(&entry.path)));
        }
    }
    if tab.source_of != tab.selected {
        tab.source_of = tab.selected.clone();
        tab.show_source = found.as_ref().is_some_and(|e| starts_on_source(e) || tab.explorers[tab.searched_in.index()].search.hit(&e.path).is_some());
    }
    let showing_preview = preview_switch(ui, tab, f, found.as_ref());
    run_bar(ui, tab, f, found.as_ref(), show_terminal);
    find_bar(ui, tab, found.as_ref());
    open_unchanged_in_editor(tab, f, found.as_ref(), showing_preview);
    let step_file = |tab: &mut Tab, forward: bool| tab.selected = step_in(&tab.order, tab.selected.as_ref(), if forward { 1 } else { -1 });
    if tab.edit.is_some() {
        if let Some(forward) = bar.step {
            step_file(tab, forward);
        }
        editor_area(ui, tab, f, found.as_ref(), highlighter, syntax_colors);
        return;
    }
    let Some(entry) = found.as_ref() else {
        if let Some(forward) = bar.step {
            step_file(tab, forward);
        }
        ui.centered_and_justified(|ui| ui.label(if tab.selected.is_some() { "This file is now in sync." } else { "Select a file to see its diff." }));
        return;
    };
    update_highlight(tab, entry, highlighter, syntax_colors, ui.ctx());
    diff_area(ui, tab, f, entry, (showing_preview, bar.step), full_view, show_terminal);
}

fn run_bar(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame, found: Option<&FileEntry>, show_terminal: &mut bool) {
    let Some(entry) = found else { return };
    let at = f.place();
    let Some(runner) = runner_for(&mut tab.runner, ui.ctx(), entry, &at) else { return };
    runner.ui(ui);
    ui.separator();
    let Some(request) = runner.run_request() else { return };
    let kind = if request.remote { TermKind::Remote } else { TermKind::Local };
    tab.terminals.run_in_shell(&mut tab.ws.sessions, kind, request.command);
    *show_terminal = true;
}

fn find_bar(ui: &mut egui::Ui, tab: &mut Tab, found: Option<&FileEntry>) {
    let Some(find) = &mut tab.find else { return };
    let (path, text) = match (&tab.edit, found) {
        (Some(ed), _) => (ed.path.clone(), ed.text()),
        (None, Some(e @ FileEntry { body: Body::Text(texts), .. })) => (e.path.clone(), texts.diff().local.as_str()),
        _ => return,
    };
    let action = find.bar(ui, &path, text);
    if let Some(query) = find.active_query() {
        let query: Arc<str> = Arc::from(query);
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(QUERY_ID), query));
    }
    match action {
        Some(FindAction::Close) => tab.find = None,
        Some(FindAction::Jump(line)) => match &mut tab.edit {
            Some(ed) => ed.show_line(line),
            None => tab.definition.jump = Some((path, line)),
        },
        None => {}
    }
}

fn preview_switch(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame, found: Option<&FileEntry>) -> bool {
    let previewable = found.is_some_and(|e| preview_for(&mut tab.preview, ui.ctx(), e, &f.place()).is_some());
    if previewable {
        let editing = tab.edit.as_ref().is_some_and(|ed| ed.dirty());
        source_switch(ui, &mut tab.show_source, editing);
    }
    let showing_preview = previewable && !tab.show_source;
    if showing_preview && tab.edit.as_ref().is_some_and(|ed| !ed.dirty()) {
        tab.edit = None;
    }
    showing_preview
}

fn open_unchanged_in_editor(tab: &mut Tab, f: &Frame, found: Option<&FileEntry>, showing_preview: bool) {
    if tab.edit.is_none()
        && !showing_preview
        && let Some(unchanged) = found.filter(|e| editable_unchanged(e))
        && let Ok(ed) = FileEdit::open(unchanged, &f.local, tab.explorers[tab.searched_in.index()].search.hit(&unchanged.path).map_or(0, |h| h.first_line()))
    {
        tab.edit = Some(ed.without_focus());
    }
}

fn editor_area(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, found: Option<&FileEntry>, highlighter: &Highlighter, syntax_colors: bool) {
    let Some(ed) = &mut tab.edit else { return };
    if let Some(e) = found {
        ed.follow(e);
    }
    if let Some(line) = tab.explorers[tab.searched_in.index()].search.target(&ed.path).or_else(|| tab.definition.take_jump(&ed.path)) {
        ed.show_line(line);
    }
    let current = tab
        .find
        .as_ref()
        .and_then(|find| find.current_line(&ed.path))
        .or_else(|| tab.explorers[tab.searched_in.index()].search.current_line(Some(&ed.path)))
        .or_else(|| tab.definition.highlighted_line(&ed.path));
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(CURRENT_LINE_ID), current));
    let mut results = Vec::new();
    if let Some(ticket) = ed.pending_ticket() {
        let done = f.shared.lock().unwrap().remote_saved.take_if(|(t, _)| *t == ticket);
        if let Some((_, outcome)) = done {
            results.push(ed.remote_saved(outcome));
        }
    }
    results.push(editor_ui(ui, ed, highlighter, syntax_colors));
    let side = ed.side();
    let mut leave = None;
    for r in results {
        if let Some(save) = r.remote_save {
            f.cmds.push(Cmd::SaveRemote(save));
        } else if r.saved && side == Side::Local {
            f.cmds.push(Cmd::LocalChanged);
        }
        if r.close {
            leave = Some(r.select);
        }
    }
    if let Some(select) = leave {
        tab.edit = None;
        if let Some(selected) = select {
            tab.selected = selected;
        }
    }
}

fn update_highlight(tab: &mut Tab, entry: &FileEntry, highlighter: &Highlighter, syntax_colors: bool, ctx: &egui::Context) {
    let Body::Text(texts) = &entry.body else { return };
    let TextDiff { local, remote, .. } = texts.diff();
    let key = (entry.path.clone(), entry.version);
    if let Some(done) = tab.hl_job.as_ref().and_then(|rx| rx.try_recv().ok()) {
        tab.hl_job = None;
        if done.key == key {
            tab.hl = Some(done);
        }
    }
    if tab.hl.as_ref().is_some_and(|h| h.key == key) {
        return;
    }
    tab.hl = Some(HlFile { local: highlighter.lines(&entry.path, local, false), remote: highlighter.lines(&entry.path, remote, false), key: key.clone() });
    tab.hl_job = syntax_colors.then(|| colors_in_background(highlighter, ctx, key, local.clone(), remote.clone()));
}

fn colors_in_background(highlighter: &Highlighter, ctx: &egui::Context, key: (String, u64), local: String, remote: String) -> Receiver<HlFile> {
    let (tx, rx) = mpsc::channel();
    let (h, ctx) = (highlighter.clone(), ctx.clone());
    std::thread::spawn(move || {
        let colored = HlFile { local: h.lines(&key.0, &local, true), remote: h.lines(&key.0, &remote, true), key };
        if tx.send(colored).is_ok() {
            ctx.request_repaint();
        }
    });
    rx
}

fn diff_area(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, entry: &FileEntry, (showing_preview, step): (bool, Option<bool>), full_view: &mut bool, show_terminal: &mut bool) {
    let hl = tab.hl.as_ref().filter(|h| h.key.0 == entry.path && h.key.1 == entry.version);
    let mut out = ViewOut::default();
    let at = f.place();
    let preview = if showing_preview {
        preview_for(&mut tab.preview, ui.ctx(), entry, &at)
    } else if matches!(entry.body, Body::Binary) {
        hex_for(&mut tab.hex, ui.ctx(), entry, &at)
    } else {
        None
    };
    let found_here = tab.explorers[tab.searched_in.index()].search.hit(&entry.path).is_some();
    if let Some(line) = tab.explorers[tab.searched_in.index()].search.target(&entry.path) {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(JUMP_ID), (line, JUMP_FRAMES)));
        tab.jumped_to = Some(entry.path.clone());
    }
    let current = tab
        .find
        .as_ref()
        .and_then(|find| find.current_line(&entry.path))
        .or_else(|| tab.explorers[tab.searched_in.index()].search.current_line(Some(&entry.path)))
        .or_else(|| tab.definition.highlighted_line(&entry.path));
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(CURRENT_LINE_ID), current));
    if let Some(line) = tab.definition.take_jump(&entry.path) {
        *full_view = true;
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(JUMP_ID), (line, JUMP_FRAMES)));
        tab.jumped_to = Some(entry.path.clone());
    }
    let mut whole_file = true;
    let full_view = if found_here { &mut whole_file } else { full_view };
    let busy = f.busy;
    let step_file = step.and_then(|forward| step_change(&mut tab.change, entry, forward));
    let shown = file_view(ui, entry, hl, busy, full_view, &mut tab.jumped_to, &mut tab.change, preview, &mut out);
    out.step_file = step_file;
    f.cmds.extend(shown);
    if (out.accept || accept_key_pressed(ui))
        && !busy
        && let Some(c) = tab.change.accept(entry)
    {
        f.cmds.push(c);
    }
    view_requests(tab, f, entry, out, show_terminal);
}

fn view_requests(tab: &mut Tab, f: &Frame, entry: &FileEntry, out: ViewOut, show_terminal: &mut bool) {
    if let Some(step) = out.step_file {
        tab.selected = step_in(&tab.order, tab.selected.as_ref(), step);
    }
    if let Some(explain) = out.explain {
        tab.terminals.paste(explain_prompt(entry, &explain));
        *show_terminal = true;
    }
    if let Some(line) = out.edit_remote_at {
        match FileEdit::open_remote(entry, &f.mirror, line) {
            Ok(ed) => tab.edit = Some(ed),
            Err(e) => f.report_error(e),
        }
    }
    if let Some(line) = out.edit_at {
        match FileEdit::open(entry, &f.local, line) {
            Ok(ed) => tab.edit = Some(ed),
            Err(e) => f.report_error(e),
        }
    }
}

pub(super) fn explain_prompt(e: &FileEntry, explain: &Explain) -> String {
    let renamed = e.rename.as_ref().map(|r| format!(", renamed from {}", r.from)).unwrap_or_default();
    let what = if explain.whole_file { "all the modifications" } else { "this modification" };
    format!("Explain {what} to {}{renamed} (- lines: my local version, + lines: the version in this remote workspace):\n\n```diff\n{}```\n", e.path, explain.patch)
}

fn bulk_confirmation(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame) {
    let Some((bulk, folder)) = &tab.confirm else { return };
    let bulk = *bulk;
    if bulk == Bulk::OverwriteRemote && folder.is_empty() {
        let id = egui::Id::new(("confirm-identical", &tab.ws.local, &tab.ws.remote));
        if let Some(yes) = confirm(ui, id, "Make the remote identical?", &identical_text(&tab.ws), "⚠ Replace the remote") {
            if yes {
                f.cmds.push(Cmd::MakeRemoteIdentical);
            }
            tab.confirm = None;
        }
        return;
    }
    match confirm_bulk(ui, bulk, folder, &f.entries, &tab.ws) {
        Some(true) => {
            let files = bulk.files(&f.entries, folder).map(|e| (e.path.clone(), e.version, bulk.action(), e.rename.clone())).collect();
            f.cmds.push(Cmd::Apply(files));
            tab.confirm = None;
        }
        Some(false) => tab.confirm = None,
        None => {}
    }
}

fn identical_text(ws: &Workspace) -> String {
    format!(
        "Everything in {} is deleted, except its .git folders, then every file git tracks in {} is sent there.\n\nUntracked and gitignored files are not sent, and their remote copies are gone too (uploads, .env, virtualenvs…). There is no backup.",
        ws.remote, ws.local
    )
}

fn delete_key_pressed(ui: &egui::Ui) -> bool {
    let free = ui.memory(|m| m.focused().is_none() && m.top_modal_layer().is_none());
    free && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
}

fn list_actions(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, acts: ListActions) {
    if let Some(e) = acts.delete {
        tab.confirm_delete = Some(e);
    }
    if let Some(path) = acts.unignore {
        f.cmds.push(Cmd::Unignore(path));
    }
    if let Some(path) = acts.delete_ignored {
        tab.confirm_delete_ignored = Some(path);
    }
    if delete_key_pressed(ui)
        && let Some(e) = tab.selected.as_deref().and_then(|path| f.entry(path))
    {
        if f.gitignored.contains(&e.path) {
            tab.confirm_delete_ignored = Some(e.path.clone());
        } else {
            tab.confirm_delete = Some(e.clone());
        }
    }
    if let Some(e) = acts.rename {
        let name = e.path.clone();
        tab.rename_file = Some((e, name));
    }
    if let Some(path) = acts.open_tab {
        tab.file_tabs.open(path, &mut tab.selected);
    }
    if let Some(bulk) = acts.bulk {
        tab.confirm = Some(bulk);
    }
    if let Some(folder) = acts.new_file_in {
        tab.new_file = Some(NewFile::in_folder(folder));
    }
    if let Some(path) = acts.gitignore {
        f.cmds.push(Cmd::AddToGitignore(path));
    }
    if let Some(folder) = acts.gitignored_toggled {
        show_gitignored_folder_now(&f.shared, Path::new(&f.local), &folder, &tab.ws.view.opened_gitignored);
        f.cmds.push(Cmd::OpenGitignored(tab.ws.view.opened_gitignored_list()));
    }
    if let Some(folder) = acts.delete_folder {
        tab.confirm_delete_folder = Some(folder);
    }
    if let Some(e) = acts.sync {
        f.cmds.push(Cmd::Sync { path: e.path, version: e.version, origin: e.origin, rename: e.rename });
    }
    if let Some(e) = acts.push_only {
        f.cmds.push(Cmd::Apply(vec![(e.path, e.version, Action::TakeLocalFile, e.rename)]));
    }
    if let Some(folder) = acts.folder_clicked {
        remember_clicked_folder(ui.ctx(), tab, folder);
    }
    if let Some((dragged, folder)) = acts.move_to {
        let name = dragged.path.rsplit('/').next().unwrap_or(&dragged.path).to_string();
        let to = format!("{folder}{name}");
        if to != dragged.path {
            if tab.selected.as_ref() == Some(&dragged.path) {
                tab.selected = Some(to.clone());
            }
            f.cmds.push(Cmd::Rename { path: dragged.path, version: dragged.version, rename: dragged.rename, to });
        }
    }
    let dropped = dropped_anywhere(ui);
    let import = acts.import.or_else(|| (!dropped.is_empty()).then(|| (new_file_folder(ui.ctx(), tab), dropped)));
    if let Some((folder, files)) = import {
        f.cmds.push(Cmd::ImportFiles { folder, files });
    }
    if let Some(folder) = acts.rename_folder {
        let name = folder.trim_end_matches('/').to_string();
        tab.rename_folder = Some((folder, name));
    }
    if let Some(folder) = acts.new_folder_in {
        tab.new_file = Some(NewFile::folder_in(folder));
    }
}

fn delete_ignored_dialog(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame) {
    let Some(path) = &tab.confirm_delete_ignored else { return };
    let id = egui::Id::new(("confirm-delete-ignored", &tab.ws.local, &tab.ws.remote));
    let text = format!("{path} is gitignored, so it only exists in {}: it is deleted there, for good (no backup). The remote is not touched.", tab.ws.local);
    let Some(yes) = confirm(ui, id, &format!("Delete {path}?"), &text, "🗑 Delete") else { return };
    if yes {
        f.cmds.push(Cmd::DeleteIgnored(path.clone()));
    }
    tab.confirm_delete_ignored = None;
}

fn rename_and_new_file_dialogs(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame) {
    delete_ignored_dialog(ui, tab, f);
    if let Some((e, name)) = &mut tab.rename_file
        && let Some(rename) = rename_dialog(ui, &e.path, name)
    {
        let to = name.trim().to_string();
        if rename && to != e.path {
            if tab.selected.as_ref() == Some(&e.path) {
                tab.selected = Some(to.clone());
            }
            f.cmds.push(Cmd::Rename { path: e.path.clone(), version: e.version, rename: e.rename.clone(), to });
        }
        tab.rename_file = None;
    }
    if let Some((folder, name)) = &mut tab.rename_folder
        && let Some(rename) = rename_dialog(ui, folder, name)
    {
        let to = name.trim().trim_end_matches('/').to_string();
        if rename && to != folder.trim_end_matches('/') {
            let files = files_in_folder(f, folder).into_iter().map(|e| (e.path.clone(), e.version, e.rename.clone())).collect();
            let new_prefix = format!("{to}/");
            if let Some(rest) = tab.selected.as_deref().and_then(|s| s.strip_prefix(folder.as_str())) {
                tab.selected = Some(format!("{new_prefix}{rest}"));
            }
            f.cmds.push(Cmd::RenameFolder { from: folder.clone(), to, files });
        }
        tab.rename_folder = None;
    }
    if let Some(new_file) = &mut tab.new_file
        && let Some(create) = new_file_dialog(ui, new_file)
    {
        if create && new_file.makes_folder {
            f.cmds.push(Cmd::CreateFolder(new_file.folder_path()));
        } else if create {
            let path = new_file.path();
            tab.selected = Some(path.clone());
            f.cmds.push(Cmd::CreateLocal { path, git_add: new_file.git_add });
        }
        tab.new_file = None;
    }
}

fn reveal_created_file(tab: &mut Tab, f: &mut Frame) {
    let Some(folders) = f.shared.lock().unwrap().open_gitignored_folders.take() else { return };
    let view = &mut tab.ws.view;
    let closed: Vec<String> = folders.into_iter().filter(|folder| !view.opened_gitignored.contains(folder)).collect();
    if !closed.is_empty() {
        view.opened_gitignored.extend(closed);
        f.cmds.push(Cmd::OpenGitignored(view.opened_gitignored_list()));
    }
}

fn commit_and_delete_dialogs(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame) {
    if let Some(dialog) = &mut tab.commit {
        match commit_dialog(ui, dialog, &f.local) {
            CommitOutcome::Open => {}
            CommitOutcome::Cancel => {
                dialog.remember_unticked(&mut tab.ws.skip_commit);
                tab.commit = None;
            }
            CommitOutcome::Commit(c) => {
                dialog.remember_unticked(&mut tab.ws.skip_commit);
                f.cmds.push(c);
                tab.commit = None;
            }
        }
    }
    if let Some(e) = &tab.confirm_delete
        && let Some(yes) = confirm(ui, egui::Id::new(("confirm-delete", &tab.ws.local, &tab.ws.remote)), &format!("Delete {}?", e.path), &delete_text(e, &tab.ws), "🗑 Delete")
    {
        if yes {
            f.cmds.push(Cmd::Apply(vec![(e.path.clone(), e.version, Action::Delete, e.rename.clone())]));
        }
        tab.confirm_delete = None;
    }
}

fn delete_folder_dialog(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame) {
    let ids = (&tab.ws.local, &tab.ws.remote);
    if let Some(folder) = &tab.confirm_delete_folder {
        let files: Vec<&FileEntry> = files_in_folder(f, folder);
        let text = delete_folder_text(folder, files.len(), &tab.ws);
        if let Some(yes) = confirm(ui, egui::Id::new(("confirm-delete-folder", ids)), &format!("Delete {folder}?"), &text, "🗑 Delete")
        {
            if yes && !files.is_empty() {
                f.cmds.push(Cmd::Apply(files.iter().map(|e| (e.path.clone(), e.version, Action::Delete, e.rename.clone())).collect()));
            }
            if yes {
                f.cmds.push(Cmd::RemoveEmptyFolder(folder.clone()));
            }
            tab.confirm_delete_folder = None;
        }
    }
}

fn files_in_folder<'a>(f: &'a Frame, folder: &str) -> Vec<&'a FileEntry> {
    f.entries.iter().chain(f.synced.iter()).filter(|e| e.path.starts_with(folder) && !f.gitignored.contains(&e.path)).collect()
}

fn delete_folder_text(folder: &str, files: usize, ws: &Workspace) -> String {
    if files == 0 {
        return format!("{folder} lists no file: it is removed from {} if it holds none (only empty folders).", ws.local);
    }
    format!(
        "The {files} file(s) listed in {folder} will be deleted from {}, from git and from {}.\n\nFiles the list does not show (untracked or gitignored ones, when hidden) stay. Every local copy is backed up in the state folder, and the remote one too when it differs. A file changed since you looked at it is not deleted.",
        ws.local, ws.remote
    )
}
