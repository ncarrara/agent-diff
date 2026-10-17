use super::*;

const TICK: Duration = Duration::from_secs(1);
const SHOWN_COMMITS: &str = "400";
const PANEL_HEIGHT: f32 = 220.0;
const LEAST_PANEL_HEIGHT: f32 = 120.0;
const LEAST_EXPLORER_HEIGHT: f32 = 160.0;
const HANDLE_HEIGHT: f32 = 6.0;
const LONGEST_INTERVAL: u64 = 3600;
const SHOWN_GRACE: Duration = Duration::from_secs(3);

type Segment = (String, Option<u8>);

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct GitPoll {
    pub(super) fetch_every: u64,
    pub(super) log_every: u64,
}

impl Default for GitPoll {
    fn default() -> Self {
        GitPoll { fetch_every: 5, log_every: 5 }
    }
}

#[derive(Default)]
struct LogState {
    lines: Vec<Vec<Segment>>,
    fetch_error: Option<String>,
    log_error: Option<String>,
    fetched: Option<Instant>,
    worktrees: Vec<crate::worktree::Listed>,
    branches: Vec<crate::worktree::Branch>,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum LogView {
    #[default]
    Log,
    Worktrees,
    Branches,
}

#[derive(Default)]
pub(super) struct GitLog {
    repo: Arc<Mutex<String>>,
    state: Arc<Mutex<LogState>>,
    poll: Arc<Mutex<GitPoll>>,
    drawn_at: Arc<Mutex<Option<Instant>>>,
    alive: Option<Arc<()>>,
    bar_from_top: Option<f32>,
    remove_request: Option<PathBuf>,
    branch_request: Option<(PathBuf, crate::worktree::Branch)>,
    view: LogView,
}

impl GitLog {
    pub(super) fn adopt(&self, poll: GitPoll) {
        *self.poll.lock().unwrap() = poll;
    }

    pub(super) fn poll(&self) -> GitPoll {
        *self.poll.lock().unwrap()
    }

    fn ensure_running(&mut self, ctx: &egui::Context, local_root: &str) {
        if self.alive.is_some() {
            return;
        }
        let alive = Arc::new(());
        let watched = Arc::downgrade(&alive);
        let (repo, state, poll, drawn_at, ctx, root) = (self.repo.clone(), self.state.clone(), self.poll.clone(), self.drawn_at.clone(), ctx.clone(), PathBuf::from(local_root));
        std::thread::spawn(move || {
            let mut logged_at = None;
            let mut repository = Repository::default();
            while watched.upgrade().is_some() {
                if !recently_drawn(&drawn_at) {
                    logged_at = None;
                    std::thread::sleep(TICK);
                    continue;
                }
                let every = *poll.lock().unwrap();
                let dir = root.join(repo.lock().unwrap().as_str());
                let fetched = every.fetch_every > 0 && fetch_if_due(&dir, repository.of(&dir), every.fetch_every, &state);
                if every.fetch_every == 0 {
                    state.lock().unwrap().fetch_error = None;
                }
                if fetched || due(logged_at, every.log_every.max(1)) {
                    read_log(&dir, &state);
                    logged_at = Some(Instant::now());
                    ctx.request_repaint();
                }
                std::thread::sleep(TICK);
            }
        });
        self.alive = Some(alive);
    }

