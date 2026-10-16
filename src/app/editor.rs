//! Editing a local file in place of its diff. The text never moves while you
//! type: instead of the diff, marks in the margin show which lines differ
//! from the remote, updated when typing pauses. Saving goes back to the diff.

use super::*;
use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{Key, Modifiers, vec2};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::Path;

/// How long typing must pause before the margin marks are updated.
const MARKS_DELAY: Duration = Duration::from_millis(300);
/// Above this size the editor is not syntax colored, to keep typing fluid.
const MAX_COLORED: usize = 256 * 1024;

const ADDED: Color32 = Color32::from_rgb(0x2e, 0x8b, 0x46);
const CHANGED: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x30);
const REMOVED: Color32 = Color32::from_rgb(0xc0, 0x39, 0x2b);

pub(super) struct FileEdit {
    /// The file list entry being edited.
    pub(super) path: String,
    side: Side,
    root: std::path::PathBuf,
    file_path: String,
    /// The file as on disk when editing started or was last saved.
    original: String,
    text: String,
    other: String,
    /// Line ranges that differ from the remote, and their color. An empty
    /// range marks lines removed before that line.
    marks: Vec<(Range<usize>, Color32)>,
    marks_of: u64,
    typed_at: Instant,
    /// Syntax colors of the text with this hash.
    colors: Option<(u64, Arc<Vec<(Range<usize>, Color32)>>)>,
    /// Line to put the cursor on, once the text is laid out.
    goto: Option<usize>,
    goto_frames: u8,
    focus_on_goto: bool,
    view: View,
    dialog: Option<Dialog>,
    error: Option<String>,
    pending: Option<(u64, String, After)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Side {
    Local,
    Remote,
}

pub(super) struct RemoteSave {
    pub(super) ticket: u64,
    pub(super) path: String,
    pub(super) expected: Option<Vec<u8>>,
    pub(super) text: Vec<u8>,
}

fn next_ticket() -> u64 {
    static TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    TICKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// What to do once the edit is saved or discarded.
#[derive(Clone, PartialEq)]
enum After {
    Stay,
    Close,
    /// Close and select this file instead.
    Select(Option<String>),
}

enum Dialog {
    /// Unsaved changes would be lost.
    Discard(After),
    /// The file changed on disk since editing started.
    Conflict(After),
}

/// What happened this frame.
#[derive(Default)]
pub(super) struct EditResult {
    pub(super) saved: bool,
    pub(super) close: bool,
    /// After closing, select this file instead.
    pub(super) select: Option<Option<String>>,
    pub(super) remote_save: Option<RemoteSave>,
}

fn hash(parts: &[&str]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

impl FileEdit {
    pub(super) fn open(e: &FileEntry, local_root: &str, line: usize) -> Result<Self, String> {
        Self::open_side(e, Side::Local, Path::new(local_root), line)
    }

    pub(super) fn open_remote(e: &FileEntry, mirror: &Path, line: usize) -> Result<Self, String> {
        Self::open_side(e, Side::Remote, mirror, line)
    }

    fn open_side(e: &FileEntry, side: Side, root: &Path, line: usize) -> Result<Self, String> {
        let file_path = match (&e.rename, side) {
            (Some(r), Side::Local) => r.local_path.clone(),
            (Some(r), Side::Remote) => r.remote_path.clone(),
            (None, _) => e.path.clone(),
        };
        let bytes = std::fs::read(root.join(&file_path)).map_err(|err| format!("cannot read {file_path}: {err}"))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("{file_path} is not UTF-8 text"))?;
        let other = other_side(e, side).unwrap_or_else(|| text.clone());
        Ok(FileEdit {
            path: e.path.clone(),
            side,
            root: root.to_path_buf(),
            file_path,
            original: text.clone(),
            text,
            other,
            marks: Vec::new(),
            marks_of: 0,
            typed_at: Instant::now(),
            colors: None,
            goto: Some(line),
            goto_frames: JUMP_FRAMES,
            focus_on_goto: true,
            view: View::default(),
            dialog: None,
            error: None,
            pending: None,
        })
    }

    pub(super) fn side(&self) -> Side {
        self.side
    }

    pub(super) fn follow(&mut self, e: &FileEntry) {
        if let Some(other) = other_side(e, self.side)
            && self.other != other
        {
            self.other = other;
        }
    }

    pub(super) fn pending_ticket(&self) -> Option<u64> {
        self.pending.as_ref().map(|(ticket, _, _)| *ticket)
    }

    pub(super) fn remote_saved(&mut self, outcome: Result<bool, String>) -> EditResult {
        let mut result = EditResult::default();
        let Some((_, sent, after)) = self.pending.take() else { return result };
        match outcome {
            Ok(true) => {
                self.original = sent;
                self.error = None;
                result.saved = true;
                close(after, &mut result);
            }
            Ok(false) => self.dialog = Some(Dialog::Conflict(after)),
            Err(e) => self.error = Some(e),
        }
        result
    }

    pub(super) fn without_focus(mut self) -> Self {
        self.focus_on_goto = false;
        self
    }

    pub(super) fn show_line(&mut self, line: usize) {
        self.goto = Some(line);
        self.goto_frames = JUMP_FRAMES;
        self.focus_on_goto = false;
    }

    fn id(&self) -> egui::Id {
        egui::Id::new(("file-edit", &self.path))
    }

    pub(super) fn has_focus(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(self.id()))
    }

    fn cursor(&self, ctx: &egui::Context) -> Option<usize> {
        Some(egui::text_edit::TextEditState::load(ctx, self.id())?.cursor.char_range()?.primary.index.0)
    }

    pub(super) fn word_at_cursor(&self, ctx: &egui::Context) -> Option<String> {
        word_at_char(&self.text, self.cursor(ctx)?)
    }

    pub(super) fn cursor_line(&self, ctx: &egui::Context) -> Option<usize> {
        let at = self.cursor(ctx)?;
        Some(self.text.chars().take(at).filter(|c| *c == '\n').count())
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn dirty(&self) -> bool {
        self.text != self.original
    }

    /// Ask before leaving for another file if there are unsaved changes.
    /// Returns false if the edit has to stay open for that.
    pub(super) fn leave_for(&mut self, selected: Option<String>) -> bool {
        if !self.dirty() {
            return true;
        }
        self.dialog = Some(Dialog::Discard(After::Select(selected)));
        false
    }

    fn disk(&self) -> Option<String> {
        std::fs::read(self.root.join(&self.file_path)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn save(&mut self, force: bool, after: After, result: &mut EditResult) -> bool {
        if !force && self.disk().as_deref() != Some(self.original.as_str()) {
            self.dialog = Some(Dialog::Conflict(after));
            return false;
        }
        if !crate::engine::contained(&self.root, &self.file_path) {
            self.error = Some(format!("{}: refusing, the path goes through a symlink", self.file_path));
            return false;
        }
        if self.side == Side::Remote {
            let ticket = next_ticket();
            let expected = (!force).then(|| self.original.clone().into_bytes());
            result.remote_save = Some(RemoteSave { ticket, path: self.file_path.clone(), expected, text: self.text.clone().into_bytes() });
            self.pending = Some((ticket, self.text.clone(), after));
            return false;
        }
        match std::fs::write(self.root.join(&self.file_path), &self.text) {
            Ok(()) => {
                self.original = self.text.clone();
                self.error = None;
                true
            }
            Err(err) => {
                self.error = Some(format!("cannot save {}: {err}", self.file_path));
                false
            }
        }
    }

    fn update_marks(&mut self, ctx: &egui::Context) {
        let key = hash(&[&self.text, &self.other]);
        if key == self.marks_of {
            return;
        }
        let wait = MARKS_DELAY.saturating_sub(self.typed_at.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        self.marks = crate::diff::hunks(&self.text, &self.other)
            .into_iter()
            .map(|h| {
                let color = match (h.local.is_empty(), h.remote.is_empty()) {
                    (true, _) => REMOVED,
                    (_, true) => ADDED,
                    _ => CHANGED,
                };
                (h.local, color)
            })
            .collect();
        self.marks_of = key;
    }
}

fn other_side(e: &FileEntry, side: Side) -> Option<String> {
    match (&e.body, side) {
        (Body::Text(texts), Side::Local) => Some(texts.diff().remote.clone()),
        (Body::Text(texts), Side::Remote) => Some(texts.diff().local.clone()),
        _ => None,
    }
}

pub(super) fn editor_ui(ui: &mut egui::Ui, ed: &mut FileEdit, highlighter: &Highlighter, syntax_colors: bool) -> EditResult {
    let mut result = EditResult::default();
    let mut request: Option<(bool, After)> = None; // (save?, then)
    let id = egui::Id::new(("file-edit", &ed.path));
    // Not while typing elsewhere, e.g. in a terminal.
    let ours = ui.memory(|m| m.focused().is_none_or(|f| f == id));

    // Shortcuts, taken before the editor sees them (no newline on
    // Ctrl+Enter, and Escape must not just drop the focus).
    let jump = if ours && ed.dialog.is_none() { page_keys(ui, ed, id) } else { None };
    if ours && ed.dialog.is_none() && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::E)) {
        delete_lines_at_cursor(ui.ctx(), ed, id);
    }
    if ours && ed.dialog.is_none() && ed.pending.is_none() {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::Enter) {
                request = Some((true, After::Close));
            } else if i.consume_key(Modifiers::COMMAND, Key::S) {
                request = Some((true, After::Stay));
            } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                request = Some((false, After::Close));
            }
        });
    }

    ui.horizontal_wrapped(|ui| {
        if ed.dirty() {
            ui.label(RichText::new("● modified").color(CHANGED).strong());
        }
        if ed.pending.is_some() {
            ui.spinner();
            ui.weak("saving to the remote…");
        }
    });
    ui.horizontal_wrapped(|ui| {
        let dirty = ed.dirty() && ed.pending.is_none();
        let big = |text: &str, fill: Option<Color32>| {
            let label = RichText::new(text).strong();
            match fill {
                Some(fill) if dirty => egui::Button::new(label.color(Color32::WHITE)).fill(fill),
                _ => egui::Button::new(label),
            }
        };
        if ui.add_enabled(dirty, big("✔ Save", Some(ADDED))).on_hover_text("Save and go back to the diff (Ctrl+Enter)").clicked() {
            request = Some((true, After::Close));
        }
        if ui.add_enabled(dirty, egui::Button::new("💾 Save, keep editing")).on_hover_text("Ctrl+S").clicked() {
            request = Some((true, After::Stay));
        }
        if ui.add_enabled(dirty, big("× Cancel", Some(REMOVED))).on_hover_text("Drop your changes and go back (Esc)").clicked() {
            request = Some((false, After::Close));
        }
        if !ed.dirty() && ed.original != ed.other && ui.add(big("← Diff", None)).on_hover_text("Back to the diff (Esc)").clicked() {
            request = Some((false, After::Close));
        }
    });
    if let Some(err) = &ed.error {
        ui.colored_label(Color32::LIGHT_RED, err);
    }
    ui.separator();

    match request {
        Some((true, after)) => finish(ed, false, after, &mut result),
        Some((false, after)) if ed.dirty() => ed.dialog = Some(Dialog::Discard(after)),
        Some((false, after)) => close(after, &mut result),
        None => {}
    }

    // The text, with line numbers and marks in a margin on its left.
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let cw = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let lines = ed.text.lines().count().max(1);
    let gutter_w = (lines.to_string().len() as f32 + 2.0) * cw + 6.0;
    let colored = syntax_colors && ed.text.len() <= MAX_COLORED;
    let query = current_query(ui.ctx());
    let current = current_line(ui.ctx());
    let mut cache = ed.colors.take();
    let path = ed.file_path.clone();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let text = buf.as_str();
        let key = hash(&[text, if colored { "colored" } else { "plain" }]);
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            cache = Some((key, Arc::new(highlighter.spans(&path, text, colored))));
        }
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        for (range, color) in cache.as_ref().unwrap().1.iter() {
            job.append(&text[range.clone()], 0.0, egui::TextFormat { font_id: font.clone(), color: *color, ..Default::default() });
        }
        if let Some(query) = &query {
            job = highlight_text(job, 0, query, current.and_then(|line| line_bytes(text, line)));
        }
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut area = egui::ScrollArea::both().id_salt(("edit-scroll", &ed.path)).auto_shrink(false);
    if let Some(y) = jump {
        area = area.vertical_scroll_offset(y.max(0.0));
    }
    let goto = ed.goto;
    let scroll = area.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (gutter, _) = ui.allocate_exact_size(vec2(gutter_w, 0.0), egui::Sense::hover());
            let out = egui::TextEdit::multiline(&mut ed.text)
                .id(id)
                .code_editor()
                .frame(egui::Frame::NONE)
                .lock_focus(true)
                .desired_width(f32::INFINITY)
                .min_size(ui.available_size())
                .layouter(&mut layouter)
                .show(ui);
            if let Some(rect) = goto.and_then(|line| line_rect(&out, line)) {
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            (gutter, out)
        })
        .inner
    });
    ed.colors = cache;
    ed.view = View { offset: scroll.state.offset.y, height: scroll.inner_rect.height(), row: ui.fonts_mut(|f| f.row_height(&font)) };
    let (gutter, out) = scroll.inner;
    // Keep Escape for us (cancel), instead of egui dropping the focus.
    if out.response.response.has_focus() {
        let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
    }
    if out.response.response.changed() {
        ed.typed_at = Instant::now();
    }
    note_clicked_word(ui, &out, &ed.text);
    share_selection_as_primary(ui, &out, &ed.text);
    if paste_primary_at_pointer(ui, &out, &mut ed.text, id) {
        ed.typed_at = Instant::now();
    }
    ed.update_marks(ui.ctx());

    // Margin: one row per line, as lines are not wrapped.
    let painter = ui.painter().with_clip_rect(scroll.inner_rect);
    let pal = palette(ui.ctx());
    let mut line = 0;
    for row in &out.galley.rows {
        let rect = row.rect().translate(out.galley_pos.to_vec2());
        let bar = egui::Rect::from_x_y_ranges(gutter.right() - 4.0..=gutter.right() - 1.0, rect.y_range());
        for (range, color) in &ed.marks {
            if range.contains(&line) {
                painter.rect_filled(bar, 0.0, *color);
            } else if range.is_empty() && range.start == line {
                painter.rect_filled(bar.with_max_y(bar.top() + 2.0).expand2(vec2(2.0, 0.0)), 0.0, *color);
            }
        }
        painter.text(egui::pos2(gutter.right() - 8.0, rect.top()), egui::Align2::RIGHT_TOP, (line + 1).to_string(), font.clone(), pal.gutter);
        if let Some(target) = ed.goto
            && target == line
        {
            let at = ed.text.split_inclusive('\n').take(line).map(|l| l.chars().count()).sum::<usize>();
            let mut state = out.state.clone();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            state.store(ui.ctx(), id);
            if ed.focus_on_goto {
                ui.memory_mut(|m| m.request_focus(id));
            }
            ed.goto_frames = ed.goto_frames.saturating_sub(1);
            if ed.goto_frames == 0 {
                ed.goto = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        if row.ends_with_newline {
            line += 1;
        }
    }
    // Past the end: the last line.
    if let Some(target) = ed.goto
        && target > line
    {
        ed.goto = Some(line);
    }

    dialogs(ui, ed, &mut result);
    result
}

fn share_selection_as_primary(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, text: &str) {
    let selecting_ended = ui.input(|i| i.pointer.primary_released()) || out.response.response.double_clicked();
    let Some(range) = out.cursor_range.filter(|_| selecting_ended && out.response.response.has_focus()) else { return };
    let (a, b) = (range.primary.index.0, range.secondary.index.0);
    if a != b {
        set_primary_text(text.chars().skip(a.min(b)).take(a.abs_diff(b)).collect());
    }
}

fn paste_primary_at_pointer(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, text: &mut String, id: egui::Id) -> bool {
    if !out.response.response.clicked_by(egui::PointerButton::Middle) {
        return false;
    }
    let (Some(pos), Some(pasted)) = (ui.input(|i| i.pointer.interact_pos()), primary_text()) else { return false };
    let at = out.galley.cursor_from_pos(pos - out.galley_pos).index.0;
    let byte = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
    text.insert_str(byte, &pasted);
    let mut state = out.state.clone();
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at + pasted.chars().count()))));
    state.store(ui.ctx(), id);
    true
}

