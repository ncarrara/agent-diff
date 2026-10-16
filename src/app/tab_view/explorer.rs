use super::*;

type Group<'a> = (String, Color32, Vec<&'a FileEntry>);
type Counts = std::collections::HashMap<String, usize>;

const ALL_FILES: &str = "All files";

pub(super) fn filter_id(tab: &Tab, kind: ExplorerKind) -> egui::Id {
    egui::Id::new(("file-filter", kind.name(), &tab.ws.local, &tab.ws.remote))
}

pub(super) fn search_id(tab: &Tab, kind: ExplorerKind) -> egui::Id {
    egui::Id::new(("file-search", kind.name(), &tab.ws.local, &tab.ws.remote))
}

pub(super) fn update_searches(ctx: &egui::Context, tab: &mut Tab, f: &Frame) {
    let files_scope = f.search_scope();
    tab.explorers[ExplorerKind::Files.index()].search.update(ctx, files_scope, || f.searchable());
    let changes_scope = f.entries.len() as u64;
    tab.explorers[ExplorerKind::Changes.index()].search.update(ctx, changes_scope, || search_files(f.entries.iter(), &f.local, &f.mirror));
    let highlighted: Arc<str> = Arc::from(tab.explorers[tab.searched_in.index()].search.shown_query().unwrap_or(""));
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(QUERY_ID), highlighted));
}

pub(super) fn explorer_panel(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, acts: &mut ListActions, kind: ExplorerKind, whole_window: bool) {
    let pane = kind.pane();
    let min_width = tab.explorer_min_widths[kind.index()].max(96.0);
    let width = match kind {
        ExplorerKind::Files => tab.layout.files_width,
        ExplorerKind::Changes => tab.layout.changes_width,
    };
    let contents = |ui: &mut egui::Ui| {
        if kind == ExplorerKind::Changes {
            git_log_panel(ui, &mut tab.git_log, &f.local, &f.repos);
        }
        ui.add_space(4.0);
        let toolbar = toolbar(ui, tab, f, kind);
        filter_row(ui, tab, kind);
        search_row(ui, tab, kind);
        ui.separator();
        egui::ScrollArea::vertical().id_salt(("explorer", kind.name())).auto_shrink(false).show(ui, |ui| {
            let background = ui.interact(ui.clip_rect(), ui.id().with("background"), egui::Sense::click());
            explorer_list(ui, tab, f, acts, kind);
            ui.add_space(12.0);
            background.context_menu(|ui| folder_menu(ui, "", acts));
            accept_drops(ui, &background, "", acts);
        });
        (toolbar, ui.min_rect().width())
    };
    if whole_window {
        let shown = egui::CentralPanel::default().show(ui, contents);
        tab.panes.record(pane, shown.response.rect);
        return;
    }
    let panel = match kind {
        ExplorerKind::Files => egui::Panel::left(panel_id(kind.name())),
        ExplorerKind::Changes => egui::Panel::right(panel_id(kind.name())),
    };
    let shown = panel.resizable(true).default_size(width).min_size(min_width).show(ui, contents);
    let shown_width = shown.response.rect.width().round();
    match kind {
        ExplorerKind::Files => tab.layout.files_width = shown_width,
        ExplorerKind::Changes => tab.layout.changes_width = shown_width,
    }
    let (toolbar, content) = shown.inner;
    tab.explorer_min_widths[kind.index()] = toolbar + shown.response.rect.width() - content;
    tab.panes.record(pane, shown.response.rect);
}

fn tree_of(view: &mut ListView, kind: ExplorerKind) -> &mut bool {
    match kind {
        ExplorerKind::Files => &mut view.tree,
        ExplorerKind::Changes => &mut view.changes_tree,
    }
}

fn expanded(view: &ListView, kind: ExplorerKind) -> bool {
    match kind {
        ExplorerKind::Files => view.expand,
        ExplorerKind::Changes => view.changes_expand,
    }
}

fn set_expanded(view: &mut ListView, kind: ExplorerKind, expand: bool) {
    match kind {
        ExplorerKind::Files => view.set_expand(expand),
        ExplorerKind::Changes => view.set_changes_expand(expand),
    }
}

fn key_prefix(kind: ExplorerKind) -> &'static str {
    match kind {
        ExplorerKind::Files => "",
        ExplorerKind::Changes => CHANGES_KEY,
    }
}

