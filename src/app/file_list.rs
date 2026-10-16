use super::*;

pub(super) const LIST_ORDER: [Origin; 3] = [Origin::Remote, Origin::Both, Origin::Local];
pub(super) struct ThemeDef {
    pub(super) name: &'static str,
    /// One of syntect's bundled themes.
    pub(super) syntax: &'static str,
    pub(super) dark: bool,
}

pub(super) fn origin_label(o: Origin) -> (&'static str, Color32) {
    match o {
        Origin::Remote => ("Changed on remote (Claude)", CLAUDE),
        Origin::Local => ("Changed locally (you)", YOU),
        Origin::Both => ("Changed on both sides", BOTH),
        Origin::Synced => ("Unchanged", Color32::GRAY),
    }
}

pub(super) fn change_color(c: Option<Change>) -> Color32 {
    match c {
        None => Color32::GRAY,
        Some(Change::Added) => Color32::from_rgb(0x4c, 0xc0, 0x6a),
        Some(Change::Modified) => Color32::from_rgb(0xe0, 0xa0, 0x30),
        Some(Change::Deleted) => Color32::from_rgb(0xe8, 0x5a, 0x5a),
        Some(Change::Renamed) => Color32::from_rgb(0xb0, 0x88, 0xf0),
    }
}

/// Files of one origin group, in display order: by path, or for the tree
/// view folders before files at each level.
pub(super) fn sorted_group(entries: &[FileEntry], origin: Origin, tree: bool) -> Vec<&FileEntry> {
    sort_files(entries.iter().filter(|e| e.origin == origin).collect(), tree)
}

/// Files in display order: by path, or for the tree view folders before
/// files at each level.
pub(super) fn sort_files(mut group: Vec<&FileEntry>, tree: bool) -> Vec<&FileEntry> {
    if tree {
        group.sort_by_cached_key(|e| {
            let parts: Vec<&str> = e.path.split('/').collect();
            let last = parts.len() - 1;
            parts.into_iter().enumerate().map(|(i, p)| (i == last, p.to_string())).collect::<Vec<_>>()
        });
    } else {
        group.sort_by(|a, b| a.path.cmp(&b.path));
    }
    group
}

/// One file in the list: colored change letters (local, remote) and a name.
/// What was chosen in the right-click menus of the file list.
#[derive(Default)]
pub(super) struct ListActions {
    pub(super) delete: Option<FileEntry>,
    pub(super) rename: Option<FileEntry>,
    pub(super) sync: Option<FileEntry>,
    /// Folder (relative path, `/`-terminated, empty for the root) to create a file in.
    pub(super) new_file_in: Option<String>,
    pub(super) bulk: Option<(Bulk, String)>,
    pub(super) open_tab: Option<String>,
    pub(super) gitignore: Option<String>,
    pub(super) gitignored_toggled: Option<String>,
    pub(super) push_only: Option<FileEntry>,
    pub(super) new_folder_in: Option<String>,
    pub(super) folder_clicked: Option<String>,
    pub(super) move_to: Option<(DraggedFile, String)>,
    pub(super) rename_folder: Option<String>,
    pub(super) import: Option<(String, Vec<PathBuf>)>,
    pub(super) delete_folder: Option<String>,
}

pub(super) struct ListCtx<'a> {
    pub(super) selected: &'a mut Option<String>,
    pub(super) acts: &'a mut ListActions,
    pub(super) repos: &'a [Repo],
    pub(super) view: &'a mut ListView,
    pub(super) filtering: Option<&'a mut std::collections::BTreeSet<String>>,
    pub(super) counts: Option<&'a std::collections::HashMap<String, usize>>,
    pub(super) untracked: &'a std::collections::BTreeSet<String>,
    pub(super) gitignored: &'a std::collections::BTreeSet<String>,
    pub(super) gitignored_dirs: &'a std::collections::BTreeSet<String>,
}

impl ListCtx<'_> {
    pub(super) fn is_open(&self, key: &str) -> bool {
        match &self.filtering {
            Some(collapsed) => !collapsed.contains(key),
            None => self.view.is_open(key),
        }
    }

    pub(super) fn toggle(&mut self, key: &str) {
        match &mut self.filtering {
            Some(collapsed) => {
                if !collapsed.remove(key) {
                    collapsed.insert(key.to_string());
                }
            }
            None => self.view.toggle(key),
        }
    }
}

