use super::*;

/// A workspace pair, as saved in the settings.
#[derive(Clone, PartialEq)]
pub(super) struct Workspace {
    pub(super) local: String,
    pub(super) remote: String,
    pub(super) interval: f64,
    /// The terminal tabs. A Claude tab's ID is its conversation, resumed on reconnect.
    pub(super) sessions: Vec<TermTab>,
    pub(super) forks: Vec<Fork>,
    pub(super) skip_commit: Vec<String>,
    pub(super) view: ListView,
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Layout {
    pub(super) terminal_shown: bool,
    pub(super) terminal_maximized: bool,
    pub(super) zoomed: Option<Pane>,
    pub(super) files_width: f32,
    pub(super) changes_width: f32,
    pub(super) terminal_height: f32,
    pub(super) detached: Vec<Pane>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { terminal_shown: true, terminal_maximized: false, zoomed: None, files_width: 340.0, changes_width: 320.0, terminal_height: 320.0, detached: Vec::new() }
    }
}

const PANE_LETTERS: [(char, Pane); 4] = [('f', Pane::Files), ('c', Pane::Changes), ('m', Pane::Main), ('t', Pane::Terminal)];

const ZOOMABLE: [(&str, Pane); 3] = [("f", Pane::Files), ("c", Pane::Changes), ("m", Pane::Main)];

impl Layout {
    pub(super) fn to_text(&self) -> String {
        let zoomed = ZOOMABLE.iter().find(|(_, p)| Some(*p) == self.zoomed).map_or("-", |(c, _)| c);
        let detached: String = self.detached.iter().filter_map(|pane| PANE_LETTERS.iter().find(|(_, p)| p == pane).map(|(c, _)| *c)).collect();
        format!(
            "s{},m{},z{zoomed},w{},c{},h{},d{detached}",
            self.terminal_shown as u8,
            self.terminal_maximized as u8,
            self.files_width.round(),
            self.changes_width.round(),
            self.terminal_height.round()
        )
    }

    pub(super) fn parse(text: &str, terminal_shown_before: bool) -> Layout {
        let mut layout = Layout { terminal_shown: terminal_shown_before, ..Layout::default() };
        for part in text.split(',') {
            let (key, value) = part.split_at(part.len().min(1));
            let size = value.parse::<f32>().ok().filter(|v| (60.0..=4000.0).contains(v));
            match key {
                "s" => layout.terminal_shown = value == "1",
                "m" => layout.terminal_maximized = value == "1",
                "z" => layout.zoomed = ZOOMABLE.iter().find(|(c, _)| *c == value).map(|(_, p)| *p),
                "w" => layout.files_width = size.unwrap_or(layout.files_width),
                "c" => layout.changes_width = size.unwrap_or(layout.changes_width),
                "h" => layout.terminal_height = size.unwrap_or(layout.terminal_height),
                "d" => layout.detached = value.chars().filter_map(|c| PANE_LETTERS.iter().find(|(l, _)| *l == c).map(|(_, p)| *p)).collect(),
                _ => {}
            }
        }
        layout
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct ListView {
    pub(super) tree: bool,
    pub(super) changes_tree: bool,
    pub(super) expand: bool,
    pub(super) changes_expand: bool,
    pub(super) flipped: std::collections::BTreeSet<String>,
    pub(super) opened_gitignored: std::collections::BTreeSet<String>,
}

impl Default for ListView {
    fn default() -> Self {
        ListView {
            tree: true,
            changes_tree: false,
            expand: false,
            changes_expand: false,
            flipped: Default::default(),
            opened_gitignored: Default::default(),
        }
    }
}

const GROUP_KEY: &str = "group:";
pub(super) const CHANGES_KEY: &str = "changes:";

impl ListView {
    pub(super) fn is_open(&self, key: &str) -> bool {
        let default = if key.starts_with(GROUP_KEY) {
            true
        } else if key.starts_with(CHANGES_KEY) {
            self.changes_expand
        } else {
            self.expand
        };
        default != self.flipped.contains(key)
    }

    pub(super) fn toggle(&mut self, key: &str) {
        if !self.flipped.remove(key) {
            self.flipped.insert(key.to_string());
        }
    }

    pub(super) fn set_expand(&mut self, expand: bool) {
        self.expand = expand;
        self.flipped.retain(|key| key.starts_with(GROUP_KEY) || key.starts_with(CHANGES_KEY));
    }

    pub(super) fn set_changes_expand(&mut self, expand: bool) {
        self.changes_expand = expand;
        self.flipped.retain(|key| !key.starts_with(CHANGES_KEY));
    }

    pub(super) fn group_key(title: &str) -> String {
        format!("{GROUP_KEY}{title}")
    }

    pub(super) fn flags(&self) -> String {
        [(self.tree, 't'), (self.changes_tree, 'T'), (self.expand, 'e'), (self.changes_expand, 'E')].iter().filter(|(on, _)| *on).map(|(_, c)| *c).collect::<String>() + "v"
    }

    pub(super) fn parse(flags: &str, flipped: &str, fallback: &ListView) -> ListView {
        if flags.is_empty() {
            return fallback.clone();
        }
        ListView {
            tree: flags.contains('t'),
            changes_tree: flags.contains('T'),
            expand: flags.contains('e'),
            changes_expand: flags.contains('E'),
            flipped: parse_repo_list(flipped).into_iter().collect(),
            opened_gitignored: Default::default(),
        }
    }

    pub(super) fn flipped_text(&self) -> String {
        repo_list_to_string(&self.flipped.iter().cloned().collect::<Vec<_>>())
    }

    pub(super) fn opened_gitignored_list(&self) -> Vec<String> {
        self.opened_gitignored.iter().cloned().collect()
    }

    pub(super) fn toggle_opened_gitignored(&mut self, folder: &str) {
        if !self.opened_gitignored.remove(folder) {
            self.opened_gitignored.insert(folder.to_string());
        }
    }
}

pub(super) fn parse_repo_list(text: &str) -> Vec<String> {
    text.split(',').filter(|r| !r.is_empty()).map(|r| if r == "." { String::new() } else { unescape(r) }).collect()
}

pub(super) fn repo_list_to_string(repos: &[String]) -> String {
    repos.iter().map(|r| if r.is_empty() { ".".to_string() } else { escape(r) }).collect::<Vec<_>>().join(",")
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum TermKind {
    Claude,
    /// A shell in the remote workspace.
    Remote,
    /// A shell in the local workspace.
    Local,
}

impl TermKind {
    /// Prefix of the tab in the settings; none for Claude, as before shells.
    fn prefix(self) -> &'static str {
        match self {
            TermKind::Claude => "",
            TermKind::Remote => "remote/",
            TermKind::Local => "local/",
        }
    }
}

#[derive(Clone, PartialEq)]
pub(super) struct TermTab {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) kind: TermKind,
}

impl TermTab {
    pub(super) fn new(kind: TermKind) -> Self {
        TermTab { id: new_session_id(), name: String::new(), kind }
    }