fn delete_lines_at_cursor(ctx: &egui::Context, ed: &mut FileEdit, id: egui::Id) {
    let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) else { return };
    let Some(range) = state.cursor.char_range() else { return };
    let (a, b) = (range.primary.index.0, range.secondary.index.0);
    let at = delete_lines(&mut ed.text, a.min(b), a.max(b));
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
    state.store(ctx, id);
    ed.typed_at = Instant::now();
}

fn delete_lines(text: &mut String, from: usize, to: usize) -> usize {
    let byte = |chars: usize| text.char_indices().nth(chars).map_or(text.len(), |(b, _)| b);
    let (from, to) = (byte(from), byte(to));
    let start = text[..from].rfind('\n').map_or(0, |i| i + 1);
    let end = text[to..].find('\n').map_or(text.len(), |i| to + i + 1);
    let start = if end == text.len() && !text[..start].is_empty() && !text[start..end].ends_with('\n') { start - 1 } else { start };
    text.replace_range(start..end, "");
    text[..start].chars().count()
}

fn note_clicked_word(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, text: &str) {
    let command_click = out.response.response.clicked() && ui.input(|i| i.modifiers.command);
    let word = out.cursor_range.and_then(|range| word_at_char(text, range.primary.index.0));
    if let Some(word) = word.filter(|_| command_click) {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(CLICKED_WORD_ID), Some(word)));
    }
}