pub(super) fn matches_filter(path: &str, filter: &str) -> bool {
    let path = path.to_lowercase();
    filter.split_whitespace().all(|word| path.contains(&word.to_lowercase()))
}

const PLAIN_FILE_ICON: &str = "📄";

pub(super) fn file_icon(path: &str) -> &'static str {
    let Some((_, extension)) = path.rsplit('/').next().and_then(|name| name.rsplit_once('.')) else { return PLAIN_FILE_ICON };
    match extension.to_ascii_lowercase().as_str() {
        "rs" => "🦀",
        "py" | "pyi" => "🐍",
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" => "📜",
        "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" => "⚙",
        "md" | "markdown" | "rst" | "txt" | "adoc" => "📝",
        "html" | "htm" | "xml" | "vue" | "svelte" => "🌐",
        "css" | "scss" | "sass" | "less" => "🎨",
        "sh" | "bash" | "zsh" | "fish" | "ps1" => "⌨",
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "go" | "java" | "kt" | "kts" | "scala" | "rb" | "php" | "swift" | "cs" => "📃",
        "sql" | "db" | "sqlite" => "🗄",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" => "🖼",
        "glb" | "gltf" | "obj" | "fbx" | "stl" => "🎲",
        "wav" | "mp3" | "ogg" | "flac" | "opus" | "aiff" => "🔊",
        "lock" => "🔒",
        _ => PLAIN_FILE_ICON,
    }
}

pub(super) fn repo_of<'a>(repos: &'a [Repo], path: &str) -> Option<&'a Repo> {
    repos
        .iter()
        .filter(|r| r.path.is_empty() || path.strip_prefix(r.path.as_str()).is_some_and(|rest| rest.starts_with('/')))
        .max_by_key(|r| r.path.len())
}

fn repo_at<'a>(repos: &'a [Repo], folder: &str) -> Option<&'a Repo> {
    let folder = folder.trim_end_matches('/');
    repos.iter().find(|r| r.path == folder)
}

fn branch_text(r: &Repo) -> String {
    r.branch.as_deref().map(|b| format!("  🌿 {b}")).unwrap_or_default()
}

fn row_height(ui: &egui::Ui) -> f32 {
    let text = ui.text_style_height(&egui::TextStyle::Monospace);
    (text + 2.0 * ui.spacing().button_padding.y).max(ui.spacing().interact_size.y)
}

fn skipped_off_screen(ui: &mut egui::Ui) -> bool {
    let height = row_height(ui);
    let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(1.0, height));
    if ui.is_rect_visible(row) {
        return false;
    }
    ui.allocate_exact_size(egui::vec2(0.0, height), egui::Sense::hover());
    true
}

#[derive(Clone)]
pub(super) struct DraggedFile {
    pub(super) path: String,
    pub(super) version: u64,
    pub(super) rename: Option<Rename>,
}

fn files_dropped(ui: &egui::Ui) -> Vec<PathBuf> {
    ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect())
}

pub(super) fn accept_drops(ui: &egui::Ui, target: &egui::Response, folder: &str, acts: &mut ListActions) {
    let files_hovering = ui.input(|i| !i.raw.hovered_files.is_empty()) && target.contains_pointer();
    if target.dnd_hover_payload::<DraggedFile>().is_some() || files_hovering {
        let stroke = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
        ui.painter().rect_stroke(target.rect.expand(2.0), 3.0, stroke, egui::StrokeKind::Outside);
    }
    if let Some(dragged) = target.dnd_release_payload::<DraggedFile>() {
        acts.move_to = Some(((*dragged).clone(), folder.to_string()));
    }
    let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| target.rect.contains(p));
    let files = files_dropped(ui);
    if acts.import.is_none() && over && !files.is_empty() {
        acts.import = Some((folder.to_string(), files));
    }
}

pub(super) fn dropped_anywhere(ui: &egui::Ui) -> Vec<PathBuf> {
    files_dropped(ui)
}