    pub(super) fn parse_list(s: &str) -> Vec<Self> {
        s.split(',')
            .filter_map(|c| {
                let (kind, c) = [TermKind::Remote, TermKind::Local]
                    .into_iter()
                    .find_map(|k| c.strip_prefix(k.prefix()).map(|c| (k, c)))
                    .unwrap_or((TermKind::Claude, c));
                let (id, name) = c.split_once(':').unwrap_or((c, ""));
                is_session_id(id).then(|| TermTab { id: id.to_string(), name: unescape(name), kind })
            })
            .collect()
    }

    pub(super) fn list_to_string(list: &[Self]) -> String {
        let one = |c: &Self| {
            let name = if c.name.is_empty() { String::new() } else { format!(":{}", escape(&c.name)) };
            format!("{}{}{name}", c.kind.prefix(), c.id)
        };
        list.iter().map(one).collect::<Vec<_>>().join(",")
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Fork {
    pub(super) child: String,
    pub(super) parent: String,
}

impl Fork {
    pub(super) fn parse_list(s: &str) -> Vec<Self> {
        s.split(',')
            .filter_map(|edge| edge.split_once('>'))
            .filter(|(child, parent)| is_session_id(child) && is_session_id(parent))
            .map(|(child, parent)| Fork { child: child.to_string(), parent: parent.to_string() })
            .collect()
    }

    pub(super) fn list_to_string(list: &[Self]) -> String {
        list.iter().map(|f| format!("{}>{}", f.child, f.parent)).collect::<Vec<_>>().join(",")
    }

    pub(super) fn parent_of<'a>(list: &'a [Self], child: &str) -> Option<&'a str> {
        list.iter().find(|f| f.child == child).map(|f| f.parent.as_str())
    }
}

fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '%' | ',' | '\t' | '\n' | '\r' => format!("%{:02X}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        match rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) if b.is_ascii() => {
                out.push(b as char);
                rest = &rest[i + 3..];
            }
            _ => {
                out.push('%');
                rest = &rest[i + 1..];
            }
        }
    }
    out + rest
}

impl Workspace {
    /// Short tab title: local folder name, and the remote host if any.
    pub(super) fn title(&self) -> String {
        let name = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string();
        match self.remote.split_once(':') {
            Some((host, _)) if !host.contains('/') => format!("{} ⇄ {host}", name(&self.local)),
            _ => format!("{} ⇄ {}", name(&self.local), name(&self.remote)),
        }
    }
}