    fn mark_drawn(&self) {
        *self.drawn_at.lock().unwrap() = Some(Instant::now());
    }
}

fn recently_drawn(drawn_at: &Mutex<Option<Instant>>) -> bool {
    drawn_at.lock().unwrap().is_some_and(|at| at.elapsed() < SHOWN_GRACE)
}

#[derive(Default)]
struct Repository {
    dir: PathBuf,
    shared: Arc<Mutex<SharedFetch>>,
}

impl Repository {
    fn of(&mut self, dir: &Path) -> Arc<Mutex<SharedFetch>> {
        if self.dir != dir {
            self.dir = dir.to_path_buf();
            self.shared = shared_fetch(&common_git_dir(dir));
        }
        self.shared.clone()
    }
}

#[derive(Default)]
struct SharedFetch {
    at: Option<Instant>,
    error: Option<String>,
}

static SHARED_FETCHES: std::sync::LazyLock<Mutex<std::collections::HashMap<PathBuf, Arc<Mutex<SharedFetch>>>>> = std::sync::LazyLock::new(Default::default);

fn shared_fetch(git_dir: &Path) -> Arc<Mutex<SharedFetch>> {
    SHARED_FETCHES.lock().unwrap().entry(git_dir.to_path_buf()).or_default().clone()
}

fn common_git_dir(dir: &Path) -> PathBuf {
    let listed = crate::engine::git_at(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    listed.ok().map(|out| PathBuf::from(String::from_utf8_lossy(&out).trim())).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| dir.to_path_buf())
}

fn due(last: Option<Instant>, every: u64) -> bool {
    last.is_none_or(|at| at.elapsed() >= Duration::from_secs(every))
}

fn fetch_if_due(dir: &Path, shared: Arc<Mutex<SharedFetch>>, every: u64, state: &Mutex<LogState>) -> bool {
    let mut repository = shared.lock().unwrap();
    if due(repository.at, every) {
        let fetched = crate::engine::git_at(dir, &["fetch", "--all", "--prune", "--quiet"]);
        repository.error = fetched.err().map(|e| format!("{e:#}"));
        repository.at = Some(Instant::now());
    }
    let mut s = state.lock().unwrap();
    let new_fetch = s.fetched != repository.at;
    s.fetched = repository.at;
    s.fetch_error = repository.error.clone();
    new_fetch
}

fn read_log(dir: &Path, state: &Mutex<LogState>) {
    let log = crate::engine::git_at(dir, &["log", "--oneline", "--all", "--graph", "--decorate", "--color=always", "-n", SHOWN_COMMITS]);
    let worktrees = crate::worktree::list(dir);
    let branches = crate::worktree::local_branches(dir);
    let mut s = state.lock().unwrap();
    s.worktrees = worktrees;
    s.branches = branches;
    match log {
        Ok(out) => {
            s.lines = String::from_utf8_lossy(&out).lines().map(colored_segments).collect();
            s.log_error = None;
        }
        Err(e) => s.log_error = Some(format!("{e:#}")),
    }
}

fn colored_segments(line: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let (mut text, mut color, mut bold) = (String::new(), None::<u8>, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            text.push(c);
            continue;
        }
        let code: String = chars.by_ref().skip_while(|&c| c == '[').take_while(|&c| c != 'm').collect();
        if !text.is_empty() {
            segments.push((std::mem::take(&mut text), shown_color(color, bold)));
        }
        for part in code.split(';') {
            match part.parse::<u8>() {
                Ok(0) | Err(_) => (color, bold) = (None, false),
                Ok(1) => bold = true,
                Ok(22) => bold = false,
                Ok(39) => color = None,
                Ok(n @ 30..=37) => color = Some(n - 30),
                Ok(n @ 90..=97) => color = Some(n - 90 + 8),
                _ => {}
            }
        }
    }
    if !text.is_empty() {
        segments.push((text, shown_color(color, bold)));
    }
    segments
}

fn shown_color(color: Option<u8>, bold: bool) -> Option<u8> {
    color.map(|i| if bold && i < 8 { i + 8 } else { i })
}

fn repo_name(path: &str) -> String {
    if path.is_empty() { "(workspace root)".to_string() } else { path.to_string() }
}