fn toolbar(ui: &mut egui::Ui, tab: &mut Tab, f: &mut Frame, kind: ExplorerKind) -> f32 {
    let mut window = false;
    let row = ui.horizontal(|ui| {
        tree_toggles(ui, &mut tab.ws.view, kind);
        ui.separator();
        match kind {
            ExplorerKind::Files => new_file_button(ui, tab),
            ExplorerKind::Changes => {
                ui.strong(format!("Changes ({})", f.entries.len()));
            }
        }
        ui.separator();
        window = ui.small_button("⧉").on_hover_text(WINDOW_HELP).clicked();
    });
    if kind == ExplorerKind::Changes {
        ui.horizontal_wrapped(|ui| bulk_buttons(ui, tab, f));
    }
    if window {
        toggle_window(tab, kind.pane());
    }
    row.response.rect.width()
}

fn tree_toggles(ui: &mut egui::Ui, view: &mut ListView, kind: ExplorerKind) {
    let tree = tree_of(view, kind);
    ui.toggle_value(tree, "🌲").on_hover_text("Show the files of this panel as a folder tree, or as a flat list when off");
    let (tree, mut expand) = (*tree, expanded(view, kind));
    if tree && ui.toggle_value(&mut expand, "⊞").on_hover_text("Expand all the folders of this panel (and new ones by default), or collapse them").changed() {
        set_expanded(view, kind, expand);
    }
}

fn new_file_button(ui: &mut egui::Ui, tab: &mut Tab) {
    let folder = new_file_folder(ui.ctx(), tab);
    let where_to = if folder.is_empty() { "at the workspace root".to_string() } else { format!("in {folder}") };
    if ui.button("🗋").on_hover_text(format!("New file {where_to} (the folder you last clicked, else the selected file's); asks its name and whether to sync it")).clicked() {
        tab.new_file = Some(NewFile::in_folder(folder));
    }
}

const PULL_GREEN: Color32 = Color32::from_rgb(0x2e, 0x8b, 0x46);
const PUSH_RED: Color32 = Color32::from_rgb(0xc0, 0x39, 0x2b);

fn filled_button(text: String, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(fill)
}

fn bulk_buttons(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame) {
    let count = |o| f.entries.iter().filter(|e| e.origin == o).count();
    let remote_only = count(Origin::Remote);
    if ui
        .add_enabled(remote_only > 0 && !f.busy, filled_button(format!("⬇ Pull ({remote_only})"), PULL_GREEN))
        .on_hover_text("Pull all Claude's changes: copy every file changed only on the remote into your local workspace")
        .clicked()
    {
        tab.confirm = Some((Bulk::PullClaude, String::new()));
    }
    let local_only = count(Origin::Local);
    if ui
        .add_enabled(local_only > 0 && !f.busy, filled_button(format!("⬆ Push ({local_only})"), PUSH_RED))
        .on_hover_text("Push all your changes: copy every file changed only locally to the remote workspace")
        .clicked()
    {
        tab.confirm = Some((Bulk::PushMine, String::new()));
    }
    if ui
        .add_enabled(!f.busy, egui::Button::new("⚠ Make remote identical"))
        .on_hover_text("Empty the remote (its .git folders kept), then send every file git tracks here (asks first)")
        .clicked()
    {
        tab.confirm = Some((Bulk::OverwriteRemote, String::new()));
    }
}

fn full_width_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 0.0), egui::Layout::right_to_left(egui::Align::Center), add);
}

fn filter_row(ui: &mut egui::Ui, tab: &mut Tab, kind: ExplorerKind) {
    let id = filter_id(tab, kind);
    full_width_row(ui, |ui| {
        let explorer = &mut tab.explorers[kind.index()];
        let cleared = !explorer.filter.is_empty() && ui.small_button("×").on_hover_text("Clear the filter").clicked();
        let field = egui::TextEdit::singleline(&mut explorer.filter).id(id).hint_text("🔍 filter files").desired_width(ui.available_width());
        let filter_field = ui.add(field).on_hover_text("Only list the files whose path contains all these words (case-insensitive)\n↑/↓ while typing here: previous / next file");
        if cleared {
            explorer.filter.clear();
        }
        if filter_field.changed() || cleared {
            explorer.filter_collapsed.clear();
        }
        if filter_field.has_focus()
            && let Some(step) = list_step(ui.ctx())
        {
            tab.selected = step_in(&tab.order, tab.selected.as_ref(), step);
        }
    });
}