/// One open workspace: its sync session and what the user is looking at.
pub(super) struct Tab {
    pub(super) ws: Workspace,
    pub(super) session: Option<Session>,
    /// Why the session could not start (missing folder, not a git repo…).
    pub(super) error: Option<String>,
    pub(super) selected: Option<String>,
    /// File list order of the previous frame, to pick the next file.
    pub(super) order: Vec<String>,
    /// The file the full-file view last jumped to its first change for.
    pub(super) jumped_to: Option<String>,
    pub(super) hl: Option<HlFile>,
    /// Background highlighting of the selected file, replacing `hl` when done.
    pub(super) hl_job: Option<Receiver<HlFile>>,
    /// The "make remote identical to local" confirmation is open.
    /// A bulk action waiting for confirmation.
    pub(super) confirm: Option<(Bulk, String)>,
    /// The "edit workspace" dialog, when open.
    pub(super) editing: Option<EditForm>,
    /// The selected unchanged file, read from disk for display.
    pub(super) unchanged: Option<FileEntry>,
    /// The Claude terminals, started when first shown.
    pub(super) terminals: Terminals,
    /// The file whose delete confirmation is open.
    pub(super) confirm_delete: Option<FileEntry>,
    /// The local file being edited, shown instead of its diff.
    pub(super) edit: Option<FileEdit>,
    pub(super) change: ChangeFocus,
    pub(super) explorers: [Explorer; 2],
    pub(super) searched_in: ExplorerKind,
    pub(super) commit: Option<CommitDialog>,
    pub(super) preview: Option<Box<dyn Preview>>,
    pub(super) hex: Option<Box<dyn Preview>>,
    pub(super) runner: Option<Box<dyn Preview>>,
    pub(super) git_log: GitLog,
    pub(super) definition: GoToDefinition,
    pub(super) quick_open: Option<QuickOpen>,
    pub(super) new_terminal: Option<TerminalPicker>,
    pub(super) find: Option<FindInFile>,
    pub(super) file_tabs: FileTabs,
    pub(super) show_source: bool,
    pub(super) source_of: Option<String>,
    /// The "new file" dialog, when open: the folder, and the name typed.
    pub(super) new_file: Option<NewFile>,
    pub(super) rename_file: Option<(FileEntry, String)>,
    pub(super) confirm_delete_folder: Option<String>,
    pub(super) rename_folder: Option<(String, String)>,
    pub(super) panes: Panes,
    pub(super) layout: Layout,
    /// Narrowest width of the file list panel, so that its toolbar fits.
    pub(super) explorer_min_widths: [f32; 2],
}

#[derive(Default)]
pub(super) struct Explorer {
    pub(super) filter: String,
    pub(super) filter_collapsed: std::collections::BTreeSet<String>,
    pub(super) search: ContentSearch,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum ExplorerKind {
    Files,
    Changes,
}

impl ExplorerKind {
    pub(super) const ALL: [ExplorerKind; 2] = [ExplorerKind::Files, ExplorerKind::Changes];

    pub(super) fn index(self) -> usize {
        self as usize
    }

    pub(super) fn pane(self) -> Pane {
        match self {
            ExplorerKind::Files => Pane::Files,
            ExplorerKind::Changes => Pane::Changes,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            ExplorerKind::Files => "files",
            ExplorerKind::Changes => "changes",
        }
    }
}

pub(super) struct EditForm {
    pub(super) local: String,
    pub(super) remote: String,
    pub(super) interval: f64,
    pub(super) error: Option<String>,
    pub(super) asking_reset: bool,
}

impl Tab {
    pub(super) fn open_editor(&mut self) {
        self.editing = Some(EditForm {
            local: self.ws.local.clone(),
            remote: self.ws.remote.clone(),
            interval: self.ws.interval,
            error: None,
            asking_reset: false,
        });
    }
}

impl Tab {
    pub(super) fn open(ws: Workspace, ctx: &egui::Context) -> Self {
        let mut tab = Tab { ws, session: None, error: None, selected: None, order: Vec::new(), jumped_to: None, hl: None, hl_job: None, confirm: None, editing: None, unchanged: None, terminals: Terminals::default(), explorer_min_widths: [0.0; 2], confirm_delete: None, new_file: None, rename_file: None, confirm_delete_folder: None, rename_folder: None, edit: None, change: ChangeFocus::default(), explorers: Default::default(), searched_in: ExplorerKind::Files, commit: None, preview: None, hex: None, runner: None, git_log: GitLog::default(), definition: GoToDefinition::default(), quick_open: None, new_terminal: None, find: None, file_tabs: FileTabs::default(), show_source: false, source_of: None, panes: Panes::default(), layout: Layout::default() };
        tab.start(ctx);
        tab
    }