pub(super) fn file_item(ui: &mut egui::Ui, e: &FileEntry, in_tree: bool, ctx: &mut ListCtx) {
    if skipped_off_screen(ui) {
        return;
    }
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let fmt = |color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    job.append(change_letter(e.local_change), 0.0, fmt(change_color(e.local_change)));
    job.append(change_letter(e.remote_change), 0.0, fmt(change_color(e.remote_change)));
    let repo = repo_of(ctx.repos, &e.path);
    let untracked = ctx.untracked.contains(&e.path);
    let gitignored = ctx.gitignored.contains(&e.path);
    let text = match (gitignored, untracked) {
        (true, _) => gitignored_color(ui),
        (false, true) => untracked_color(ui),
        _ => ui.visuals().text_color(),
    };
    let (folder, name) = match e.path.rsplit_once('/') {
        Some((folder, name)) => (format!("{folder}/"), name),
        None => (String::new(), e.path.as_str()),
    };
    job.append(&format!("  {} ", file_icon(&e.path)), 0.0, fmt(text));
    match repo {
        _ if in_tree => {}
        Some(r) if !r.path.is_empty() => {
            job.append(&r.path, 0.0, fmt(REPO));
            job.append(&folder[r.path.len().min(folder.len())..], 0.0, fmt(text));
        }
        _ => job.append(&folder, 0.0, fmt(text)),
    }
    job.append(name, 0.0, fmt(text));
    let mut hover = e.path.clone();
    if let Some(r) = &e.rename {
        job.append(&format!("  ← {}", r.from), 0.0, fmt(change_color(Some(Change::Renamed)).gamma_multiply(0.8)));
        hover = format!("{} → {} ({}% similar)", r.from, e.path, r.similarity);
    }
    match repo {
        Some(r) => hover += &format!("\n📦 repository {}{}", if r.path.is_empty() { "(workspace root)" } else { &r.path }, branch_text(r)),
        None if !ctx.repos.is_empty() => {
            job.append("  ⚠", 0.0, fmt(Color32::from_rgb(0xe8, 0x5a, 0x5a)));
            hover += "\n⚠ not in any git repository: taking it will not git add it, so it stays out of git";
        }
        None => {}
    }
    if untracked {
        job.append("  ?", 0.0, fmt(Color32::from_rgb(0xe0, 0xa0, 0x40)));
        hover += "\n? not added to git: right-click → Sync to git add it";
    }
    if gitignored {
        job.append("  🙈", 0.0, fmt(Color32::GRAY));
        hover += &format!("\n🙈 {GITIGNORED_HELP}");
    }
    if let Some(count) = ctx.counts.and_then(|c| c.get(&e.path)) {
        job.append(&format!("  ({count})"), 0.0, fmt(Color32::from_rgb(0xe0, 0xc0, 0x40)));
    }
    let item = ui
        .selectable_label(ctx.selected.as_deref() == Some(&e.path), job)
        .on_hover_text(format!("{hover}\nfirst letter: local change, second: remote change\nA added, M modified, D deleted, R renamed/moved\nclick: open · double-click: open in its own tab · drag onto a folder: move it · right-click for more"))
        .interact(egui::Sense::drag());
    if !gitignored && (item.drag_started() || item.dragged()) {
        item.dnd_set_drag_payload(DraggedFile { path: e.path.clone(), version: e.version, rename: e.rename.clone() });
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if item.clicked() {
        *ctx.selected = Some(e.path.clone());
    }
    if item.double_clicked() || item.middle_clicked() {
        ctx.acts.open_tab = Some(e.path.clone());
    }
    item.context_menu(|ui| {
        if ui.button("📑 New tab").on_hover_text("A tab that keeps this file, even with F7 (double-click or middle-click does the same)").clicked() {
            ctx.acts.open_tab = Some(e.path.clone());
        }
        if ui.button("📄 New file…").on_hover_text("A new file next to this one; asks its name and whether to sync it").clicked() {
            ctx.acts.new_file_in = Some(e.path.rsplit_once('/').map(|(folder, _)| format!("{folder}/")).unwrap_or_default());
        }
        let sync_help = match e.origin {
            Origin::Local => "git add it, and push your version to the remote",
            Origin::Remote => "Take Claude's version, git added",
            Origin::Both => "Changed on both sides: take one side or pick hunks instead",
            Origin::Synced => "Already the same on both sides: only git add it",
        };
        let refused = gitignored.then_some(GITIGNORED_HELP);
        let can_sync = e.origin != Origin::Both && refused.is_none();
        let sync_help = if untracked { "git add it, and push it to the remote" } else { sync_help };
        if ui.add_enabled(can_sync, egui::Button::new("⇄ Sync")).on_hover_text(sync_help).on_disabled_hover_text(refused.unwrap_or(sync_help)).clicked() {
            ctx.acts.sync = Some(e.clone());
        }
        if e.origin == Origin::Local && refused.is_none() && ui.button("⬆ Push only").on_hover_text("Send your version to the remote, without git add").clicked() {
            ctx.acts.push_only = Some(e.clone());
        }
        let refused_help = refused.unwrap_or_default();
        if ui
            .add_enabled(refused.is_none(), egui::Button::new("✏ Rename…"))
            .on_hover_text("Locally, on the remote and in git")
            .on_disabled_hover_text(refused_help)
            .clicked()
        {
            ctx.acts.rename = Some(e.clone());
        }
        if ui
            .add_enabled(refused.is_none(), egui::Button::new("🗑 Delete…"))
            .on_hover_text("Locally, on the remote and in git (asks first)")
            .on_disabled_hover_text(refused_help)
            .clicked()
        {
            ctx.acts.delete = Some(e.clone());
        }
        ui.separator();
        if refused.is_none() && gitignore_button(ui) {
            ctx.acts.gitignore = Some(e.path.clone());
        }
    });
}

pub(super) const GITIGNORED_HELP: &str = "gitignored: never synced, only your local copy (you can still open and edit it)";

/// A folder of the tree view.
#[derive(Default)]
pub(super) struct Dir<'a> {
    pub(super) dirs: std::collections::BTreeMap<&'a str, Dir<'a>>,
    pub(super) files: Vec<&'a FileEntry>,
}