fn search_row(ui: &mut egui::Ui, tab: &mut Tab, kind: ExplorerKind) {
    let id = search_id(tab, kind);
    full_width_row(ui, |ui| {
        let explorer = &mut tab.explorers[kind.index()];
        let cleared = !explorer.search.query.is_empty() && ui.small_button("×").on_hover_text("Clear the search").clicked();
        let mut step = search_steps(ui, &explorer.search, tab.selected.as_deref());
        if explorer.search.searching() {
            ui.spinner();
        }
        let field = egui::TextEdit::singleline(&mut explorer.search.query).id(id).hint_text("🔎 search in files").desired_width(ui.available_width());
        let shown = ui.add(field).on_hover_text("Search the content of the files of this panel (case-insensitive). Enter: next match");
        if shown.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && explorer.search.results.as_ref().is_some_and(|r| !r.is_empty()) {
            step = Some(true);
        }
        if let Some(forward) = step
            && let Some(path) = explorer.search.step(tab.selected.as_deref(), forward)
        {
            tab.selected = Some(path);
        }
        if cleared {
            explorer.search.query.clear();
        }
        if shown.changed() || cleared {
            explorer.search.typed();
            explorer.filter_collapsed.clear();
        }
        if shown.changed() || step.is_some() {
            tab.searched_in = kind;
        }
    });
}

fn search_steps(ui: &mut egui::Ui, search: &ContentSearch, selected: Option<&str>) -> Option<bool> {
    if !search.results.as_ref().is_some_and(|r| !r.is_empty()) {
        return None;
    }
    if let Some((at, of)) = search.position(selected) {
        ui.weak(format!("{at}/{of}"));
    }
    let mut step = None;
    if ui.small_button("⏷").on_hover_text("Next match (Enter in the search field)").clicked() {
        step = Some(true);
    }
    if ui.small_button("⏶").on_hover_text("Previous match").clicked() {
        step = Some(false);
    }
    step
}

fn candidates<'a>(f: &'a Frame, kind: ExplorerKind) -> Vec<&'a FileEntry> {
    match kind {
        ExplorerKind::Files => f.entries.iter().chain(f.gitignored_entries.iter()).chain(f.synced.iter()).collect(),
        ExplorerKind::Changes => f.entries.iter().collect(),
    }
}

fn groups<'a>(tab: &Tab, f: &'a Frame, kind: ExplorerKind, counts: Option<&Counts>) -> Vec<Group<'a>> {
    let view = &tab.ws.view;
    let tree = match kind {
        ExplorerKind::Files => view.tree,
        ExplorerKind::Changes => view.changes_tree,
    };
    if let Some(counts) = counts {
        let found = candidates(f, kind).into_iter().filter(|e| counts.contains_key(&e.path)).collect();
        return vec![("🔎 Found in".into(), Color32::from_rgb(0xe0, 0xc0, 0x40), sort_files(found, tree))];
    }
    match kind {
        ExplorerKind::Files => {
            let files = candidates(f, kind);
            vec![(ALL_FILES.into(), Color32::GRAY, sort_files(files, tree))]
        }
        ExplorerKind::Changes => LIST_ORDER
            .iter()
            .map(|&origin| {
                let (title, color) = origin_label(origin);
                (title.into(), color, sorted_group(&f.entries, origin, tree))
            })
            .collect(),
    }
}

fn shown_gitignored_folders<'a>(tab: &Tab, f: &'a Frame, kind: ExplorerKind, searching: bool) -> Vec<&'a str> {
    if kind != ExplorerKind::Files || searching {
        return Vec::new();
    }
    let filter = &tab.explorers[kind.index()].filter;
    f.gitignored_dirs.iter().filter(|d| matches_filter(d, filter)).map(String::as_str).collect()
}

fn empty_message(tab: &Tab, f: &Frame, kind: ExplorerKind, found: Option<&Counts>, nothing_shown: bool) -> Option<&'static str> {
    let filter = &tab.explorers[kind.index()].filter;
    match found {
        Some(counts) if counts.is_empty() => Some("No file contains this text."),
        None if kind == ExplorerKind::Changes && f.entries.is_empty() => Some("✔ Local and remote are in sync."),
        _ if nothing_shown && !filter.trim().is_empty() => Some("No file matches the filter."),
        _ => None,
    }
}