    pub(super) fn start(&mut self, ctx: &egui::Context) {
        // The remote may have changed: the terminals restart there when shown.
        self.terminals = Terminals::default();
        match start_workspace_session(&self.ws, ctx) {
            Ok(session) => {
                self.error = None;
                self.session = Some(session);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub(super) fn take_failed_session(&mut self) {
        let failed = self.session.as_ref().and_then(|s| s.shared.lock().unwrap().failed.take());
        if let Some(e) = failed {
            self.session = None;
            self.error = Some(e);
        }
    }

    /// Number of files that differ, if the session is running.
    pub(super) fn running_action(&self) -> Option<String> {
        let shared = self.session.as_ref()?.shared.lock().unwrap();
        shared.busy.then(|| match &shared.progress {
            Some(p) => format!("{} · {:.0}%", p.step, p.fraction * 100.0),
            None => "an action".to_string(),
        })
    }

    pub(super) fn pending(&self) -> Option<usize> {
        Some(self.session.as_ref()?.shared.lock().unwrap().entries.len())
    }
}

pub(super) fn start_workspace_session(ws: &Workspace, ctx: &egui::Context) -> anyhow::Result<Session> {
    let remote = Remote::parse(&ws.remote)?;
    let view = ws.view.clone();
    let configure = move |engine: &Engine| {
        engine.set_include_untracked(true);
        engine.set_include_gitignored(true);
        engine.set_opened_gitignored(view.opened_gitignored_list());
    };
    start_session(&ws.local, remote, configure, Duration::from_secs_f64(ws.interval.max(0.5)), ctx)
}

/// An unchanged file prepared for display (its content read from the local
/// copy, which is identical to the remote), cached per version.
pub(super) fn unchanged_view(cache: &mut Option<FileEntry>, e: &FileEntry, local_root: &str) -> FileEntry {
    if let Some(c) = cache
        && c.path == e.path
        && c.version == e.version
    {
        return c.clone();
    }
    let file = std::path::Path::new(local_root).join(&e.path);
    let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= MAX_SHOWN_TEXT as u64);
    let bytes = if small { std::fs::read(&file).unwrap_or_default() } else { Vec::new() };
    let body = match String::from_utf8(bytes) {
        Ok(text) if small && !text.contains('\0') && text.len() <= MAX_SHOWN_TEXT => {
            Body::Text(Texts::new(text.clone(), text.clone()))
        }
        _ => Body::Binary,
    };
    let view = FileEntry { body, ..e.clone() };
    *cache = Some(view.clone());
    view
}

/// A saved workspace whose session could not start.
pub(super) fn failed_tab_ui(tab: &mut Tab, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading(tab.ws.title());
        ui.monospace(format!("local:  {}\nremote: {}", tab.ws.local, tab.ws.remote));
        ui.add_space(8.0);
        ui.colored_label(Color32::LIGHT_RED, tab.error.as_deref().unwrap_or("not started"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("⟳ Retry").clicked() {
                tab.start(ui.ctx());
            }
            if ui.button("⚙ Edit workspace").clicked() {
                tab.open_editor();
            }
        });
    });
}

const MAX_SHOWN_TEXT: usize = 4 * 1024 * 1024;

pub(super) struct NewFile {
    pub(super) folder: String,
    pub(super) name: String,
    pub(super) git_add: bool,
    pub(super) makes_folder: bool,
}

impl NewFile {
    pub(super) fn in_folder(folder: String) -> Self {
        NewFile { folder, name: String::new(), git_add: true, makes_folder: false }
    }

    pub(super) fn folder_in(parent: String) -> Self {
        NewFile { makes_folder: true, ..NewFile::in_folder(parent) }
    }

    pub(super) fn folder_path(&self) -> String {
        format!("{}{}", self.folder, self.name.trim().trim_matches('/'))
    }

    pub(super) fn path(&self) -> String {
        format!("{}{}", self.folder, self.name.trim())
    }
}

pub(super) fn new_file_dialog(ui: &egui::Ui, new_file: &mut NewFile) -> Option<bool> {
    let NewFile { folder, name, git_add, makes_folder } = new_file;
    let makes_folder = *makes_folder;
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("new-file")).show(ui.ctx(), |ui| {
        ui.set_width(420.0);
        ui.heading(if makes_folder { "New folder" } else { "New file" });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.monospace(folder);
            let edit = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY).hint_text(if makes_folder { "folder" } else { "name.rs" }));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
                answer = Some(true);
            }
        });
        ui.add_space(6.0);
        if makes_folder {
            ui.weak("Created empty in your local workspace. Git keeps no empty folder: it is synced once it holds a file.");
        } else {
            new_file_sync_choice(ui, git_add);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let create = if makes_folder { "📁 Create" } else { "📄 Create" };
            if ui.add_enabled(!name.trim().is_empty(), egui::Button::new(create)).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}