impl<'a> Dir<'a> {
    pub(super) fn insert(&mut self, e: &'a FileEntry) {
        let mut node = self;
        let mut parts = e.path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.files.push(e);
            } else {
                node = node.dirs.entry(part).or_default();
            }
        }
    }

    pub(super) fn insert_folder(&mut self, path: &'a str) {
        let mut node = self;
        for part in path.trim_end_matches('/').split('/') {
            node = node.dirs.entry(part).or_default();
        }
    }
}

pub(super) fn tree_ui(ui: &mut egui::Ui, root: Dir, id: &str, ctx: &mut ListCtx) {
    match repo_at(ctx.repos, "").filter(|_| ctx.repos.len() > 1).cloned() {
        Some(root_repo) => {
            let title = repo_title("(workspace root)", &root_repo, ui);
            let hover = repo_hover(&root_repo, count_files(&root));
            folder_ui(ui, title, hover, format!("{id}/"), "", ctx, |ui, ctx| dir_ui(ui, &root, id, "", ctx));
        }
        None => dir_ui(ui, &root, id, "", ctx),
    }
}

fn folder_chain<'d, 'a>(name: &str, sub: &'d Dir<'a>, folder: &str, repos: &[Repo], gitignored_dirs: &std::collections::BTreeSet<String>) -> (String, &'d Dir<'a>, String) {
    let stops_at = |path: &str| repo_at(repos, path).is_some() || gitignored_dirs.contains(path);
    let (mut label, mut node, mut path) = (name.to_string(), sub, format!("{folder}{name}/"));
    while node.files.is_empty() && node.dirs.len() == 1 && !stops_at(&path) {
        let (n, s) = node.dirs.iter().next().unwrap();
        if stops_at(&format!("{path}{n}/")) {
            break;
        }
        label = format!("{label}/{n}");
        path = format!("{path}{n}/");
        node = s;
    }
    (label, node, path)
}