#[derive(Default, Clone, Copy)]
struct View {
    offset: f32,
    height: f32,
    row: f32,
}

impl View {
    fn page_lines(&self) -> usize {
        if self.row <= 0.0 { 1 } else { ((self.height / self.row) as usize).saturating_sub(1).max(1) }
    }
}

fn page_keys(ui: &egui::Ui, ed: &FileEdit, id: egui::Id) -> Option<f32> {
    let focused = ui.memory(|m| m.has_focus(id));
    let page = ed.view.page_lines() as isize;
    let mut lines = 0isize;
    let mut select = false;
    ui.input_mut(|i| {
        for shift in [false, true] {
            let modifiers = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
            let pages = i.count_and_consume_key(modifiers, Key::PageDown) as isize - i.count_and_consume_key(modifiers, Key::PageUp) as isize;
            lines += pages * page;
            select |= shift && pages != 0;
        }
        if !focused {
            lines += i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as isize - i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as isize;
        }
    });
    if lines == 0 {
        return None;
    }
    if focused && let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
        let range = state.cursor.char_range().unwrap_or_default();
        let moved = CCursor::new(move_lines(&ed.text, range.primary.index.0, lines));
        let range = if select { CCursorRange::two(range.secondary, moved) } else { CCursorRange::one(moved) };
        state.cursor.set_char_range(Some(range));
        state.store(ui.ctx(), id);
    }
    ui.ctx().request_repaint();
    Some(ed.view.offset + lines as f32 * ed.view.row)
}

