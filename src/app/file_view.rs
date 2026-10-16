use super::*;

/// Highlighted lines of the selected file, cached for one (path, version).
pub(super) struct HlFile {
    pub(super) key: (String, u64),
    pub(super) local: Vec<Line>,
    pub(super) remote: Vec<Line>,
}

/// What the user asked for in the file view, besides sync actions.
#[derive(Default)]
pub(super) struct Explain {
    pub(super) patch: String,
    pub(super) whole_file: bool,
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn hunk_patch(h: &Hunk, local: &str, remote: &str) -> String {
    let lines = |text: &str, range: &std::ops::Range<usize>| text.split_inclusive('\n').skip(range.start).take(range.len()).collect::<String>();
    as_patch(h, &lines(local, &h.local), &lines(remote, &h.remote))
}

fn whole_patch(e: &FileEntry) -> Option<String> {
    match &e.body {
        Body::Text(texts) if !texts.diff().hunks.is_empty() => {
            let TextDiff { hunks, local, remote } = texts.diff();
            Some(hunks.iter().map(|h| hunk_patch(h, local, remote)).collect())
        }
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct ViewOut {
    /// A change, as a diff, to ask Claude about.
    pub(super) explain: Option<Explain>,
    /// Edit the local file, the cursor on this line (0-based).
    pub(super) edit_at: Option<usize>,
    pub(super) edit_remote_at: Option<usize>,
    pub(super) current: Option<usize>,
    pub(super) step_file: Option<isize>,
    pub(super) accept: bool,
}

#[derive(Default)]
pub(super) struct ChangeFocus {
    path: String,
    pub(super) index: usize,
    pending: Option<(u64, Instant)>,
    scroll: bool,
    land_on_last: bool,
}

impl ChangeFocus {
    fn follow(&mut self, e: &FileEntry, changes: usize) -> bool {
        if self.path != e.path {
            let (scroll, last) = (self.scroll, self.land_on_last);
            *self = ChangeFocus { path: e.path.clone(), scroll, ..Default::default() };
            if last {
                self.index = changes.saturating_sub(1);
            }
        }
        self.land_on_last = false;
        self.index = self.index.min(changes.saturating_sub(1));
        let arrived = self.pending.is_some_and(|(version, _)| version != e.version);
        if arrived {
            self.pending = None;
        }
        arrived || std::mem::take(&mut self.scroll)
    }

    fn step(&mut self, forward: bool, changes: usize) -> Option<isize> {
        self.scroll = true;
        match forward {
            true if self.index + 1 < changes => self.index += 1,
            false if self.index > 0 => self.index -= 1,
            _ => {
                self.land_on_last = !forward;
                return Some(if forward { 1 } else { -1 });
            }
        }
        None
    }

    pub(super) fn accept(&mut self, e: &FileEntry) -> Option<Cmd> {
        let waiting = self.pending.is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(3));
        if e.origin == Origin::Synced || self.path != e.path || waiting {
            return None;
        }
        self.pending = Some((e.version, Instant::now()));
        Some(Cmd::Apply(vec![(e.path.clone(), e.version, accept_action(e, self.index), e.rename.clone())]))
    }
}

pub(super) fn step_change(focus: &mut ChangeFocus, e: &FileEntry, forward: bool) -> Option<isize> {
    focus.step(forward, change_count(e))
}

fn change_count(e: &FileEntry) -> usize {
    match &e.body {
        Body::Text(texts) => texts.diff().hunks.len(),
        _ => 0,
    }
}

fn accept_action(e: &FileEntry, i: usize) -> Action {
    let claude = e.origin != Origin::Local;
    let per_change = e.local_exists && e.remote_exists && change_count(e) > 0;
    match (per_change, claude) {
        (true, true) => Action::TakeRemoteHunk(i),
        (true, false) => Action::TakeLocalHunk(i),
        (false, true) => Action::TakeRemoteFile,
        (false, false) => Action::TakeLocalFile,
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub(super) fn accept_key_pressed(ui: &egui::Ui) -> bool {
    let free = ui.memory(|m| m.focused().is_none() && m.top_modal_layer().is_none());
    free && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F7))
}

pub(super) fn file_view(
    ui: &mut egui::Ui,
    e: &FileEntry,
    hl: Option<&HlFile>,
    busy: bool,
    full: &mut bool,
    jumped_to: &mut Option<String>,
    focus: &mut ChangeFocus,
    preview: Option<&mut (dyn Preview + 'static)>,
    out: &mut ViewOut,
) -> Option<Cmd> {
    let mut cmd = None;
    let scroll_to_current = focus.follow(e, change_count(e));
    let current = focus.index;
    let item = |a| Cmd::Apply(vec![(e.path.clone(), e.version, a, e.rename.clone())]);
    let (title, color) = origin_label(e.origin);
    ui.label(RichText::new(title).color(color)).on_hover_text(&e.path);
    if let Some(r) = &e.rename {
        let side = if e.origin == Origin::Remote { "on the remote" } else { "locally" };
        let what = if r.similarity == 100 { "moved".to_string() } else { format!("moved and modified, {}% similar", r.similarity) };
        ui.label(
            RichText::new(format!("Renamed {side}: {} → {}  ({what})", r.from, e.path))
                .color(change_color(Some(Change::Renamed))),
        );
    }
    // Unchanged files are read-only and always shown whole.
    let mut always = true;
    let full: &mut bool = if e.origin == Origin::Synced { &mut always } else { full };
    let editable = e.local_exists && matches!(e.body, Body::Text(_));
    let first_change = match &e.body {
        Body::Text(texts) => texts.diff().hunks.first().map_or(0, |h| h.local.start),
        _ => 0,
    };
    let edit_help = "Edit: your local file here (or double-click a line)";
    if e.origin == Origin::Synced {
        ui.horizontal(|ui| {
            ui.weak(if e.remote_exists { "Identical locally and on the remote." } else { "🙈 Gitignored: never synced, only your local copy." });
            if editable && ui.button("✏").on_hover_text(edit_help).clicked() {
                out.edit_at = Some(0);
            }
        });
    } else {
        ui.horizontal_wrapped(|ui| {
            let big = |text: &str, fill| egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(fill);
            let renamed_on = e.rename.as_ref().map(|_| e.origin);
            let (pull, pull_help) = match (e.local_exists, e.remote_exists) {
                _ if renamed_on == Some(Origin::Remote) => {
                    ("⬇ Pull rename", "Move your local file to the new path and take the remote content (git add / git rm)")
                }
                _ if renamed_on.is_some() => ("⬇ Undo rename locally", "Move your local file back to the old path, with the remote content"),
                (_, false) => ("🗑 Delete local file", "The file was deleted on the remote: delete it locally too"),
                (false, true) => ("➕ Create local file", "New file on the remote: create it locally (and git add it)"),
                (true, true) => ("⬇ Pull file", "Overwrite your local file with the remote version"),
            };
            let (push, push_help) = match (e.local_exists, e.remote_exists) {
                _ if renamed_on == Some(Origin::Local) => ("⬆ Push rename", "Move the remote file to your new path, with your content"),
                _ if renamed_on.is_some() => ("⬆ Undo rename on remote", "Move the remote file back to the old path, with your content"),
                (false, _) => ("🗑 Delete remote file", "The file does not exist locally: delete it on the remote too"),
                (true, false) => ("➕ Create remote file", "The file does not exist on the remote: create it there"),
                (true, true) => ("⬆ Push file", "Overwrite the remote file with your local version"),
            };
            if ui.add_enabled(!busy, big(pull, Color32::from_rgb(0x2e, 0x8b, 0x46))).on_hover_text(pull_help).clicked() {
                cmd = Some(item(Action::TakeRemoteFile));
            }
            if ui.add_enabled(!busy, big(push, Color32::from_rgb(0xc0, 0x39, 0x2b))).on_hover_text(push_help).clicked() {
                cmd = Some(item(Action::TakeLocalFile));
            }
            ui.separator();
            let accept = if e.origin == Origin::Local { "push" } else { "take" };
            let accept_button = egui::Button::new(format!("⏵ {} change {}/{} and go on", capitalized(accept), current + 1, change_count(e).max(1)));
            if ui.add_enabled(!busy, accept_button).on_hover_text("Use F7 too").clicked() {
                out.accept = true;
            }
            ui.separator();
            ui.selectable_value(full, false, "Changes only");
            if ui.selectable_value(full, true, "Full file").clicked() {
                *jumped_to = None;
            }
            if editable {
                ui.separator();
                if ui.button("✏").on_hover_text(edit_help).clicked() {
                    out.edit_at = Some(first_change);
                }
            }
            if e.remote_exists && matches!(e.body, Body::Text(_)) {
                let first_remote_change = match &e.body {
                    Body::Text(texts) => texts.diff().hunks.first().map_or(0, |h| h.remote.start),
                    _ => 0,
                };
                let edit_remote = egui::Button::new(RichText::new("✏").color(CLAUDE));
                if ui.add(edit_remote).on_hover_text("Edit remote: Claude's version on the remote; saving pushes it there").clicked() {
                    out.edit_remote_at = Some(first_remote_change);
                }
            }
            if let Some(patch) = whole_patch(e) {
                let ask = egui::Button::new(RichText::new("?").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0x4f, 0x6f, 0xb0));
                if ui.add(ask).on_hover_text("Explain all: ask Claude to explain all the changes of this file, in a Claude terminal").clicked() {
                    out.explain = Some(Explain { patch, whole_file: true });
                }
            }
        });
    }
    ui.separator();

    if let Some(preview) = preview {
        preview.ui(ui);
        return cmd;
    }
    let (Body::Text(texts), Some(hl)) = (&e.body, hl) else {
        ui.label(if e.origin == Origin::Synced {
            "Binary or very large file."
        } else {
            "Binary or very large file: use the Pull / Push file buttons above."
        });
        return cmd;
    };
    let TextDiff { hunks, local, remote } = texts.diff();
    let exists = (e.local_exists, e.remote_exists);
    let n = hunks.len();
    // Full-file view: an overview strip on the right marks where the changes
    // are; clicking or dragging on it scrolls there.
    // In that view the strip *is* the vertical scroll bar: egui's own bars are
    // hidden and a thin horizontal bar is drawn at the bottom instead.
    // Unchanged files have no changes to mark or act on: no overview strip
    // (so egui's own scroll bars) and no action column.
    let overview = *full && e.origin != Origin::Synced;
    let gutter = if e.origin == Origin::Synced { 0.0 } else { ACTIONS_WIDTH };
    let (strip_width, hbar_height) = if overview { (14.0, 10.0) } else { (0.0, 0.0) };
    let avail = ui.available_rect_before_wrap();
    let main = avail
        .with_max_x(avail.right() - strip_width - if overview { 2.0 } else { 0.0 })
        .with_max_y(avail.bottom() - hbar_height);
    let strip = egui::Rect::from_x_y_ranges(avail.right() - strip_width..=avail.right(), main.y_range());
    let hbar = egui::Rect::from_x_y_ranges(main.x_range(), main.bottom() + 2.0..=avail.bottom());
    let jump_id = egui::Id::new(("overview-jump", &e.path));
    let hjump_id = egui::Id::new(("hbar-jump", &e.path));
    let jump: Option<f32> = ui.ctx().data_mut(|d| d.remove_temp(jump_id));
    let scroll_keys = scroll_keys(ui);
    let hjump: Option<f32> = ui.ctx().data_mut(|d| d.remove_temp(hjump_id));
    // Each change's vertical span in the scrolled content, and its color.
    let mut marks: Vec<(f32, f32, Color32)> = Vec::new();

    let scrolled = ui
        .scope_builder(egui::UiBuilder::new().max_rect(main), |ui| {
            // Each file keeps its own scroll position.
            let mut area = egui::ScrollArea::both().id_salt(&e.path).auto_shrink(false);
            if overview {
                area = area.scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
            }
            if let Some(y) = jump {
                area = area.vertical_scroll_offset(y);
            }
            if let Some(x) = hjump {
                area = area.horizontal_scroll_offset(x);
            }
            area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let chunk = |ui: &mut egui::Ui, i, h, context, out: &mut ViewOut| {
                    let focus = Focus { current: i == current, scroll: i == current && scroll_to_current };
                    chunk_ui(ui, i, n, h, hl, (local, remote), exists, e.rename.is_some(), busy, context, focus, out)
                };
                if !*full {
                    for (i, h) in hunks.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);
                        }
                        if let Some(a) = chunk(ui, i, h, true, out) {
                            cmd = Some(item(a));
                        }
                    }
                    return;
                }
                let search_jump = ui.ctx().data(|d| d.get_temp::<(usize, u8)>(egui::Id::new(JUMP_ID)));
                // Full file: local lines, with each change shown in place.
                let content_top = ui.min_rect().top();
                let mut next = 0;
                let mut hs = hunks.iter().enumerate().peekable();
                loop {
                    while let Some((i, h)) = hs.next_if(|(_, h)| h.local.start <= next) {
                        let top = ui.cursor().top();
                        if let Some(a) = chunk(ui, i, h, false, out) {
                            cmd = Some(item(a));
                        }
                        let bottom = ui.cursor().top();
                        let color = match (h.local.is_empty(), h.remote.is_empty()) {
                            (true, _) => Color32::from_rgb(0x2e, 0x8b, 0x46),
                            (_, true) => Color32::from_rgb(0xc0, 0x39, 0x2b),
                            _ => Color32::from_rgb(0xe0, 0xa0, 0x30),
                        };
                        marks.push((top - content_top, bottom - content_top, color));
                        // Opening a file (or switching to this view) jumps to its first change.
                        if i == 0 && jumped_to.as_deref() != Some(e.path.as_str()) {
                            // A narrow rect at the left edge: scroll vertically, and
                            // horizontally back to the start so the buttons show.
                            scroll_to_start(ui, ui.max_rect().left(), top);
                            *jumped_to = Some(e.path.clone());
                        }
                        if let Some((line, frames_left)) = search_jump
                            && (h.local.contains(&line) || h.local.start == line)
                        {
                            let left = ui.max_rect().left();
                            jump_to_match(ui, egui::Rect::from_x_y_ranges(left..=left + 1.0, top..=bottom), line, frames_left);
                        }
                        next = next.max(h.local.end);
                    }
                    let Some(line) = hl.local.get(next) else { break };
                    let shown = row(ui, gutter, next + 1, line);
                    if shown.double_clicked() && editable {
                        out.edit_at = Some(next);
                    }
                    if let Some((line, frames_left)) = search_jump
                        && line == next
                    {
                        jump_to_match(ui, shown.rect, line, frames_left);
                    }
                    next += 1;
                }
            })
        })
        .inner;

    let content_h = scrolled.content_size.y;
    if let Some(keys) = scroll_keys {
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        let page = (scrolled.inner_rect.height() - row_h).max(row_h);
        let delta = keys.lines * row_h + keys.pages * page;
        let max = (content_h - scrolled.inner_rect.height()).max(0.0);
        ui.ctx().data_mut(|d| d.insert_temp(jump_id, (scrolled.state.offset.y + delta).clamp(0.0, max)));
        ui.ctx().request_repaint();
    }
    if overview && content_h > 0.0 {
        let to_strip = |y: f32| strip.top() + (y / content_h).clamp(0.0, 1.0) * strip.height();
        let painter = ui.painter();
        painter.rect_filled(strip, 2.0, ui.visuals().faint_bg_color);
        for &(top, bottom, color) in &marks {
            let (y0, y1) = (to_strip(top), to_strip(bottom));
            let y1 = y1.max(y0 + 3.0);
            painter.rect_filled(egui::Rect::from_x_y_ranges(strip.x_range(), y0..=y1), 1.0, color);
        }
        // The part of the file currently on screen.
        let view_top = scrolled.state.offset.y;
        let view = egui::Rect::from_x_y_ranges(strip.x_range(), to_strip(view_top)..=to_strip(view_top + scrolled.inner_rect.height()));
        painter.rect_filled(view, 2.0, ui.visuals().text_color().gamma_multiply(0.15));
        painter.rect_stroke(view, 2.0, egui::Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.4)), egui::StrokeKind::Inside);

        let resp = ui.interact(strip, egui::Id::new(("overview", &e.path)), egui::Sense::click_and_drag());
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            // Center the clicked spot in the view.
            let y = (pos.y - strip.top()) / strip.height() * content_h - scrolled.inner_rect.height() / 2.0;
            let max = (content_h - scrolled.inner_rect.height()).max(0.0);
            ui.ctx().data_mut(|d| d.insert_temp(jump_id, y.clamp(0.0, max)));
            ui.ctx().request_repaint();
        }
        resp.on_hover_text("Changes in this file: click or drag to scroll there");

        // Horizontal bar, only when lines are wider than the view.
        let (content_w, view_w) = (scrolled.content_size.x, scrolled.inner_rect.width());
        if content_w > view_w + 1.0 {
            let painter = ui.painter();
            painter.rect_filled(hbar, 3.0, ui.visuals().faint_bg_color);
            let to_bar = |x: f32| hbar.left() + (x / content_w).clamp(0.0, 1.0) * hbar.width();
            let left = scrolled.state.offset.x;
            let thumb = egui::Rect::from_x_y_ranges(to_bar(left)..=to_bar(left + view_w), hbar.y_range());
            painter.rect_filled(thumb, 3.0, ui.visuals().text_color().gamma_multiply(0.35));
            let resp = ui.interact(hbar, egui::Id::new(("hbar", &e.path)), egui::Sense::click_and_drag());
            if (resp.clicked() || resp.dragged())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let x = (pos.x - hbar.left()) / hbar.width() * content_w - view_w / 2.0;
                ui.ctx().data_mut(|d| d.insert_temp(hjump_id, x.clamp(0.0, content_w - view_w)));
                ui.ctx().request_repaint();
            }
        }
    }
    if let Some(i) = out.current {
        focus.index = i;
    }
    cmd
}

