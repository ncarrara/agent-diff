use super::*;

pub(super) struct FindInFile {
    pub(super) query: String,
    index: usize,
    path: String,
    lines: Vec<usize>,
    focus: bool,
}

pub(super) enum FindAction {
    Close,
    Jump(usize),
}

impl FindInFile {
    pub(super) fn open(path: String) -> Self {
        FindInFile { query: String::new(), index: 0, path, lines: Vec::new(), focus: true }
    }

    pub(super) fn refocus(&mut self) {
        self.focus = true;
    }

    fn field_id(&self) -> egui::Id {
        egui::Id::new("find-in-file")
    }

    pub(super) fn current_line(&self, path: &str) -> Option<usize> {
        (path == self.path).then(|| self.lines.get(self.index).copied()).flatten()
    }

    pub(super) fn active_query(&self) -> Option<&str> {
        Some(self.query.as_str()).filter(|q| !q.is_empty())
    }

    fn update(&mut self, path: &str, text: &str) {
        if path != self.path {
            self.path = path.to_string();
            self.index = 0;
        }
        self.lines = if self.query.is_empty() { Vec::new() } else { search_text(path, text, &self.query).map(|hit| hit.lines).unwrap_or_default() };
        self.index = self.index.min(self.lines.len().saturating_sub(1));
    }

    fn step(&mut self, forward: bool) -> Option<usize> {
        let count = self.lines.len();
        if count == 0 {
            return None;
        }
        self.index = if forward { (self.index + 1) % count } else { (self.index + count - 1) % count };
        self.lines.get(self.index).copied()
    }

    pub(super) fn bar(&mut self, ui: &mut egui::Ui, path: &str, text: &str) -> Option<FindAction> {
        let id = self.field_id();
        let typing_here = ui.memory(|m| m.has_focus(id));
        let (enter, shift_enter, escape) = ui.input_mut(|i| {
            let shift_enter = typing_here && i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
            let enter = typing_here && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            let escape = typing_here && i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            (enter || i.consume_key(egui::Modifiers::NONE, egui::Key::F3), shift_enter || i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3), escape)
        });
        let mut action = None;
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut self.query).id(id).hint_text("find in this file").desired_width(220.0));
            if std::mem::take(&mut self.focus) {
                field.request_focus();
            }
            let changed = field.changed() || path != self.path;
            self.update(path, text);
            if changed && !self.lines.is_empty() {
                self.index = 0;
                action = Some(FindAction::Jump(self.lines[0]));
            }
            let position = if self.lines.is_empty() { if self.query.is_empty() { String::new() } else { "no match".into() } } else { format!("{}/{}", self.index + 1, self.lines.len()) };
            ui.weak(position);
            let previous = ui.small_button("⏶").on_hover_text("Previous match (Shift+Enter, Shift+F3)").clicked();
            let next = ui.small_button("⏷").on_hover_text("Next match (Enter, F3)").clicked();
            if (next || enter) && let Some(line) = self.step(true) {
                action = Some(FindAction::Jump(line));
            }
            if (previous || shift_enter) && let Some(line) = self.step(false) {
                action = Some(FindAction::Jump(line));
            }
            if ui.small_button("×").on_hover_text("Close (Esc)").clicked() || escape {
                action = Some(FindAction::Close);
            }
        });
        action
    }
}