fn new_file_sync_choice(ui: &mut egui::Ui, git_add: &mut bool) {
    ui.checkbox(git_add, "⇄ Sync it: git add it, so it is pushed to the remote");
    ui.weak(if *git_add {
        "Created empty in your local workspace and git added, so it is listed as your change, to push when ready."
    } else {
        "Created empty in your local workspace, not added to git: it is listed with ❓ (untracked files, turned on). Right-click → ⇄ Sync to git add it and push it."
    });
}

pub(super) fn rename_dialog(ui: &egui::Ui, path: &str, name: &mut String) -> Option<bool> {
    let mut answer = None;
    let valid = !name.trim().is_empty() && !name.trim().ends_with('/');
    let modal = egui::Modal::new(egui::Id::new("rename-file")).show(ui.ctx(), |ui| {
        ui.set_width(480.0);
        ui.heading(format!("Rename {path}"));
        ui.add_space(6.0);
        let edit = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        if !edit.has_focus() && !edit.lost_focus() {
            edit.request_focus();
        }
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid {
            answer = Some(true);
        }
        ui.add_space(4.0);
        ui.weak("The path from the workspace root. Moved locally (git rm + git add) and on the remote, so it stays in sync. A file changed since you looked at it is not renamed.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(valid, egui::Button::new("✏ Rename")).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}

#[derive(Default)]
pub(super) struct FileTabs {
    pub(super) paths: Vec<String>,
    pub(super) pinned: Option<usize>,
    main_selected: Option<String>,
}

impl FileTabs {
    pub(super) fn open(&mut self, path: String, selected: &mut Option<String>) {
        let index = match self.paths.iter().position(|p| *p == path) {
            Some(index) => index,
            None => {
                self.paths.push(path);
                self.paths.len() - 1
            }
        };
        self.show(Some(index), selected);
    }

    pub(super) fn show(&mut self, index: Option<usize>, selected: &mut Option<String>) {
        if self.pinned.is_none() {
            self.main_selected = selected.clone();
        }
        *selected = match index {
            Some(i) => Some(self.paths[i].clone()),
            None => self.main_selected.clone(),
        };
        self.pinned = index;
    }

    pub(super) fn close(&mut self, index: usize, selected: &mut Option<String>) {
        match self.pinned {
            Some(shown) if shown == index => self.show(None, selected),
            Some(shown) if shown > index => self.pinned = Some(shown - 1),
            _ => {}
        }
        self.paths.remove(index);
    }

    pub(super) fn follow_list(&mut self, selected: &Option<String>) {
        if let Some(index) = self.pinned
            && selected.as_ref() != self.paths.get(index)
        {
            self.pinned = None;
        }
    }

    fn main_file<'a>(&'a self, selected: &'a Option<String>) -> Option<&'a String> {
        if self.pinned.is_none() { selected.as_ref() } else { self.main_selected.as_ref() }
    }
}

pub(super) struct TabsBar {
    pub(super) window: bool,
    pub(super) step: Option<bool>,
}

pub(super) fn file_tabs_bar(ui: &mut egui::Ui, tabs: &mut FileTabs, selected: &mut Option<String>) -> TabsBar {
    let mut show = None;
    let mut close = None;
    let mut bar = TabsBar { window: false, step: None };
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("⏶").on_hover_text("Previous change (before the first one: the previous file)").clicked() {
            bar.step = Some(false);
        }
        if ui.small_button("⏷").on_hover_text("Next change (after the last one: the next file)").clicked() {
            bar.step = Some(true);
        }
        bar.window = ui.small_button("⧉").on_hover_text(WINDOW_HELP).clicked();
        let main_path = tabs.main_file(selected).cloned();
        let main = main_path.as_deref().map_or("no file", file_name).to_string();
        let shown = closable_tab(ui, tabs.pinned.is_none(), |ui| tab_label(ui, format!("≡ {main}")), None);
        let hover = main_path.map_or("Follows the file list and F7".to_string(), |p| format!("{p}\nfollows the file list and F7"));
        if shown.response.on_hover_text(hover).clicked() {
            show = Some(None);
        }
        for (i, path) in tabs.paths.iter().enumerate() {
            let shown = closable_tab(ui, tabs.pinned == Some(i), |ui| tab_label(ui, format!("📌 {}", file_name(path))), Some("Close this tab"));
            if shown.close {
                close = Some(i);
            } else if shown.response.on_hover_text(format!("{path}\nkeeps this file, even with F7")).clicked() {
                show = Some(Some(i));
            }
        }
    });
    ui.separator();
    if let Some(index) = close {
        tabs.close(index, selected);
    } else if let Some(index) = show {
        tabs.show(index, selected);
    }
    bar
}

pub(super) fn search_files<'a>(files: impl Iterator<Item = &'a FileEntry>, local_root: &str, mirror: &Path) -> Vec<(String, PathBuf)> {
    files
        .map(|e| {
            let (local_path, remote_path) = e.rename.as_ref().map_or((e.path.as_str(), e.path.as_str()), |r| (r.local_path.as_str(), r.remote_path.as_str()));
            let file = if e.local_exists { Path::new(local_root).join(local_path) } else { mirror.join(remote_path) };
            (e.path.clone(), file)
        })
        .collect()
}

pub(super) fn source_switch(ui: &mut egui::Ui, show_source: &mut bool, editing: bool) {
    ui.horizontal(|ui| {
        let preview = ui.add_enabled(!editing, egui::Button::selectable(!*show_source, "👁 Preview"));
        if preview.on_disabled_hover_text("Save or cancel your edit first").clicked() {
            *show_source = false;
        }
        if ui.selectable_label(*show_source, "</> Source").on_hover_text("The file itself: its diff, its text, or its bytes").clicked() {
            *show_source = true;
        }
    });
}

pub(super) fn editable_unchanged(e: &FileEntry) -> bool {
    e.origin == Origin::Synced && e.local_exists && matches!(e.body, Body::Text(_))
}

/// What deleting `e` does, for its confirmation.
pub(super) fn delete_text(e: &FileEntry, ws: &Workspace) -> String {
    let paths = match &e.rename {
        Some(r) if r.local_path != r.remote_path => format!("{} (local) and {} (remote)", r.local_path, r.remote_path),
        _ => e.path.clone(),
    };
    let mut sides = Vec::new();
    if e.local_exists {
        sides.push(format!("deleted from {} and from git", ws.local));
    } else {
        sides.push("removed from git".to_string());
    }
    if e.remote_exists {
        sides.push(format!("deleted from {}", ws.remote));
    }
    format!(
        "{paths} will be {}.\n\nThe local copy is backed up in the state folder, and the remote one too when it differs. A file changed since you looked at it is not deleted.",
        sides.join(", and ")
    )
}

/// Actions on many files at once, each confirmed in a dialog first.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Bulk {
    /// Take the remote version of every file only Claude changed.
    PullClaude,
    /// Push every file only you changed.
    PushMine,
    /// Push your version of every file that differs.
    OverwriteRemote,
}