fn repo_picker(ui: &mut egui::Ui, log: &GitLog, repos: &[Repo]) {
    let mut repo = log.repo.lock().unwrap();
    if !repos.iter().any(|r| r.path == *repo)
        && let Some(first) = repos.first()
    {
        *repo = first.path.clone();
    }
    if repos.len() < 2 {
        return;
    }
    egui::ComboBox::from_id_salt("git-log-repo").selected_text(repo_name(&repo)).show_ui(ui, |ui| {
        for r in repos {
            if ui.selectable_label(*repo == r.path, repo_name(&r.path)).clicked() {
                *repo = r.path.clone();
            }
        }
    });
}

fn seconds(ui: &mut egui::Ui, label: &str, value: &mut u64, least: u64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(least..=LONGEST_INTERVAL).suffix(" s"));
    });
}

fn poll_settings(ui: &mut egui::Ui, log: &GitLog) {
    let mut poll = log.poll();
    seconds(ui, "git fetch every", &mut poll.fetch_every, 0);
    ui.weak("0: never fetch");
    seconds(ui, "git log every", &mut poll.log_every, 1);
    log.adopt(poll);
}

fn view_tabs(ui: &mut egui::Ui, log: &mut GitLog) {
    let (worktrees, branches) = {
        let state = log.state.lock().unwrap();
        (state.worktrees.len().saturating_sub(1), state.branches.len())
    };
    let tabs = [(LogView::Log, "git log".to_string()), (LogView::Worktrees, format!("🌿 worktrees ({worktrees})")), (LogView::Branches, format!("⑂ branches ({branches})"))];
    for (view, label) in tabs {
        if ui.selectable_label(log.view == view, RichText::new(label).strong()).clicked() {
            log.view = view;
        }
    }
}

fn log_header(ui: &mut egui::Ui, log: &mut GitLog, repos: &[Repo]) {
    ui.horizontal(|ui| {
        view_tabs(ui, log);
        ui.separator();
        repo_picker(ui, log, repos);
        ui.menu_button("⚙", |ui| poll_settings(ui, log)).response.on_hover_text("How often to fetch and to refresh the log, for every workspace");
        let state = log.state.lock().unwrap();
        match (log.poll().fetch_every, state.fetched) {
            (0, _) => ui.weak(RichText::new("fetch off").small()),
            (_, Some(at)) => ui.weak(RichText::new(format!("fetched {}s ago", at.elapsed().as_secs())).small()),
            (_, None) => ui.weak(RichText::new("fetching…").small()),
        };
        if let Some(e) = &state.fetch_error {
            ui.colored_label(Color32::from_rgb(0xe0, 0xa0, 0x40), RichText::new("fetch failed").small()).on_hover_text(e);
        }
    });
}

fn log_lines(ui: &mut egui::Ui, log: &GitLog) {
    let state = log.state.lock().unwrap();
    if let Some(e) = &state.log_error {
        ui.colored_label(Color32::LIGHT_RED, e);
        return;
    }
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let default = ui.visuals().text_color();
    let row = ui.fonts_mut(|f| f.row_height(&font));
    egui::ScrollArea::both().id_salt("git-log").auto_shrink(false).show_rows(ui, row, state.lines.len(), |ui, rows| {
        for segments in &state.lines[rows] {
            let mut job = egui::text::LayoutJob::default();
            for (text, color) in segments {
                let color = color.map_or(default, |i| ansi_index_color(i, default));
                job.append(text, 0.0, egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
            }
            ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend));
        }
    });
}

fn resize_handle(ui: &mut egui::Ui) -> Option<f32> {
    let (rect, handle) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HANDLE_HEIGHT), egui::Sense::drag());
    if handle.hovered() || handle.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        ui.painter().hline(rect.x_range(), rect.center().y, ui.visuals().widgets.hovered.fg_stroke);
    }
    handle.dragged().then(|| handle.drag_delta().y)
}

fn log_height(available: f32, bar_from_top: f32) -> f32 {
    let least = LEAST_EXPLORER_HEIGHT + LEAST_PANEL_HEIGHT;
    if available <= least {
        return available * LEAST_PANEL_HEIGHT / least;
    }
    available - bar_from_top.clamp(LEAST_EXPLORER_HEIGHT, available - LEAST_PANEL_HEIGHT)
}