struct ScrollKeys {
    lines: f32,
    pages: f32,
}

fn scroll_keys(ui: &egui::Ui) -> Option<ScrollKeys> {
    let free = ui.memory(|m| m.focused().is_none() && m.top_modal_layer().is_none());
    if !free {
        return None;
    }
    let mut keys = ScrollKeys { lines: 0.0, pages: 0.0 };
    ui.input_mut(|i| {
        for (key, lines, pages) in [(egui::Key::ArrowDown, 1.0, 0.0), (egui::Key::ArrowUp, -1.0, 0.0), (egui::Key::PageDown, 0.0, 1.0), (egui::Key::PageUp, 0.0, -1.0)] {
            let count = i.count_and_consume_key(egui::Modifiers::NONE, key) as f32;
            keys.lines += lines * count;
            keys.pages += pages * count;
        }
    });
    (keys.lines != 0.0 || keys.pages != 0.0).then_some(keys)
}

#[derive(Clone, Copy)]
pub(super) struct Focus {
    current: bool,
    scroll: bool,
}

const ROOM_ABOVE_CHANGE: f32 = 28.0;

fn scroll_to_start(ui: &egui::Ui, left: f32, top: f32) {
    let start = egui::Rect::from_x_y_ranges(left..=left + 1.0, top - ROOM_ABOVE_CHANGE..=top);
    ui.scroll_to_rect(start, Some(egui::Align::Min));
}

