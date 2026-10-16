use super::*;

const MAX_FILES: usize = 50;
const MAX_LINES: usize = 300;
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
const MIN_TEXT_QUERY: usize = 2;
const TYPING_PAUSE: Duration = Duration::from_millis(250);

#[derive(Clone, PartialEq, Debug)]
pub(super) struct LineHit {
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) text: String,
}

#[derive(Clone, PartialEq, Debug)]
pub(super) enum Choice {
    File(String),
    Line(LineHit),
}

#[derive(Default)]
pub(super) struct QuickOpen {
    query: String,
    selected: usize,
    typed_at: Option<Instant>,
    searched: String,
    job: Option<std::sync::mpsc::Receiver<(String, Vec<LineHit>)>>,
    lines: Vec<LineHit>,
    opened_at_frame: bool,
}

pub(super) fn matching_files(paths: &[String], query: &str) -> Vec<String> {
    let first_word = query.split_whitespace().next().unwrap_or("").to_lowercase();
    let mut found: Vec<&String> = paths.iter().filter(|p| matches_filter(p, query)).collect();
    found.sort_by_key(|p| {
        let name = p.rsplit('/').next().unwrap_or(p).to_lowercase();
        (!name.starts_with(&first_word), !name.contains(&first_word), p.len(), p.to_string())
    });
    found.into_iter().take(MAX_FILES).cloned().collect()
}

pub(super) fn lines_containing(path: &str, text: &str, query: &str, room: usize) -> Vec<LineHit> {
    let query = query.to_lowercase();
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&query))
        .take(room)
        .map(|(line, text)| LineHit { path: path.to_string(), line, text: text.trim().chars().take(200).collect() })
        .collect()
}

fn search_lines(files: &[(String, PathBuf)], query: &str) -> Vec<LineHit> {
    let mut hits = Vec::new();
    for (path, file) in files {
        if hits.len() >= MAX_LINES {
            break;
        }
        if std::fs::metadata(file).map_or(true, |m| m.len() > MAX_FILE_SIZE) {
            continue;
        }
        let Ok(bytes) = std::fs::read(file) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        hits.extend(lines_containing(path, &String::from_utf8_lossy(&bytes), query, MAX_LINES - hits.len()));
    }
    hits
}

impl QuickOpen {
    pub(super) fn open() -> Self {
        QuickOpen { opened_at_frame: true, ..Default::default() }
    }

    fn update_search(&mut self, ctx: &egui::Context, files: impl FnOnce() -> Vec<(String, PathBuf)>) {
        if let Some(rx) = &self.job
            && let Ok((query, lines)) = rx.try_recv()
        {
            self.job = None;
            self.searched = query;
            self.lines = lines;
        }
        let query = self.query.trim().to_string();
        if query.len() < MIN_TEXT_QUERY {
            self.lines.clear();
            self.searched = query;
            return;
        }
        if query == self.searched || self.job.is_some() {
            return;
        }
        let wait = TYPING_PAUSE.saturating_sub(self.typed_at.map_or(TYPING_PAUSE, |t| t.elapsed()));
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (files, ctx) = (files(), ctx.clone());
        std::thread::spawn(move || {
            let lines = search_lines(&files, &query);
            let _ = tx.send((query, lines));
            ctx.request_repaint();
        });
        self.job = Some(rx);
    }

    fn choices(&self, paths: &[String]) -> Vec<Choice> {
        let query = self.query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let files = matching_files(paths, query).into_iter().map(Choice::File);
        let lines = self.lines.iter().cloned().map(Choice::Line);
        files.chain(lines).collect()
    }

    fn arrow_steps(ui: &egui::Ui) -> isize {
        ui.input_mut(|i| {
            i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as isize - i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as isize
        })
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, paths: &[String], files: impl FnOnce() -> Vec<(String, PathBuf)>) -> (bool, Option<Choice>) {
        self.update_search(ctx, files);
        let choices = self.choices(paths);
        let mut picked = None;
        let modal = egui::Modal::new(egui::Id::new("quick-open")).show(ctx, |ui| {
            ui.set_width(ctx.content_rect().width().clamp(300.0, 900.0) * 0.8);
            ui.heading("Open a file, or go to a line containing…");
            let steps = Self::arrow_steps(ui);
            let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let field = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("part of a file name, or text in a file").desired_width(f32::INFINITY));
            if std::mem::take(&mut self.opened_at_frame) || !field.has_focus() {
                field.request_focus();
            }
            if field.changed() {
                self.selected = 0;
                self.typed_at = Some(Instant::now());
            }
            if !choices.is_empty() {
                self.selected = (self.selected as isize + steps).clamp(0, choices.len() as isize - 1) as usize;
            }
            if enter {
                picked = choices.get(self.selected).cloned();
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.6).show(ui, |ui| {
                choices_list(ui, &choices, self.selected, steps != 0, &mut picked);
            });
            let searching = self.job.is_some() || (self.query.trim().len() >= MIN_TEXT_QUERY && self.query.trim() != self.searched);
            ui.horizontal(|ui| {
                if searching {
                    ui.spinner();
                    ui.weak("searching in files…");
                }
                ui.weak("↑ ↓ to choose · Enter to open · Esc to close");
            });
        });
        let close = picked.is_some() || modal.should_close();
        (close, picked)
    }
}

fn choices_list(ui: &mut egui::Ui, choices: &[Choice], selected: usize, scroll_to_selected: bool, picked: &mut Option<Choice>) {
    let mut in_lines = false;
    for (i, choice) in choices.iter().enumerate() {
        let text = match choice {
            Choice::File(path) => RichText::new(format!("📄 {path}")).monospace(),
            Choice::Line(hit) => {
                if !in_lines {
                    in_lines = true;
                    ui.add_space(4.0);
                    ui.weak("In files");
                }
                RichText::new(format!("{}:{}  {}", hit.path, hit.line + 1, hit.text)).monospace()
            }
        };
        let row = ui.selectable_label(i == selected, text);
        if i == selected && scroll_to_selected {
            row.scroll_to_me(None);
        }
        if row.clicked() {
            *picked = Some(choice.clone());
        }
    }
    if choices.is_empty() {
        ui.weak("Type to find a file by name, or a line by its text.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_starting_with_the_query_come_first() {
        let paths = ["src/app/workspace.rs", "src/engine.rs", "docs/engine-notes.md", "README.md"].map(String::from);
        assert_eq!(matching_files(&paths, "engine"), ["src/engine.rs", "docs/engine-notes.md"]);
        assert_eq!(matching_files(&paths, "app work"), ["src/app/workspace.rs"]);
    }

    #[test]
    fn lines_are_found_case_insensitively() {
        let hits = lines_containing("a.rs", "fn main() {}\nlet Needle = 1;\n", "needle", 10);
        assert_eq!(hits, [LineHit { path: "a.rs".into(), line: 1, text: "let Needle = 1;".into() }]);
    }
}