impl GitLog {
    pub(super) fn take_remove_request(&mut self) -> Option<PathBuf> {
        self.remove_request.take()
    }

    pub(super) fn take_branch_request(&mut self) -> Option<(PathBuf, crate::worktree::Branch)> {
        self.branch_request.take()
    }

    fn repo_dir(&self, local_root: &str) -> PathBuf {
        Path::new(local_root).join(self.repo.lock().unwrap().as_str())
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn worktree_list(ui: &mut egui::Ui, log: &mut GitLog, local_root: &str) {
    let worktrees = log.state.lock().unwrap().worktrees.clone();
    if worktrees.len() < 2 {
        ui.weak("No other worktree: 🌿 → New worktree… in the top bar adds one.");
        return;
    }
    egui::ScrollArea::vertical().id_salt(("worktree-list", local_root)).auto_shrink(false).show(ui, |ui| {
        for worktree in &worktrees {
            if worktree_row(ui, worktree, Path::new(local_root)) {
                log.remove_request = Some(worktree.path.clone());
            }
        }
    });
}

fn branch_list(ui: &mut egui::Ui, log: &mut GitLog, local_root: &str) {
    let branches = log.state.lock().unwrap().branches.clone();
    egui::ScrollArea::vertical().id_salt(("branch-list", local_root)).auto_shrink(false).show(ui, |ui| {
        for branch in &branches {
            if branch_row(ui, branch) {
                log.branch_request = Some((log.repo_dir(local_root), branch.clone()));
            }
        }
    });
}

fn branch_row(ui: &mut egui::Ui, branch: &crate::worktree::Branch) -> bool {
    let mut delete = false;
    ui.horizontal(|ui| {
        let button = ui.add_enabled(branch.checked_out_in.is_none(), egui::Button::new(RichText::new("🗑").small()));
        let why_not = format!("Checked out in {}: switch it to another branch or remove that worktree first", branch.checked_out_in.as_deref().unwrap_or(Path::new("")).display());
        delete = button.on_hover_text("Delete this branch…").on_disabled_hover_text(why_not).clicked();
        let mut name = RichText::new(format!("⑂ {}", branch.name)).color(REPO);
        if branch.current {
            name = name.strong();
        }
        ui.label(name);
        if let Some(upstream) = &branch.upstream {
            ui.weak(RichText::new(format!("→ {upstream}")).small());
        }
    });
    delete
}

fn worktree_row(ui: &mut egui::Ui, worktree: &crate::worktree::Listed, shown: &Path) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        let deletable = !worktree.main && !worktree.missing;
        let button = ui.add_enabled(deletable, egui::Button::new(RichText::new("🗑").small()));
        let why_not = if worktree.main { "The main worktree holds the repository: it is not removed from here" } else { "Its folder is gone: git worktree prune forgets it" };
        remove = button.on_hover_text("Remove this worktree…").on_disabled_hover_text(why_not).clicked();
        let branch = worktree.branch.as_deref().unwrap_or("(detached)");
        let mut name = RichText::new(format!("🌿 {branch}")).color(REPO);
        if same_folder(&worktree.path, shown) {
            name = name.strong();
        }
        ui.label(name).on_hover_text(worktree.path.display().to_string());
        let place = if worktree.missing { "missing".to_string() } else { worktree.path.display().to_string() };
        ui.weak(RichText::new(place).small());
    });
    remove
}