fn jump_to_match(ui: &egui::Ui, rect: egui::Rect, line: usize, frames_left: u8) {
    ui.scroll_to_rect(rect, Some(egui::Align::Center));
    ui.ctx().data_mut(|d| match frames_left {
        0 | 1 => d.remove::<(usize, u8)>(egui::Id::new(JUMP_ID)),
        left => {
            d.insert_temp(egui::Id::new(JUMP_ID), (line, left - 1));
        }
    });
    ui.ctx().request_repaint();
}

/// Width of the action column on the left of the code.
pub(super) const ACTIONS_WIDTH: f32 = 136.0;

/// A line outside any change: empty action column (`gutter` wide), then the code.
pub(super) fn row(ui: &mut egui::Ui, gutter: f32, number: usize, line: &Line) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(gutter);
        code_line(ui, number, " ", line, None)
    })
    .inner
}

/// One code line: gutter (line number and sign), syntax-colored code, and an
/// optional full-width diff background.
pub(super) fn code_line(ui: &mut egui::Ui, number: usize, sign: &str, line: &Line, bg: Option<Color32>) -> egui::Response {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let fmt = |color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    let pal = palette(ui.ctx());
    let gutter = if bg.is_some() { pal.on_diff.gamma_multiply(0.7) } else { pal.gutter };
    job.append(&format!("{number:>5} {sign} "), 0.0, fmt(gutter));
    let code_start = job.text.len();
    for (color, text) in line {
        job.append(text, 0.0, fmt(*color));
    }
    if let Some(query) = current_query(ui.ctx()) {
        let is_current = sign != "+" && current_line(ui.ctx()) == Some(number - 1);
        let whole = code_start..job.text.len();
        job = highlight_text(job, code_start, &query, is_current.then_some(whole));
    }
    let bg_slot = ui.painter().add(egui::Shape::Noop);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let resp = ui.add(egui::Label::new(galley.clone()).extend());
    note_hovered_word(ui, &resp, &galley, code_start);
    if let Some(bg) = bg {
        // Reach down to the next line (plus a pixel for rounding) so
        // consecutive changed lines form one solid band, without seams. The
        // next line's own text is painted on top of the overlap.
        let mut rect = resp.rect.with_max_x(resp.rect.right().max(ui.max_rect().right()));
        rect.max.y += ui.spacing().item_spacing.y + 1.0;
        ui.painter().set(bg_slot, egui::Shape::rect_filled(rect, 0.0, bg));
    }
    resp
}