fn dir_ui(ui: &mut egui::Ui, dir: &Dir, id: &str, folder: &str, ctx: &mut ListCtx) {
    for (name, sub) in &dir.dirs {
        let (label, node, path) = folder_chain(name, sub, folder, ctx.repos, ctx.gitignored_dirs);
        if ctx.gitignored_dirs.contains(&path) {
            gitignored_folder_ui(ui, &label, &path, format!("{id}{path}"), ctx, |ui, ctx| dir_ui(ui, node, id, &path, ctx));
            continue;
        }
        let (title, hover) = match repo_at(ctx.repos, &path) {
            Some(r) => (repo_title(&label, r, ui), repo_hover(r, count_files(node))),
            None => (egui::WidgetText::from(format!("📁 {label}")), format!("{path}\nright-click for more")),
        };
        folder_ui(ui, title, hover, format!("{id}{path}"), &path, ctx, |ui, ctx| dir_ui(ui, node, id, &path, ctx));
    }
    for e in &dir.files {
        file_item(ui, e, true, ctx);
    }
}

fn folder_ui(
    ui: &mut egui::Ui,
    title: egui::WidgetText,
    hover: String,
    id_salt: String,
    path: &str,
    ctx: &mut ListCtx,
    contents: impl FnOnce(&mut egui::Ui, &mut ListCtx),
) {
    let open = ctx.is_open(&id_salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(&id_salt), open);
    state.set_open(open);
    let header = state.show_header(ui, |ui| ui.add(egui::Label::new(title).selectable(false).sense(egui::Sense::click())));
    let (arrow, label, _) = header.body(|ui| contents(ui, ctx));
    if arrow.clicked() || label.inner.clicked() {
        ctx.toggle(&id_salt);
        ctx.acts.folder_clicked = Some(path.to_string());
    }
    accept_drops(ui, &label.inner, path, ctx.acts);
    label.inner.on_hover_text(hover).context_menu(|ui| folder_menu(ui, path, ctx.acts));
}

const GITIGNORED_OPACITY: f32 = 0.35;
const UNTRACKED_ON_DARK: Color32 = Color32::from_rgb(0xf0, 0x8a, 0x8a);
const UNTRACKED_ON_LIGHT: Color32 = Color32::from_rgb(0xc8, 0x48, 0x48);

pub(super) fn gitignored_color(ui: &egui::Ui) -> Color32 {
    let grey = if ui.visuals().dark_mode { Color32::LIGHT_GRAY } else { Color32::GRAY };
    grey.gamma_multiply(GITIGNORED_OPACITY)
}

fn untracked_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode { UNTRACKED_ON_DARK } else { UNTRACKED_ON_LIGHT }
}

fn gitignored_folder_title(label: &str, path: &str, ui: &egui::Ui) -> (egui::WidgetText, String) {
    (RichText::new(format!("📁 {label}  🙈")).color(gitignored_color(ui)).into(), format!("{path}\n🙈 gitignored folder: click to list what it holds, one level at a time"))
}

fn toggle_gitignored_folder(path: &str, ctx: &mut ListCtx) {
    ctx.view.toggle_opened_gitignored(path);
    ctx.acts.gitignored_toggled = Some(path.to_string());
    ctx.acts.folder_clicked = Some(path.to_string());
}

fn gitignored_folder_ui(ui: &mut egui::Ui, label: &str, path: &str, id_salt: String, ctx: &mut ListCtx, contents: impl FnOnce(&mut egui::Ui, &mut ListCtx)) {
    let (title, hover) = gitignored_folder_title(label, path, ui);
    let open = ctx.view.opened_gitignored.contains(path);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(&id_salt), open);
    state.set_open(open);
    let header = state.show_header(ui, |ui| ui.add(egui::Label::new(title).selectable(false).sense(egui::Sense::click())));
    let (arrow, label, _) = header.body(|ui| contents(ui, ctx));
    if arrow.clicked() || label.inner.clicked() {
        toggle_gitignored_folder(path, ctx);
    }
    label.inner.on_hover_text(hover);
}

pub(super) fn gitignored_folder_row(ui: &mut egui::Ui, path: &str, ctx: &mut ListCtx) {
    let open = ctx.view.opened_gitignored.contains(path);
    let (title, hover) = gitignored_folder_title(&format!("{}{path}", if open { "⏷ " } else { "⏵ " }), path, ui);
    if ui.add(egui::Label::new(title).selectable(false).sense(egui::Sense::click())).on_hover_text(hover).clicked() {
        toggle_gitignored_folder(path, ctx);
    }
}