pub(super) fn git_log_panel(ui: &mut egui::Ui, log: &mut GitLog, local_root: &str, repos: &[Repo]) {
    log.mark_drawn();
    log.ensure_running(ui.ctx(), local_root);
    let available = ui.available_height();
    let wanted = log.bar_from_top.unwrap_or(available - PANEL_HEIGHT);
    let below_bar = log_height(available, wanted);
    let mut dragged = None;
    egui::Panel::bottom(egui::Id::new(("git-log-panel", local_root))).resizable(false).exact_size(below_bar).show(ui, |ui| {
        dragged = resize_handle(ui);
        log_header(ui, log, repos);
        match log.view {
            LogView::Log => log_lines(ui, log),
            LogView::Worktrees => worktree_list(ui, log, local_root),
            LogView::Branches => branch_list(ui, log, local_root),
        }
    });
    log.bar_from_top = Some(match dragged {
        Some(delta) => available - log_height(available, available - below_bar + delta),
        None => wanted,
    });
    ui.ctx().request_repaint_after(Duration::from_secs(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_git_colors() {
        let line = "* \u{1b}[33m1a2b3c4\u{1b}[m \u{1b}[33m(\u{1b}[1;36mHEAD -> \u{1b}[m\u{1b}[1;32mmain\u{1b}[m\u{1b}[33m)\u{1b}[m fix";
        let segments = colored_segments(line);
        assert_eq!(segments.iter().map(|(t, _)| t.as_str()).collect::<String>(), "* 1a2b3c4 (HEAD -> main) fix");
        assert!(segments.contains(&("1a2b3c4".to_string(), Some(3))));
        assert!(segments.contains(&("HEAD -> ".to_string(), Some(14))));
        assert!(segments.contains(&("main".to_string(), Some(10))));
        assert!(segments.contains(&(" fix".to_string(), None)));
    }

    fn repo_with_a_worktree(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("agent-diff-git-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("proj");
        std::fs::create_dir_all(&main).unwrap();
        let git = |dir: &Path, args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success());
        git(&main, &["init", "-q"]);
        git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "first"]);
        let linked = root.join("proj.wt/feat");
        git(&main, &["worktree", "add", "-q", "-b", "feat", linked.to_str().unwrap()]);
        (main, linked)
    }

    #[test]
    fn worktrees_of_a_repository_share_one_fetch() {
        let (main, linked) = repo_with_a_worktree("shared");
        assert_eq!(common_git_dir(&main), common_git_dir(&linked));
        let (main_state, linked_state) = (Mutex::new(LogState::default()), Mutex::new(LogState::default()));
        let (mut main_repo, mut linked_repo) = (Repository::default(), Repository::default());
        assert!(fetch_if_due(&main, main_repo.of(&main), 60, &main_state));
        assert!(linked_repo.of(&linked).lock().unwrap().at.is_some());
        assert!(fetch_if_due(&linked, linked_repo.of(&linked), 60, &linked_state));
        assert_eq!(main_state.lock().unwrap().fetched, linked_state.lock().unwrap().fetched);
        assert!(!fetch_if_due(&main, main_repo.of(&main), 60, &main_state));
    }

    #[test]
    fn a_log_not_drawn_lately_is_paused() {
        assert!(!recently_drawn(&Mutex::new(None)));
        assert!(recently_drawn(&Mutex::new(Some(Instant::now()))));
        assert!(!recently_drawn(&Mutex::new(Instant::now().checked_sub(SHOWN_GRACE * 2))));
    }

    #[test]
    fn the_log_and_the_list_above_it_keep_their_least_heights() {
        assert_eq!(log_height(800.0, 580.0), 220.0);
        assert_eq!(log_height(800.0, 790.0), LEAST_PANEL_HEIGHT);
        assert_eq!(log_height(800.0, 20.0), 800.0 - LEAST_EXPLORER_HEIGHT);
        let cramped = (LEAST_EXPLORER_HEIGHT + LEAST_PANEL_HEIGHT) / 2.0;
        assert_eq!(log_height(cramped, 0.0), LEAST_PANEL_HEIGHT / 2.0);
    }

    #[test]
    fn intervals_come_due() {
        assert!(due(None, 5));
        assert!(!due(Some(Instant::now()), 5));
    }
}