fn move_lines(text: &str, at: usize, lines: isize) -> usize {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.chars().enumerate().filter(|(_, c)| *c == '\n').map(|(i, _)| i + 1))
        .collect();
    let line = starts.partition_point(|&s| s <= at).saturating_sub(1);
    let column = at - starts[line];
    let target = (line as isize + lines).clamp(0, starts.len() as isize - 1) as usize;
    let end = starts.get(target + 1).map_or(text.chars().count(), |next| next - 1);
    (starts[target] + column).min(end)
}

fn line_rect(out: &egui::text_edit::TextEditOutput, target: usize) -> Option<egui::Rect> {
    let mut line = 0;
    for row in &out.galley.rows {
        if line == target {
            return Some(row.rect().translate(out.galley_pos.to_vec2()));
        }
        if row.ends_with_newline {
            line += 1;
        }
    }
    out.galley.rows.last().map(|row| row.rect().translate(out.galley_pos.to_vec2()))
}

fn close(after: After, result: &mut EditResult) {
    match after {
        After::Stay => {}
        After::Close => result.close = true,
        After::Select(s) => {
            result.close = true;
            result.select = Some(s);
        }
    }
}

/// Save (unless the file changed on disk, which opens the conflict
/// dialog), then do `after`.
fn finish(ed: &mut FileEdit, force: bool, after: After, result: &mut EditResult) {
    if !ed.dirty() && !force {
        close(after, result);
        return;
    }
    if ed.save(force, after.clone(), result) {
        result.saved = true;
        close(after, result);
    }
}