fn gitignore_button(ui: &mut egui::Ui) -> bool {
    ui.button("🙈 Ignore").on_hover_text("Add it to the closest .gitignore above it (in its git repository); a file git already tracks stays tracked").clicked()
}

pub(super) fn folder_menu(ui: &mut egui::Ui, path: &str, acts: &mut ListActions) {
    if ui.button("📄 New file…").on_hover_text("A new file in this folder").clicked() {
        acts.new_file_in = Some(path.to_string());
    }
    if ui.button("📁 New folder…").on_hover_text("An empty folder in this one, local until it holds a file").clicked() {
        acts.new_folder_in = Some(path.to_string());
    }
    if !path.is_empty() && ui.button("✏ Rename…").on_hover_text("Its files locally, on the remote and in git").clicked() {
        acts.rename_folder = Some(path.to_string());
    }
    if !path.is_empty() && gitignore_button(ui) {
        acts.gitignore = Some(format!("{}/", path.trim_end_matches('/')));
    }
    if !path.is_empty() && ui.button("🗑 Delete…").on_hover_text("Every file listed in it, locally, on the remote and in git (asks first)").clicked() {
        acts.delete_folder = Some(path.to_string());
    }
    ui.separator();
    let here = if path.is_empty() { "the workspace".to_string() } else { path.to_string() };
    if ui
        .button(RichText::new("⬇ Pull…").color(CLAUDE))
        .on_hover_text(format!("Take the remote version of every file only Claude changed in {here} (asks first)"))
        .clicked()
    {
        acts.bulk = Some((Bulk::PullClaude, path.to_string()));
    }
    if ui
        .button(RichText::new("⬆ Push…").color(YOU))
        .on_hover_text(format!("Push every file only you changed in {here} (asks first)"))
        .clicked()
    {
        acts.bulk = Some((Bulk::PushMine, path.to_string()));
    }
    if ui
        .button(RichText::new("⚠ Overwrite remote…").color(Color32::from_rgb(0xe8, 0x5a, 0x5a)))
        .on_hover_text(format!("Push your version of every file that differs in {here}, also the ones Claude changed (asks first)"))
        .clicked()
    {
        acts.bulk = Some((Bulk::OverwriteRemote, path.to_string()));
    }
}

fn repo_title(label: &str, r: &Repo, ui: &egui::Ui) -> egui::WidgetText {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.append(&format!("📦 {label}"), 0.0, egui::TextFormat { font_id: font.clone(), color: REPO, ..Default::default() });
    job.append(&branch_text(r), 0.0, egui::TextFormat { font_id: font, color: ui.visuals().weak_text_color(), ..Default::default() });
    job.into()
}

fn repo_hover(r: &Repo, files: usize) -> String {
    let name = if r.path.is_empty() { "(workspace root)" } else { &r.path };
    let branch = r.branch.as_deref().unwrap_or("unknown");
    format!("📦 git repository {name}\n🌿 branch {branch}\n{files} file(s) listed here\nright-click for more")
}

fn count_files(dir: &Dir) -> usize {
    dir.files.len() + dir.dirs.values().map(count_files).sum::<usize>()
}

pub(super) fn change_letter(c: Option<Change>) -> &'static str {
    match c {
        None => "·",
        Some(Change::Added) => "A",
        Some(Change::Modified) => "M",
        Some(Change::Deleted) => "D",
        Some(Change::Renamed) => "R",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_order_matches_display() {
        use crate::engine::{Body, FileEntry, Origin};
        let e = |path: &str| FileEntry {
            path: path.into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Binary,
            version: 0,
        };
        let entries: Vec<_> = ["b.txt", "a/z.txt", "a/b/c.txt", "a.txt", "c/d.txt"].map(e).into();
        let order = |tree| sorted_group(&entries, Origin::Remote, tree).iter().map(|e| e.path.as_str()).collect::<Vec<_>>();
        assert_eq!(order(false), ["a.txt", "a/b/c.txt", "a/z.txt", "b.txt", "c/d.txt"]);

        // The tree draws folders (alphabetically, recursively) before files.
        fn walk<'a>(d: &Dir<'a>, out: &mut Vec<&'a str>) {
            for sub in d.dirs.values() {
                walk(sub, out);
            }
            out.extend(d.files.iter().map(|e| e.path.as_str()));
        }
        let mut root = Dir::default();
        for f in sorted_group(&entries, Origin::Remote, true) {
            root.insert(f);
        }
        let mut drawn = Vec::new();
        walk(&root, &mut drawn);
        assert_eq!(order(true), drawn);
        assert_eq!(drawn, ["a/b/c.txt", "a/z.txt", "c/d.txt", "a.txt", "b.txt"]);
    }
}