fn note_hovered_word(ui: &egui::Ui, resp: &egui::Response, galley: &egui::Galley, code_start: usize) {
    let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| resp.rect.contains(*p)) else { return };
    let cursor = galley.cursor_from_pos(pointer - resp.rect.min);
    let text = galley.text();
    let byte = text.char_indices().nth(cursor.index.0).map_or(text.len(), |(b, _)| b);
    let Some(word) = text.get(code_start..).and_then(|code| word_at(code, byte.saturating_sub(code_start))).filter(|_| byte >= code_start) else { return };
    let (command, clicked) = ui.input(|i| (i.modifiers.command, i.pointer.primary_clicked()));
    if command {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(HOVERED_WORD_ID), Some(word.clone()));
        if command && clicked {
            d.insert_temp(egui::Id::new(CLICKED_WORD_ID), Some(word));
        }
    });
}

/// One change: its buttons in the action column, next to its lines, with a
/// bar spanning the change's height. With `context`, the surrounding
/// unchanged lines are shown too (the full-file view has them already).
/// `out.explain` is set to the change as a diff when its "?" button is
/// clicked; `out.edit_at` when one of its lines is double-clicked.
#[allow(clippy::too_many_arguments)]
pub(super) fn chunk_ui(
    ui: &mut egui::Ui,
    i: usize,
    n: usize,
    h: &Hunk,
    hl: &HlFile,
    (local, remote): (&str, &str),
    (local_exists, remote_exists): (bool, bool),
    renamed: bool,
    busy: bool,
    context: bool,
    focus: Focus,
    out: &mut ViewOut,
) -> Option<Action> {
    let mut action = None;
    let pal = palette(ui.ctx());
    let mut info = format!("Change {}/{n}  {}", i + 1, h.header());
    if renamed {
        info += "\nThe file was renamed: taking a side also moves the file to that side's path";
    }
    let take_fill = Color32::from_rgb(0x2e, 0x8b, 0x46);
    let push_fill = Color32::from_rgb(0xc0, 0x39, 0x2b);
    ui.add_space(3.0);
    let resp = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // Room for the action column; its buttons are drawn afterwards so they
        // can stay on screen (and on top) when the code scrolls sideways.
        let (column, _) = ui.allocate_exact_size(egui::vec2(ACTIONS_WIDTH, 26.0), egui::Sense::hover());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let empty = Line::new();
            for &(kind, idx) in &h.lines {
                let (sign, line, bg) = match kind {
                    LineKind::Context if !context => continue,
                    LineKind::Context => (" ", hl.local.get(idx), None),
                    LineKind::Local => ("−", hl.local.get(idx), Some(pal.red)),
                    LineKind::Remote => ("+", hl.remote.get(idx), Some(pal.green)),
                };
                // Remote lines are not in the local file: edit where they would go.
                let at = if kind == LineKind::Remote { h.local.start } else { idx };
                let clicked = code_line(ui, idx + 1, sign, line.unwrap_or(&empty), bg);
                if clicked.double_clicked() && local_exists {
                    out.edit_at = Some(at);
                } else if clicked.clicked() {
                    out.current = Some(i);
                }
            }
        });
        column
    });
    if focus.current {
        let outline = resp.response.rect.expand2(egui::vec2(0.0, 2.0)).with_max_x(ui.max_rect().right().max(resp.response.rect.right()));
        ui.painter().rect_stroke(outline, 3.0, egui::Stroke::new(1.5, ui.visuals().selection.stroke.color), egui::StrokeKind::Outside);
    }
    if focus.scroll {
        let chunk = resp.response.rect;
        scroll_to_start(ui, chunk.left(), chunk.top());
    }
    // Pin the column to the left edge of the visible area, over the code.
    let column = resp.inner;
    let shift = (ui.clip_rect().left() - column.left()).max(0.0);
    let sticky = column.translate(egui::vec2(shift, 0.0)).with_max_y(resp.response.rect.bottom());
    ui.painter().rect_filled(sticky, 0.0, ui.visuals().panel_fill);
    let bar = egui::Rect::from_min_max(sticky.left_top(), egui::pos2(sticky.left() + 3.0, sticky.bottom()));
    ui.painter().rect_filled(bar, 1.0, take_fill);
    let buttons = egui::UiBuilder::new()
        .max_rect(sticky.with_min_x(sticky.left() + 6.0))
        .layout(egui::Layout::left_to_right(egui::Align::Min));
    // A detached child: drawn over the change without moving the layout
    // cursor (`scope_builder` would move it back up to the buttons' bottom
    // and make the next lines overlap this change).
    {
        let ui = &mut ui.new_child(buttons);
        ui.spacing_mut().item_spacing.x = 4.0;
        let big = |text: &str, fill| {
            egui::Button::new(RichText::new(text).color(Color32::WHITE).strong().size(16.0))
                .fill(fill)
                .corner_radius(4.0)
                .min_size(egui::vec2(28.0, 24.0))
        };
        // With the file on one side only, the change is the whole file: take
        // and push create or delete it.
        let (take, take_help, push, push_help) = match (local_exists, remote_exists) {
            (true, true) => (
                Action::TakeRemoteHunk(i),
                "Take remote: apply only this change to your local file",
                Action::TakeLocalHunk(i),
                "Push local: overwrite only this change on the remote with your lines",
            ),
            (false, _) => (
                Action::TakeRemoteFile,
                "Take remote: create this file locally (and git add it)",
                Action::TakeLocalFile,
                "Push local: the file does not exist locally, so DELETE it on the remote",
            ),
            (true, false) => (
                Action::TakeRemoteFile,
                "Take remote: the file was deleted on the remote, so DELETE it locally",
                Action::TakeLocalFile,
                "Push local: create this file on the remote",
            ),
        };
        if ui.add_enabled(!busy, big("⬇", take_fill)).on_hover_text(format!("{info}\n{take_help}")).clicked() {
            action = Some(take);
        }
        if ui.add_enabled(!busy, big("⬆", push_fill)).on_hover_text(format!("{info}\n{push_help}")).clicked() {
            action = Some(push);
        }
        ui.menu_button(RichText::new("📋").size(16.0), |ui| {
            let remote_text: String = remote.split_inclusive('\n').skip(h.remote.start).take(h.remote.len()).collect();
            let local_text: String = local.split_inclusive('\n').skip(h.local.start).take(h.local.len()).collect();
            if ui.add_enabled(!remote_text.is_empty(), egui::Button::new("Copy remote lines (Claude)")).clicked() {
                ui.ctx().copy_text(remote_text.clone());
            }
            if ui.add_enabled(!local_text.is_empty(), egui::Button::new("Copy local lines (yours)")).clicked() {
                ui.ctx().copy_text(local_text.clone());
            }
            if ui.button("Copy as diff").clicked() {
                ui.ctx().copy_text(as_patch(h, &local_text, &remote_text));
            }
        })
        .response
        .on_hover_text(format!("{info}\nCopy this change"));
        if ui
            .add(big("?", Color32::from_rgb(0x4f, 0x6f, 0xb0)))
            .on_hover_text(format!("{info}\nAsk Claude to explain this change, in a Claude terminal"))
            .clicked()
        {
            out.explain = Some(Explain { patch: hunk_patch(h, local, remote), whole_file: false });
        }
    }
    ui.add_space(3.0);
    action
}