fn explorer_list(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame, acts: &mut ListActions, kind: ExplorerKind) {
    let explorer = &tab.explorers[kind.index()];
    let found = explorer.search.results.clone().filter(|_| explorer.search.active_query().is_some());
    let counts: Option<Counts> = found.map(|hits| hits.iter().map(|h| (h.path.clone(), h.count)).collect());
    let filter = explorer.filter.clone();
    let groups: Vec<Group> = groups(tab, f, kind, counts.as_ref())
        .into_iter()
        .map(|(title, color, group)| (title, color, group.into_iter().filter(|e| matches_filter(&e.path, &filter)).collect()))
        .collect();
    let folders = shown_gitignored_folders(tab, f, kind, counts.is_some());
    let empty_folders: Vec<&str> = if kind == ExplorerKind::Files && counts.is_none() { f.empty_folders.iter().map(String::as_str).collect() } else { Vec::new() };
    let nothing_shown = folders.is_empty() && groups.iter().all(|(_, _, g)| g.is_empty());
    if let Some(message) = empty_message(tab, f, kind, counts.as_ref(), nothing_shown) {
        ui.add_space(8.0);
        ui.weak(message);
    }
    for (index, group) in groups.into_iter().enumerate() {
        let (group_folders, group_empty_folders): (&[&str], &[&str]) = if index == 0 { (&folders, &empty_folders) } else { (&[], &[]) };
        if !group.2.is_empty() || !group_folders.is_empty() || !group_empty_folders.is_empty() {
            file_group(ui, tab, f, acts, group, Listing { kind, folders: group_folders, empty_folders: group_empty_folders, counts: counts.as_ref() });
        }
    }
}

struct Listing<'a> {
    kind: ExplorerKind,
    folders: &'a [&'a str],
    empty_folders: &'a [&'a str],
    counts: Option<&'a Counts>,
}

fn file_group(ui: &mut egui::Ui, tab: &mut Tab, f: &Frame, acts: &mut ListActions, (title, color, group): Group, Listing { kind, folders, empty_folders, counts }: Listing) {
    ui.add_space(6.0);
    let prefix = key_prefix(kind);
    let header = RichText::new(format!("{title} ({})", group.len())).color(color).strong();
    let group_key = ListView::group_key(&format!("{prefix}{title}"));
    let explorer = &mut tab.explorers[kind.index()];
    let filtering = !explorer.filter.trim().is_empty() || counts.is_some();
    let open = if filtering { !explorer.filter_collapsed.contains(&group_key) } else { tab.ws.view.is_open(&group_key) };
    let tree = *tree_of(&mut tab.ws.view, kind);
    let shown = egui::CollapsingHeader::new(header).id_salt(("file-group", kind.name(), &title, &tab.ws.local, &tab.ws.remote)).open(Some(open)).show(ui, |ui| {
        let mut ctx = ListCtx {
            selected: &mut tab.selected,
            acts: &mut *acts,
            repos: &f.repos,
            view: &mut tab.ws.view,
            filtering: filtering.then_some(&mut explorer.filter_collapsed),
            counts,
            untracked: &f.untracked,
            gitignored: &f.gitignored,
            gitignored_dirs: &f.gitignored_dirs,
        };
        if tree {
            let mut root = Dir::default();
            for folder in folders.iter().chain(empty_folders) {
                root.insert_folder(folder);
            }
            for e in &group {
                root.insert(e);
            }
            tree_ui(ui, root, &format!("{prefix}{title}/"), &mut ctx);
        } else {
            for folder in folders {
                gitignored_folder_row(ui, folder, &mut ctx);
            }
            for e in group {
                file_item(ui, e, false, &mut ctx);
            }
        }
    });
    shown.header_response.context_menu(|ui| folder_menu(ui, "", acts));
    accept_drops(ui, &shown.header_response, "", acts);
    if shown.header_response.clicked() {
        if !filtering {
            tab.ws.view.toggle(&group_key);
        } else if !explorer.filter_collapsed.remove(&group_key) {
            explorer.filter_collapsed.insert(group_key);
        }
    }
}