#[cfg(test)]
mod repo_tests {
    use super::*;

    fn file(path: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: Some(Change::Added),
            local_exists: false,
            remote_exists: true,
            body: Body::Binary,
            version: 0,
        }
    }

    fn repos(paths: &[&str]) -> Vec<Repo> {
        paths.iter().map(|p| Repo { path: p.to_string(), branch: Some("main".into()) }).collect()
    }

    #[test]
    fn folder_chains_stop_at_repositories() {
        let files: Vec<FileEntry> = ["back/plugins/x/src/new.rs", "docs/guide/a.md"].map(file).into();
        let mut root = Dir::default();
        for f in &files {
            root.insert(f);
        }
        let repos = repos(&["back", "back/plugins/x"]);
        let chain = |name: &str| {
            let (label, _, path) = folder_chain(name, &root.dirs[name], "", &repos, &Default::default());
            (label, path)
        };
        assert_eq!(chain("back"), ("back".into(), "back/".into()));
        assert_eq!(chain("docs"), ("docs/guide".into(), "docs/guide/".into()));
        let back = &root.dirs["back"];
        let (label, node, path) = folder_chain("plugins", &back.dirs["plugins"], "back/", &repos, &Default::default());
        assert_eq!((label.as_str(), path.as_str()), ("plugins", "back/plugins/"));
        let (label, _, path) = folder_chain("x", &node.dirs["x"], &path, &repos, &Default::default());
        assert_eq!((label.as_str(), path.as_str()), ("x", "back/plugins/x/"));
        assert!(repo_at(&repos, &path).is_some());
    }

    fn drawn_texts(ctx: &egui::Context, draw: impl FnMut(&mut egui::Ui)) -> Vec<(String, f32)> {
        let mut draw = draw;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 800.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| draw(ui));
        out.textures_delta.clear();
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some((t.galley.job.text.clone(), t.pos.x)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn nested_repositories_sit_inside_the_root_one() {
        let files: Vec<FileEntry> = ["README", "api/main.rs", "web/app.js", "tools/cli/run.sh", "docs/a.md"].map(file).into();
        let repos = repos(&["", "api", "web", "tools"]);
        let ctx = egui::Context::default();
        let texts = drawn_texts(&ctx, |ui| {
            let mut root = Dir::default();
            for f in &files {
                root.insert(f);
            }
            let (mut selected, mut acts) = (None, ListActions::default());
            tree_ui(ui, root, "g/", &mut ListCtx { selected: &mut selected, acts: &mut acts, repos: &repos, view: &mut ListView { expand: true, ..ListView::default() }, filtering: None, counts: None, untracked: &Default::default(), gitignored: &Default::default(), gitignored_dirs: &Default::default() });
        });
        let x = |needle: &str| texts.iter().find(|(t, _)| t.contains(needle)).map(|(_, x)| *x).unwrap_or_else(|| panic!("{needle} not in {texts:?}"));
        let root = x("📦 (workspace root)");
        for repo in ["📦 api", "📦 web", "📦 tools"] {
            assert!(x(repo) > root, "{repo}: {texts:?}");
        }
        assert!(x("main.rs") > x("📦 api"), "{texts:?}");
        assert!(x("📁 cli") > x("📦 tools"), "{texts:?}");
        assert!(x("README") > root, "{texts:?}");
    }

    #[test]
    fn folders_follow_the_saved_view() {
        let files: Vec<FileEntry> = ["src/app/deep/main.rs", "README"].map(file).into();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let draw = |view: &mut ListView| {
            drawn_texts(&ctx, |ui| {
                let mut root = Dir::default();
                for f in &files {
                    root.insert(f);
                }
                let (mut selected, mut acts) = (None, ListActions::default());
                let repos = repos(&["", "src/app"]);
                tree_ui(ui, root, "g/", &mut ListCtx { selected: &mut selected, acts: &mut acts, repos: &repos, view, filtering: None, counts: None, untracked: &Default::default(), gitignored: &Default::default(), gitignored_dirs: &Default::default() });
            })
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>()
        };
        let shows = |texts: &[String], needle: &str| texts.iter().any(|t| t.contains(needle));
        let mut view = ListView::default();
        let collapsed = draw(&mut view);
        assert!(shows(&collapsed, "(workspace root)") && !shows(&collapsed, "main.rs") && !shows(&collapsed, "README"), "{collapsed:?}");
        view.set_expand(true);
        let expanded = draw(&mut view);
        assert!(shows(&expanded, "main.rs") && shows(&expanded, "README"), "{expanded:?}");
        view.toggle("g/src/app/");
        let one_closed = draw(&mut view);
        assert!(!shows(&one_closed, "main.rs") && shows(&one_closed, "README"), "{one_closed:?}");
        view.set_expand(false);
        assert!(!shows(&draw(&mut view), "main.rs"));
    }

    #[test]
    fn filtering_opens_the_way_to_the_matches() {
        let files: Vec<FileEntry> = ["src/app/deep/editor.rs", "src/app/deep/other.rs", "docs/guide.md"].map(file).into();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = ListView::default();
        let mut collapsed = std::collections::BTreeSet::new();
        let mut draw = |filter: &str, collapsed: &mut std::collections::BTreeSet<String>| {
            let matching: Vec<&FileEntry> = files.iter().filter(|f| matches_filter(&f.path, filter)).collect();
            drawn_texts(&ctx, |ui| {
                let mut root = Dir::default();
                for f in &matching {
                    root.insert(f);
                }
                let (mut selected, mut acts) = (None, ListActions::default());
                let filtering = (!filter.is_empty()).then_some(&mut *collapsed);
                tree_ui(ui, root, "g/", &mut ListCtx { selected: &mut selected, acts: &mut acts, repos: &[], view: &mut view, filtering, counts: None, untracked: &Default::default(), gitignored: &Default::default(), gitignored_dirs: &Default::default() });
            })
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>()
        };
        let shows = |texts: &[String], needle: &str| texts.iter().any(|t| t.contains(needle));
        let found = draw("editor", &mut collapsed);
        assert!(shows(&found, "editor.rs") && !shows(&found, "other.rs") && !shows(&found, "docs"), "{found:?}");
        collapsed.insert("g/src/app/deep/".into());
        assert!(!shows(&draw("editor", &mut collapsed), "editor.rs"));
        let unfiltered = draw("", &mut collapsed);
        assert!(!shows(&unfiltered, "editor.rs") && shows(&unfiltered, "src/app/deep"), "{unfiltered:?}");
    }

    #[test]
    fn filter_keeps_paths_with_every_word() {
        assert!(matches_filter("src/app/Editor.rs", ""));
        assert!(matches_filter("src/app/Editor.rs", "editor"));
        assert!(matches_filter("src/app/Editor.rs", "app  .RS"));
        assert!(!matches_filter("src/app/Editor.rs", "app view"));
    }

    #[test]
    fn icons_by_extension() {
        assert_eq!(file_icon("src/main.rs"), file_icon("a.RS"));
        assert_ne!(file_icon("a.rs"), file_icon("a.py"));
        assert_eq!(file_icon("Makefile"), PLAIN_FILE_ICON);
        assert_eq!(file_icon("v1.2/README"), PLAIN_FILE_ICON);
        assert_ne!(file_icon("assets/chair.glb"), PLAIN_FILE_ICON);
        assert_ne!(file_icon("sfx/hit.wav"), file_icon("a.png"));
    }

    #[test]
    fn closest_repository() {
        let repos = repos(&["", "a", "a/b"]);
        assert_eq!(repo_of(&repos, "a/b/c").unwrap().path, "a/b");
        assert_eq!(repo_of(&repos, "ab/c").unwrap().path, "");
        assert!(repo_of(&repos[1..], "z").is_none());
    }
}