fn dialogs(ui: &egui::Ui, ed: &mut FileEdit, result: &mut EditResult) {
    let Some(dialog) = &ed.dialog else { return };
    let (heading, text, conflict) = match dialog {
        Dialog::Discard(_) => (
            format!("Save your changes to {}?", ed.file_path),
            "You modified this file and did not save it.".to_string(),
            false,
        ),
        Dialog::Conflict(_) if ed.side == Side::Remote => (
            format!("{} changed on the remote", ed.file_path),
            "Claude (or something else on the remote) changed this file since you started editing it. Saving would overwrite that change.".to_string(),
            true,
        ),
        Dialog::Conflict(_) => (
            format!("{} changed on disk", ed.file_path),
            "The file changed since you started editing it (a pull, another editor, git…). Saving would overwrite that change.".to_string(),
            true,
        ),
    };
    let after = match dialog {
        Dialog::Discard(a) | Dialog::Conflict(a) => a.clone(),
    };
    let mut choice = None;
    let modal = egui::Modal::new(egui::Id::new(("edit-dialog", &ed.path))).show(ui.ctx(), |ui| {
        ui.set_width(460.0);
        ui.heading(heading);
        ui.add_space(6.0);
        ui.label(text);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let red = |t: &str| egui::Button::new(RichText::new(t).color(Color32::WHITE).strong()).fill(REMOVED);
            if conflict {
                if ui.add(red("Overwrite with my version")).clicked() {
                    choice = Some("overwrite");
                }
                let reload = if ed.side == Side::Remote { "Reload Claude's version (lose my changes)" } else { "Reload from disk (lose my changes)" };
                if ui.button(reload).clicked() {
                    choice = Some("reload");
                }
            } else {
                if ui.add(egui::Button::new(RichText::new("✔ Save").color(Color32::WHITE).strong()).fill(ADDED)).clicked() {
                    choice = Some("save");
                }
                if ui.add(red("Discard my changes")).clicked() {
                    choice = Some("discard");
                }
            }
            if ui.button("Keep editing").clicked() {
                choice = Some("keep");
            }
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some("keep");
    }
    let Some(choice) = choice else { return };
    ed.dialog = None;
    match choice {
        "save" => finish(ed, false, after, result),
        "overwrite" => finish(ed, true, after, result),
        "discard" => close(after, result),
        "reload" => match ed.disk() {
            Some(disk) => {
                ed.original = disk.clone();
                ed.text = disk;
            }
            None => close(After::Close, result),
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str, local: &str, remote: &str) -> (std::path::PathBuf, FileEntry) {
        let root = std::env::temp_dir().join(format!("agent-diff-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), local).unwrap();
        let e = FileEntry {
            path: "src/a.rs".into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: Some(crate::engine::Change::Modified),
            local_exists: true,
            remote_exists: true,
            body: Body::Text(Texts::new(local, remote)),
            version: 1,
        };
        (root, e)
    }

    /// Run `f` in a headless frame, 800×600.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, f: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
        let mut f = f;
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| f(ui));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn save_refuses_to_overwrite_a_change_on_disk() {
        let (root, e) = setup("conflict", "a\nb\n", "a\nB\n");
        let local = root.to_str().unwrap();
        let mut ed = FileEdit::open(&e, local, 0).unwrap();
        assert!(!ed.dirty());
        ed.text = "a\nb\nc\n".into();
        assert!(ed.dirty());
        let mut r = EditResult::default();
        finish(&mut ed, false, After::Stay, &mut r);
        assert!(r.saved && !r.close && !ed.dirty());
        assert_eq!(std::fs::read_to_string(root.join("src/a.rs")).unwrap(), "a\nb\nc\n");

        // Changed on disk meanwhile: refused, then forced.
        std::fs::write(root.join("src/a.rs"), "pulled\n").unwrap();
        ed.text = "mine\n".into();
        let mut r = EditResult::default();
        finish(&mut ed, false, After::Close, &mut r);
        assert!(!r.saved && !r.close && matches!(ed.dialog, Some(Dialog::Conflict(After::Close))));
        assert_eq!(std::fs::read_to_string(root.join("src/a.rs")).unwrap(), "pulled\n");
        finish(&mut ed, true, After::Close, &mut r);
        assert!(r.saved && r.close);
        assert_eq!(std::fs::read_to_string(root.join("src/a.rs")).unwrap(), "mine\n");
    }

    #[test]
    fn remote_saves_wait_for_the_worker() {
        let (root, e) = setup("remote", "mine\n", "claude\n");
        let mirror = root.join("mirror");
        std::fs::create_dir_all(mirror.join("src")).unwrap();
        std::fs::write(mirror.join("src/a.rs"), "claude\n").unwrap();
        let mut ed = FileEdit::open_remote(&e, &mirror, 0).unwrap();
        assert_eq!((ed.side(), ed.text.as_str(), ed.other.as_str()), (Side::Remote, "claude\n", "mine\n"));

        ed.text = "claude, fixed\n".into();
        let mut r = EditResult::default();
        finish(&mut ed, false, After::Close, &mut r);
        let save = r.remote_save.expect("a save for the worker");
        assert!(!r.saved && !r.close && ed.pending_ticket() == Some(save.ticket));
        assert_eq!((save.path.as_str(), save.expected.as_deref(), save.text.as_slice()), ("src/a.rs", Some(&b"claude\n"[..]), &b"claude, fixed\n"[..]));

        let done = ed.remote_saved(Ok(true));
        assert!(done.saved && done.close && !ed.dirty() && ed.pending_ticket().is_none());
        std::fs::write(mirror.join("src/a.rs"), "claude, fixed\n").unwrap();

        ed.text = "again\n".into();
        let mut r = EditResult::default();
        finish(&mut ed, false, After::Stay, &mut r);
        assert!(r.remote_save.is_some());
        let refused = ed.remote_saved(Ok(false));
        assert!(!refused.saved && matches!(ed.dialog, Some(Dialog::Conflict(After::Stay))) && ed.dirty());

        ed.dialog = None;
        let mut r = EditResult::default();
        finish(&mut ed, true, After::Stay, &mut r);
        assert_eq!(r.remote_save.unwrap().expected, None);
        ed.remote_saved(Err("network down".into()));
        assert_eq!(ed.error.as_deref(), Some("network down"));
    }

    #[test]
    fn search_matches_are_highlighted_in_the_editor() {
        let text: String = (0..40).map(|i| if i == 20 { "let Needle = 1;\n".to_string() } else { format!("line {i}\n") }).collect();
        let (root, e) = setup("highlight", &text, &text);
        let local = root.to_str().unwrap().to_string();
        let mut ed = FileEdit::open(&e, &local, 0).unwrap().without_focus();
        let ctx = egui::Context::default();
        let h = Highlighter::new();
        let mut marked = Vec::new();
        for frame in 0..5 {
            if frame == 2 {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new(QUERY_ID), Arc::<str>::from("needle")));
            }
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    editor_ui(ui, &mut ed, &h, true);
                });
            });
            let painted = ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
            let highlight_vertices = painted
                .iter()
                .filter_map(|p| if let egui::epaint::Primitive::Mesh(m) = &p.primitive { Some(m.vertices.iter().filter(|v| v.color == HIGHLIGHT).count()) } else { None })
                .sum::<usize>();
            if frame == 4 {
                assert!(highlight_vertices >= 4, "the highlight is painted: {highlight_vertices} vertices");
            }
            out.textures_delta.clear();
            marked = out
                .shapes
                .iter()
                .filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some(t.galley.clone()) } else { None })
                .flat_map(|g| g.job.sections.iter().filter(|s| s.format.background == HIGHLIGHT).map(|s| g.job.text[s.byte_range.start.0..s.byte_range.end.0].to_string()).collect::<Vec<_>>())
                .collect();
        }
        assert_eq!(marked, ["Needle"]);
    }

    #[test]
    fn deletes_the_lines_under_the_cursor() {
        let mut text = "one\ntwo\nthree\n".to_string();
        assert_eq!(delete_lines(&mut text, 5, 5), 4);
        assert_eq!(text, "one\nthree\n");
        let mut text = "one\ntwo\nthree".to_string();
        assert_eq!(delete_lines(&mut text, 9, 9), 7);
        assert_eq!(text, "one\ntwo");
        let mut text = "one\ntwo\nthree\n".to_string();
        delete_lines(&mut text, 1, 5);
        assert_eq!(text, "three\n");
    }

    #[test]
    fn leaving_asks_only_with_unsaved_changes() {
        let (root, e) = setup("leave", "a\n", "a\n");
        let mut ed = FileEdit::open(&e, root.to_str().unwrap(), 0).unwrap();
        assert!(ed.leave_for(Some("other".into())));
        ed.text.push('x');
        assert!(!ed.leave_for(Some("other".into())));
        assert!(matches!(&ed.dialog, Some(Dialog::Discard(After::Select(Some(p)))) if p == "other"));
    }

    #[test]
    fn marks_wait_for_a_pause_in_typing() {
        let (root, e) = setup("marks", "1\n2\n3\n4\n", "1\n2\n3\n4\n");
        let mut ed = FileEdit::open(&e, root.to_str().unwrap(), 0).unwrap();
        let ctx = egui::Context::default();
        ed.text = "1\nTWO\n3\nnew\n4\n".into();
        ed.typed_at = Instant::now();
        ed.update_marks(&ctx);
        assert!(ed.marks.is_empty());
        ed.typed_at = Instant::now() - MARKS_DELAY;
        ed.update_marks(&ctx);
        assert_eq!(ed.marks, vec![(1..2, CHANGED), (3..4, ADDED)]);
        ed.text = "1\n3\n4\n".into();
        ed.update_marks(&ctx);
        assert_eq!(ed.marks, vec![(1..1, REMOVED)]);
    }

    #[test]
    fn moving_by_lines_keeps_the_column() {
        let text = "abcdef\nxy\n\nlong line\nend";
        assert_eq!(move_lines(text, 4, 1), 9);
        assert_eq!(move_lines(text, 4, 3), 15);
        assert_eq!(move_lines(text, 15, -3), 4);
        assert_eq!(move_lines(text, 2, 100), 23);
        assert_eq!(move_lines(text, 22, -100), 1);
    }

    #[test]
    fn page_keys_scroll_and_move_the_cursor() {
        let text: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        let (root, e) = setup("page", &text, &text);
        let local = root.to_str().unwrap().to_string();
        let mut ed = FileEdit::open(&e, &local, 0).unwrap().without_focus();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| {
            style.animation_time = 0.0;
            style.scroll_animation = egui::style::ScrollAnimation::none();
        });
        let h = Highlighter::new();
        let id = egui::Id::new(("file-edit", "src/a.rs"));
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let run = |events: Vec<egui::Event>, ed: &mut FileEdit| frame(&ctx, events, |ui| {
            editor_ui(ui, ed, &h, false);
        });
        for _ in 0..JUMP_FRAMES {
            run(vec![], &mut ed);
        }
        run(vec![key(Key::PageDown)], &mut ed);
        run(vec![], &mut ed);
        let page = ed.view.page_lines();
        assert!(page > 5, "{page}");
        assert!((ed.view.offset - page as f32 * ed.view.row).abs() < 1.0, "{} {}", ed.view.offset, page);
        run(vec![key(Key::ArrowDown)], &mut ed);
        run(vec![], &mut ed);
        assert!((ed.view.offset - (page + 1) as f32 * ed.view.row).abs() < 1.0);

        ctx.memory_mut(|m| m.request_focus(id));
        run(vec![], &mut ed);
        run(vec![], &mut ed);
        let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
        state.store(&ctx, id);
        run(vec![key(Key::PageDown), key(Key::PageDown)], &mut ed);
        run(vec![], &mut ed);
        let cursor = egui::text_edit::TextEditState::load(&ctx, id).unwrap().cursor.char_range().unwrap().primary;
        let line_of = |at: usize| text.chars().take(at).filter(|c| *c == '\n').count();
        assert_eq!(line_of(cursor.index.0), 2 * page);
    }

    #[test]
    fn editor_runs_and_puts_the_cursor_on_the_line() {
        let text: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let (root, e) = setup("ui", &text, &text);
        let local = root.to_str().unwrap().to_string();
        let mut ed = FileEdit::open(&e, &local, 150).unwrap();
        let ctx = egui::Context::default();
        let h = Highlighter::new();
        let id = egui::Id::new(("file-edit", "src/a.rs"));
        for _ in 0..JUMP_FRAMES {
            frame(&ctx, vec![], |ui| {
                editor_ui(ui, &mut ed, &h, true);
            });
        }
        assert!(ed.goto.is_none());
        assert!(ctx.memory(|m| m.has_focus(id)));
        let cursor = egui::text_edit::TextEditState::load(&ctx, id).unwrap().cursor.char_range().unwrap();
        let at: usize = text.split_inclusive('\n').take(150).map(|l| l.chars().count()).sum();
        assert_eq!(cursor.primary, CCursor::new(at));

        // Typing, then Ctrl+S saves and stays; Escape closes.
        let mut last = EditResult::default();
        frame(&ctx, vec![egui::Event::Text("X".into())], |ui| last = editor_ui(ui, &mut ed, &h, true));
        assert!(ed.dirty());
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        frame(&ctx, vec![key(Key::S, Modifiers::COMMAND)], |ui| last = editor_ui(ui, &mut ed, &h, true));
        assert!(last.saved && !last.close && !ed.dirty());
        assert!(std::fs::read_to_string(root.join("src/a.rs")).unwrap().contains("Xline 151"));
        frame(&ctx, vec![key(Key::Escape, Modifiers::NONE)], |ui| last = editor_ui(ui, &mut ed, &h, true));
        assert!(last.close && !last.saved);
    }
}