/// A chunk in unified diff form: `-` local lines, then `+` remote lines.
pub(super) fn as_patch(h: &Hunk, local: &str, remote: &str) -> String {
    let mut out = format!("@@ -{},{} +{},{} @@\n", h.local.start + 1, h.local.len(), h.remote.start + 1, h.remote.len());
    for (sign, text) in [('-', local), ('+', remote)] {
        for line in text.split_inclusive('\n') {
            out.push(sign);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_scroll_the_view() {
        let text: String = (1..=500).map(|i| format!("line {i}\n")).collect();
        let e = FileEntry {
            path: "a.txt".into(),
            rename: None,
            origin: Origin::Synced,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Text(Texts::new(text.clone(), text.clone())),
            version: 1,
        };
        let h = Highlighter::new();
        let hl = HlFile { key: (e.path.clone(), 1), local: h.lines("a.txt", &text, false), remote: h.lines("a.txt", &text, false) };
        let ctx = egui::Context::default();
        let target = egui::Id::new(("overview-jump", "a.txt"));
        let press = |keys: &[egui::Key]| {
            let events = keys.iter().map(|&key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }).collect();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                file_view(ui, &e, Some(&hl), false, &mut true, &mut None, &mut ChangeFocus::default(), None, &mut ViewOut::default());
            });
            out.textures_delta.clear();
            ctx.data(|d| d.get_temp::<f32>(target))
        };
        assert_eq!(press(&[]), None);
        let page = press(&[egui::Key::PageDown]).unwrap();
        assert!(page > 400.0, "{page}");
        press(&[]);
        let lines = press(&[egui::Key::ArrowDown, egui::Key::ArrowDown]).unwrap();
        let row = (lines - page) / 2.0;
        assert!(row > 5.0 && row < 40.0, "{page} {lines}");
        press(&[]);
        let one_more = press(&[egui::Key::ArrowDown]).unwrap();
        assert!((one_more - lines - row).abs() < 0.5, "two presses in one frame count twice: {page} {lines} {one_more}");
        press(&[]);
        press(&[egui::Key::ArrowUp]);
        press(&[]);
        press(&[]);
        assert!((press(&[egui::Key::PageUp]).unwrap() - 2.0 * row).abs() < 1.0);
        press(&[]);
        assert_eq!(press(&[egui::Key::ArrowUp, egui::Key::ArrowUp, egui::Key::ArrowUp]), Some(0.0));
    }

    fn entry(origin: Origin, local: &str, remote: &str, version: u64) -> FileEntry {
        FileEntry {
            path: "a.txt".into(),
            rename: None,
            origin,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Text(Texts::new(local, remote)),
            version,
        }
    }

    fn applied(cmd: Option<Cmd>) -> Option<Action> {
        match cmd {
            Some(Cmd::Apply(items)) => Some(items[0].2),
            _ => None,
        }
    }

    #[test]
    fn the_next_change_is_shown_at_the_top_left() {
        let padding = "x".repeat(400);
        let local: String = (1..=300).map(|i| format!("line {i} {padding}\n")).collect();
        let remote = local.replace(&format!("line 20 {padding}\n"), "CHANGED twenty\n").replace(&format!("line 200 {padding}\n"), "CHANGED two hundred\n");
        let e = FileEntry { path: "a.txt".into(), ..entry(Origin::Remote, &local, &remote, 2) };
        let h = Highlighter::new();
        let hl = HlFile { key: (e.path.clone(), 2), local: h.lines("a.txt", &local, false), remote: h.lines("a.txt", &remote, false) };
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| {
            style.animation_time = 0.0;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        let mut focus = ChangeFocus { path: e.path.clone(), index: 1, ..ChangeFocus::default() };
        let mut visible = Vec::new();
        let mut panel = egui::Rect::NOTHING;
        for frame in 0..8 {
            if frame == 1 {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(("hbar-jump", "a.txt")), 1500.0f32));
            }
            if frame == 3 {
                focus.pending = Some((1, Instant::now()));
            }
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                file_view(ui, &e, Some(&hl), false, &mut true, &mut Some(e.path.clone()), &mut focus, None, &mut ViewOut::default());
            });
            out.textures_delta.clear();
            visible = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) if t.galley.job.text.contains("line ") || t.galley.job.text.contains("CHANGED") => {
                        panel = c.clip_rect;
                        let rect = t.visual_bounding_rect();
                        c.clip_rect.intersects(rect).then(|| (t.galley.job.text.clone(), rect.top(), rect.left()))
                    }
                    _ => None,
                })
                .collect();
        }
        let changed = visible.iter().find(|(text, _, _)| text.contains("CHANGED two hundred")).unwrap_or_else(|| panic!("{visible:?}"));
        let from_top = changed.1 - panel.top();
        assert!((ROOM_ABOVE_CHANGE..ROOM_ABOVE_CHANGE + 40.0).contains(&from_top), "{changed:?} in {panel:?}");
        let from_left = changed.2 - panel.left();
        assert!((ACTIONS_WIDTH..ACTIONS_WIDTH + 40.0).contains(&from_left), "scrolled back to the left: {changed:?} in {panel:?}");
    }

    #[test]
    fn a_search_result_is_highlighted_and_shown() {
        let local: String = (0..300).map(|i| if i == 150 { "let Needle = 1;\n".to_string() } else { format!("line {i}\n") }).collect();
        let e = FileEntry { path: "a.txt".into(), ..entry(Origin::Synced, &local, &local, 1) };
        let h = Highlighter::new();
        let hl = HlFile { key: (e.path.clone(), 1), local: h.lines("a.txt", &local, false), remote: h.lines("a.txt", &local, false) };
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| {
            style.animation_time = 0.0;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(QUERY_ID), Arc::<str>::from("needle"));
            d.insert_temp(egui::Id::new(JUMP_ID), (150usize, JUMP_FRAMES));
        });
        let mut found = None;
        let mut panel = egui::Rect::NOTHING;
        for _ in 0..4 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                file_view(ui, &e, Some(&hl), false, &mut true, &mut Some(e.path.clone()), &mut ChangeFocus::default(), None, &mut ViewOut::default());
            });
            out.textures_delta.clear();
            for c in &out.shapes {
                if let egui::Shape::Text(t) = &c.shape
                    && t.galley.job.text.contains("Needle")
                {
                    panel = c.clip_rect;
                    let marked: Vec<String> = t.galley.job.sections.iter().filter(|s| s.format.background == HIGHLIGHT).map(|s| t.galley.job.text[s.byte_range.start.0..s.byte_range.end.0].to_string()).collect();
                    found = Some((marked, t.visual_bounding_rect().center().y));
                }
            }
        }
        let (marked, y) = found.expect("the matching line is drawn");
        assert_eq!(marked, ["Needle"]);
        assert!((y - panel.center().y).abs() < 40.0, "{y} in {panel:?}");
    }

    #[test]
    fn whole_file_patch_has_every_change() {
        let (local, remote) = ("1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\nTWO\n3\n4\n5\n6\n7\nEIGHT\n9\n");
        let patch = whole_patch(&entry(Origin::Remote, local, remote, 1)).unwrap();
        assert_eq!(patch, "@@ -2,1 +2,1 @@\n-2\n+TWO\n@@ -8,1 +8,1 @@\n-8\n+EIGHT\n");
        assert!(whole_patch(&entry(Origin::Synced, local, local, 1)).is_none());
        assert!(whole_patch(&FileEntry { body: Body::Binary, ..entry(Origin::Remote, local, remote, 1) }).is_none());
    }

    #[test]
    fn accept_takes_claude_or_pushes_mine() {
        let (local, remote) = ("1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\nTWO\n3\n4\n5\n6\n7\nEIGHT\n9\n");
        assert_eq!(accept_action(&entry(Origin::Remote, local, remote, 1), 1), Action::TakeRemoteHunk(1));
        assert_eq!(accept_action(&entry(Origin::Both, local, remote, 1), 0), Action::TakeRemoteHunk(0));
        assert_eq!(accept_action(&entry(Origin::Local, local, remote, 1), 0), Action::TakeLocalHunk(0));
        let new = FileEntry { local_exists: false, ..entry(Origin::Remote, "", "x\n", 1) };
        assert_eq!(accept_action(&new, 0), Action::TakeRemoteFile);
        let binary = FileEntry { body: Body::Binary, ..entry(Origin::Local, "", "", 1) };
        assert_eq!(accept_action(&binary, 0), Action::TakeLocalFile);
    }

    #[test]
    fn accept_waits_for_the_previous_one() {
        let (local, remote) = ("1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\nTWO\n3\n4\n5\n6\n7\nEIGHT\n9\n");
        let mut focus = ChangeFocus::default();
        let e = entry(Origin::Remote, local, remote, 1);
        assert!(!focus.follow(&e, 2));
        focus.index = 1;
        assert_eq!(applied(focus.accept(&e)), Some(Action::TakeRemoteHunk(1)));
        assert_eq!(applied(focus.accept(&e)), None);
        let next = entry(Origin::Remote, local, "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n", 2);
        assert!(focus.follow(&next, 1));
        assert_eq!(focus.index, 0);
        assert_eq!(applied(focus.accept(&next)), Some(Action::TakeRemoteHunk(0)));
        let other = FileEntry { path: "b.txt".into(), ..next };
        focus.follow(&other, 1);
        assert_eq!(focus.index, 0);
        assert_eq!(applied(focus.accept(&FileEntry { origin: Origin::Synced, ..other })), None);
    }

    #[test]
    fn double_click_on_a_line_is_seen() {
        let ctx = egui::Context::default();
        let mut hits = vec![];
        let line: Line = vec![(Color32::WHITE, "let x = 1;".into())];
        let at = egui::pos2(60.0, 8.0);
        let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let frames = [vec![egui::Event::PointerMoved(at)], vec![click(true), click(false)], vec![click(true), click(false)], vec![]];
        for events in frames {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                hits.push(row(ui, 0.0, 1, &line).double_clicked());
            });
            out.textures_delta.clear();
        }
        assert!(hits.contains(&true), "{hits:?}");
    }
}