impl Bulk {
    pub(super) fn files<'a>(self, entries: &'a [FileEntry], folder: &'a str) -> impl Iterator<Item = &'a FileEntry> {
        entries.iter().filter(move |e| {
            let ours = match self {
                Bulk::PullClaude => e.origin == Origin::Remote,
                Bulk::PushMine => e.origin == Origin::Local,
                Bulk::OverwriteRemote => true,
            };
            ours && e.path.starts_with(folder)
        })
    }

    pub(super) fn action(self) -> Action {
        match self {
            Bulk::PullClaude => Action::TakeRemoteFile,
            Bulk::PushMine | Bulk::OverwriteRemote => Action::TakeLocalFile,
        }
    }
}

fn changed_on_both_sides(bulk: Bulk, entries: &[FileEntry], folder: &str) -> usize {
    if bulk == Bulk::OverwriteRemote {
        return 0;
    }
    entries.iter().filter(|e| e.origin == Origin::Both && e.path.starts_with(folder)).count()
}

/// What the action will do to one file, in words.
fn effect(e: &FileEntry, action: Action) -> &'static str {
    let pull = matches!(action, Action::TakeRemoteFile);
    match (e.rename.is_some(), e.local_exists, e.remote_exists, pull) {
        (true, _, _, true) => "moved locally",
        (true, _, _, false) => "moved on the remote",
        (_, true, true, true) => "overwritten locally",
        (_, false, true, true) => "created locally",
        (_, _, false, true) => "deleted locally",
        (_, true, true, false) => "overwritten on the remote",
        (_, true, false, false) => "created on the remote",
        (_, false, _, false) => "deleted on the remote",
    }
}

/// The confirmation dialog of a bulk action: what will happen to which file.
/// Returns `Some(true)` to go ahead, `Some(false)` to cancel.
pub(super) fn confirm_bulk(ui: &mut egui::Ui, bulk: Bulk, folder: &str, entries: &[FileEntry], ws: &Workspace) -> Option<bool> {
    let files: Vec<&FileEntry> = bulk.files(entries, folder).collect();
    let action = bulk.action();
    let (title, intro, warning, button, fill) = match bulk {
        Bulk::PullClaude => (
            "Pull all Claude changes?",
            format!("The remote version of {} file(s) changed only by Claude will be taken into {}:", files.len(), ws.local),
            "Overwritten or deleted local files are backed up in the state folder. New files are git added, deletions staged.",
            "⬇ Pull all",
            Color32::from_rgb(0x2e, 0x8b, 0x46),
        ),
        Bulk::PushMine => (
            "Push all my changes?",
            format!("Your version of {} file(s) changed only locally will be pushed to {}:", files.len(), ws.remote),
            "Claude did not change these files, so none of its work is overwritten.",
            "⬆ Push all",
            Color32::from_rgb(0xc0, 0x39, 0x2b),
        ),
        Bulk::OverwriteRemote => (
            "Make remote identical to local?",
            format!("Your local version of {} file(s) will be pushed to {}:", files.len(), ws.remote),
            "Claude's changes to these files on the remote will be lost.",
            "⬆ Overwrite remote",
            Color32::from_rgb(0xc0, 0x39, 0x2b),
        ),
    };

    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new(("confirm-bulk", &ws.local, &ws.remote))).show(ui.ctx(), |ui| {
        ui.set_width(520.0);
        ui.heading(title);
        if !folder.is_empty() {
            ui.label(RichText::new(format!("in {folder}")).monospace().strong());
        }
        ui.add_space(6.0);
        ui.label(intro);
        let skipped = changed_on_both_sides(bulk, entries, folder);
        if skipped > 0 {
            ui.colored_label(
                Color32::from_rgb(0xe0, 0xa0, 0x40),
                format!("{skipped} file(s) changed on both sides are left out: review them, or use ⚠ Overwrite remote to push your version anyway."),
            );
        }
        // Summary per effect, then the files themselves.
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for e in &files {
            let what = effect(e, action);
            match counts.iter_mut().find(|(w, _)| *w == what) {
                Some((_, n)) => *n += 1,
                None => counts.push((what, 1)),
            }
        }
        for (what, n) in counts {
            ui.label(format!("  • {n} {what}"));
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
            for e in &files {
                ui.horizontal(|ui| {
                    ui.monospace(&e.path);
                    ui.weak(effect(e, action));
                });
            }
        });
        ui.add_space(6.0);
        let warn = if bulk == Bulk::OverwriteRemote { Color32::from_rgb(0xe8, 0x5a, 0x5a) } else { ui.visuals().weak_text_color() };
        ui.colored_label(warn, warning);
        ui.weak("Files changed since you looked at them are skipped.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let go = egui::Button::new(RichText::new(button).color(Color32::WHITE).strong()).fill(fill);
            if ui.add_enabled(!files.is_empty(), go).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_highlights_an_unchanged_file() {
        let root = std::env::temp_dir().join(format!("agent-diff-ws-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (local, remote) = (root.join("local"), root.join("remote"));
        let text: String = (0..30).map(|i| if i == 12 { "the Needle is here\n".to_string() } else { format!("line {i}\n") }).collect();
        for side in [&local, &remote] {
            std::fs::create_dir_all(side).unwrap();
            std::fs::write(side.join("notes.txt"), &text).unwrap();
        }
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&local).args(args).output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["add", "."]);
        let ctx = egui::Context::default();
        let ws = Workspace { local: local.display().to_string(), remote: remote.display().to_string(), interval: 3600.0, sessions: Vec::new(), forks: Vec::new(), skip_commit: Vec::new(), view: ListView::default() };
        let mut tab = Tab::open(ws, &ctx);
        let state_dir = tab.session.as_ref().unwrap().state_dir.clone();
        let mut options = Options { theme: 0, syntax_colors: true, full_view: false, layout: Layout::default(), git_poll: GitPoll::default() };
        let h = Highlighter::new();
        let mut frame = |tab: &mut Tab| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| tab_ui(tab, ui, &h, &mut options));
            out.textures_delta.clear();
            out.shapes
                .iter()
                .filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some(t.galley.clone()) } else { None })
                .flat_map(|g| g.job.sections.iter().filter(|s| s.format.background != Color32::TRANSPARENT).map(|s| g.job.text[s.byte_range.start.0..s.byte_range.end.0].to_string()).collect::<Vec<_>>())
                .collect::<Vec<String>>()
        };
        for _ in 0..300 {
            frame(&mut tab);
            if !tab.session.as_ref().unwrap().shared.lock().unwrap().synced.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        tab.explorers[0].search.query = "needle".into();
        tab.explorers[0].search.typed();
        std::thread::sleep(Duration::from_millis(600));
        for _ in 0..300 {
            frame(&mut tab);
            if tab.explorers[0].search.results.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(tab.explorers[0].search.results.as_ref().unwrap().len(), 1);
        tab.selected = Some("notes.txt".into());
        let mut marked = Vec::new();
        for _ in 0..5 {
            marked = frame(&mut tab);
        }
        assert!(tab.edit.is_some(), "an unchanged text file opens in the editor");
        assert!(marked.iter().any(|m| m == "Needle"), "{marked:?}");
        drop(tab);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&state_dir);
    }


    #[test]
    fn unchanged_text_files_open_in_the_editor() {
        let text = |origin, local_exists| FileEntry {
            path: "a.rs".into(),
            rename: None,
            origin,
            local_change: None,
            remote_change: None,
            local_exists,
            remote_exists: true,
            body: Body::Text(Texts::new("x\n", "x\n")),
            version: 0,
        };
        assert!(editable_unchanged(&text(Origin::Synced, true)));
        assert!(!editable_unchanged(&text(Origin::Remote, true)));
        assert!(!editable_unchanged(&text(Origin::Synced, false)));
        assert!(!editable_unchanged(&FileEntry { body: Body::Binary, ..text(Origin::Synced, true) }));
    }

    #[test]
    fn explain_one_change_or_the_whole_file() {
        let e = FileEntry {
            path: "src/a.rs".into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Binary,
            version: 0,
        };
        let one = explain_prompt(&e, &Explain { patch: "@@ -1 +1 @@\n-a\n+b\n".into(), whole_file: false });
        assert!(one.starts_with("Explain this modification to src/a.rs"), "{one}");
        assert!(one.contains("```diff\n@@ -1 +1 @@\n-a\n+b\n```"), "{one}");
        let all = explain_prompt(&e, &Explain { patch: String::new(), whole_file: true });
        assert!(all.starts_with("Explain all the modifications to src/a.rs"), "{all}");
    }

    #[test]
    fn list_view_is_saved_and_read_back() {
        let mut view = ListView { tree: true, changes_tree: true, expand: false, ..ListView::default() };
        view.toggle("Changed on remote (Claude)/src/app/");
        view.toggle(&ListView::group_key("All files"));
        let back = ListView::parse(&view.flags(), &view.flipped_text(), &ListView::default());
        assert_eq!(back, view);
        assert!(back.is_open("Changed on remote (Claude)/src/app/") && !back.is_open("Changed on remote (Claude)/docs/"));
        assert!(!back.is_open(&ListView::group_key("All files")) && back.is_open(&ListView::group_key("Changed locally (you)")));

        let mut expanded = back.clone();
        expanded.set_expand(true);
        assert!(expanded.is_open("Changed on remote (Claude)/docs/") && expanded.is_open("Changed on remote (Claude)/src/app/"));
        assert!(!expanded.is_open(&ListView::group_key("All files")));

        let off = ListView::default();
        assert_eq!(ListView::parse(&off.flags(), "", &view), off);
        let legacy = ListView { tree: false, expand: true, ..ListView::default() };
        assert_eq!(ListView::parse("", "", &legacy), legacy);
    }

    #[test]
    fn bulk_in_a_folder() {
        let entry = |path: &str, origin| FileEntry {
            path: path.into(),
            rename: None,
            origin,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Binary,
            version: 0,
        };
        let entries = [entry("back/a", Origin::Remote), entry("back/b", Origin::Local), entry("backup/c", Origin::Remote), entry("back/d", Origin::Both)];
        let paths = |bulk: Bulk, folder| bulk.files(&entries, folder).map(|e| e.path.as_str()).collect::<Vec<_>>();
        assert_eq!(paths(Bulk::PullClaude, "back/"), ["back/a"]);
        assert_eq!(paths(Bulk::PushMine, "back/"), ["back/b"]);
        assert_eq!(paths(Bulk::PullClaude, ""), ["back/a", "backup/c"]);
    }

    #[test]
    fn effects_describe_each_case() {
        let e = |local_exists, remote_exists, renamed: bool| FileEntry {
            path: "f".into(),
            rename: renamed.then(|| Rename { local_path: "a".into(), remote_path: "f".into(), from: "a".into(), similarity: 100 }),
            origin: Origin::Remote,
            local_change: None,
            remote_change: None,
            local_exists,
            remote_exists,
            body: Body::Binary,
            version: 0,
        };
        let pull = Action::TakeRemoteFile;
        let push = Action::TakeLocalFile;
        assert_eq!(effect(&e(true, true, false), pull), "overwritten locally");
        assert_eq!(effect(&e(false, true, false), pull), "created locally");
        assert_eq!(effect(&e(true, false, false), pull), "deleted locally");
        assert_eq!(effect(&e(true, true, true), pull), "moved locally");
        assert_eq!(effect(&e(true, true, false), push), "overwritten on the remote");
        assert_eq!(effect(&e(true, false, false), push), "created on the remote");
        assert_eq!(effect(&e(false, true, false), push), "deleted on the remote");
        assert_eq!(effect(&e(true, true, true), push), "moved on the remote");
    }
}
