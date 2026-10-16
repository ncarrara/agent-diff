use super::*;

pub(super) const HOVERED_WORD_ID: &str = "definition-hovered-word";
pub(super) const CLICKED_WORD_ID: &str = "definition-clicked-word";
const MAX_FILE: u64 = 2 * 1024 * 1024;

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Definition {
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) text: String,
}

const MODIFIERS: &[&str] = &[
    "pub", "crate", "super", "self", "in", "async", "unsafe", "extern", "export", "default", "abstract", "public", "private", "protected", "static",
    "final", "sealed", "data", "open", "internal", "inline", "declare", "virtual", "override", "partial", "readonly",
];

fn extension(path: &str) -> String {
    path.rsplit('/').next().and_then(|name| name.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn keywords(path: &str) -> &'static [&'static str] {
    match extension(path).as_str() {
        "rs" => &["fn", "struct", "enum", "trait", "type", "mod", "const", "static", "union", "macro_rules"],
        "py" | "pyi" => &["class", "def"],
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" => &["class", "function", "interface", "type", "enum", "const", "let", "var", "namespace"],
        "go" => &["func", "type"],
        "java" | "kt" | "kts" | "scala" | "cs" => &["class", "interface", "enum", "record", "fun", "object", "trait", "struct", "def", "val"],
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" => &["struct", "class", "enum", "union", "define", "typedef"],
        "rb" => &["class", "module", "def"],
        "php" => &["class", "interface", "trait", "function", "enum"],
        "swift" => &["class", "struct", "enum", "protocol", "func", "extension"],
        "sh" | "bash" | "zsh" => &["function"],
        _ => &[],
    }
}

pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn without_strings(line: &str) -> String {
    let mut out = String::new();
    let mut quote = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' || c == '`' => quote = Some(c),
            None => out.push(c),
        }
    }
    out
}

fn defined_name(line: &str, keywords: &[&str]) -> Option<String> {
    let code = without_strings(line.trim_start().trim_start_matches(['#', '@']));
    let tokens: Vec<&str> = code.split(|c: char| !is_word_char(c)).filter(|t| !t.is_empty()).collect();
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1)?;
        let next_is_word = MODIFIERS.contains(next) || keywords.contains(next);
        match (keywords.contains(token), MODIFIERS.contains(token)) {
            (true, _) if !next_is_word => return Some(next.to_string()),
            (true, _) | (_, true) => continue,
            _ => return None,
        }
    }
    None
}

fn shell_function(line: &str) -> Option<String> {
    let name = line.trim().split_once("()")?.0.trim();
    (!name.is_empty() && name.chars().all(|c| is_word_char(c) || c == '-')).then(|| name.to_string())
}

const NOT_DEFINITIONS: &[&str] = &["if", "for", "while", "switch", "catch", "return", "else", "do", "new", "await", "throw", "typeof", "sizeof", "with", "match"];

fn has_methods(path: &str) -> bool {
    matches!(
        extension(path).as_str(),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "java" | "kt" | "kts" | "scala" | "cs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "php" | "go" | "swift"
    )
}

fn word_position(code: &str, name: &str) -> Option<usize> {
    code.match_indices(name).map(|(i, _)| i).find(|&i| {
        let starts_word = code[..i].chars().next_back().is_none_or(|c| !is_word_char(c));
        let ends_word = code[i + name.len()..].chars().next().is_none_or(|c| !is_word_char(c));
        starts_word && ends_word
    })
}

fn without_go_receiver(code: &str) -> &str {
    code.strip_prefix("func (").and_then(|rest| rest.split_once(')')).map_or(code, |(_, after)| after.trim_start())
}

fn defines_method(line: &str, name: &str) -> bool {
    let code = without_strings(line);
    let code = without_go_receiver(code.trim());
    let Some(at) = word_position(code, name) else { return false };
    let (before, after) = (&code[..at], code[at + name.len()..].trim_start());
    let arrow_property = after.starts_with('=') && after.contains("=>");
    let parameters = (after.starts_with('(') || after.starts_with('<')) && !after.contains("=>");
    let plain_prefix = !before.contains(['=', '(', '.', ',', ';', ':']) && !before.split(|c: char| !is_word_char(c)).any(|w| NOT_DEFINITIONS.contains(&w));
    (parameters || arrow_property) && code.ends_with('{') && plain_prefix && !NOT_DEFINITIONS.contains(&name)
}

pub(super) fn definitions_in(path: &str, text: &str, name: &str) -> Vec<Definition> {
    let keywords = keywords(path);
    let shell = keywords == ["function"];
    let methods = has_methods(path);
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(name))
        .filter(|(_, line)| {
            defined_name(line, keywords).as_deref() == Some(name) || shell && shell_function(line).as_deref() == Some(name) || methods && defines_method(line, name)
        })
        .map(|(line, text)| Definition { path: path.to_string(), line, text: text.trim().to_string() })
        .collect()
}

fn definitions_in_file(path: &str, file: &Path, name: &str) -> Vec<Definition> {
    if keywords(path).is_empty() || std::fs::metadata(file).map_or(true, |m| m.len() > MAX_FILE) {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
    if bytes.contains(&0) {
        return Vec::new();
    }
    definitions_in(path, &String::from_utf8_lossy(&bytes), name)
}

pub(super) fn word_at(text: &str, byte: usize) -> Option<String> {
    let byte = (0..=byte.min(text.len())).rev().find(|&b| text.is_char_boundary(b))?;
    let start = text[..byte].char_indices().rev().take_while(|(_, c)| is_word_char(*c)).last().map_or(byte, |(i, _)| i);
    let end = text[byte..].char_indices().find(|(_, c)| !is_word_char(*c)).map_or(text.len(), |(i, _)| byte + i);
    let word = &text[start..end];
    (!word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit())).then(|| word.to_string())
}

pub(super) fn word_at_char(text: &str, char_index: usize) -> Option<String> {
    let byte = text.char_indices().nth(char_index).map_or(text.len(), |(b, _)| b);
    word_at(text, byte)
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Place {
    pub(super) path: String,
    pub(super) line: Option<usize>,
}

#[derive(Default)]
pub(super) struct GoToDefinition {
    job: Option<(String, std::sync::mpsc::Receiver<Vec<Definition>>)>,
    pub(super) choices: Option<(String, Vec<Definition>)>,
    pub(super) back: Vec<Place>,
    pub(super) jump: Option<(String, usize)>,
    pub(super) highlighted: Option<(String, usize)>,
}

pub(super) enum Found {
    Nothing(String),
    One(Definition),
    Several,
}

impl GoToDefinition {
    pub(super) fn look_for(&mut self, ctx: &egui::Context, name: String, files: Vec<(String, PathBuf)>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (ctx, wanted) = (ctx.clone(), name.clone());
        std::thread::spawn(move || {
            let mut found: Vec<Definition> = files.iter().flat_map(|(path, file)| definitions_in_file(path, file, &wanted)).collect();
            found.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
            let _ = tx.send(found);
            ctx.request_repaint();
        });
        self.choices = None;
        self.job = Some((name, rx));
    }

    pub(super) fn poll(&mut self) -> Option<Found> {
        let (name, rx) = self.job.as_ref()?;
        let found = rx.try_recv().ok()?;
        let name = name.clone();
        self.job = None;
        Some(match found.len() {
            0 => Found::Nothing(name),
            1 => Found::One(found.into_iter().next().unwrap()),
            _ => {
                self.choices = Some((name, found));
                Found::Several
            }
        })
    }

    pub(super) fn take_jump(&mut self, path: &str) -> Option<usize> {
        self.jump.take_if(|(at, _)| at == path).map(|(_, line)| line)
    }

    pub(super) fn go(&mut self, from: Place, to: &Definition) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
        }
        self.jump = Some((to.path.clone(), to.line));
        self.highlighted = Some((to.path.clone(), to.line));
    }

    pub(super) fn highlighted_line(&self, path: &str) -> Option<usize> {
        self.highlighted.as_ref().filter(|(at, _)| at == path).map(|(_, line)| *line)
    }
}

pub(super) fn choices_window(ctx: &egui::Context, choices: &mut Option<(String, Vec<Definition>)>) -> Option<Definition> {
    let (name, found) = choices.as_ref()?;
    let mut picked = None;
    let mut open = true;
    egui::Window::new(format!("Definitions of {name}")).collapsible(false).open(&mut open).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0)).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
            for definition in found {
                let label = format!("{}:{}  {}", definition.path, definition.line + 1, definition.text);
                if ui.selectable_label(false, RichText::new(label).monospace()).clicked() {
                    picked = Some(definition.clone());
                }
            }
        });
    });
    if picked.is_some() || !open || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        *choices = None;
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_definitions_by_language() {
        let rust = "pub(crate) struct Engine {\n    engine: Engine,\n}\npub const fn engine() {}\nimpl Engine {}\n";
        assert_eq!(definitions_in("a.rs", rust, "Engine").iter().map(|d| d.line).collect::<Vec<_>>(), [0]);
        assert_eq!(definitions_in("a.rs", rust, "engine").iter().map(|d| d.line).collect::<Vec<_>>(), [3]);
        let python = "class Config:\n    async def load(self):\n        Config()\n";
        assert_eq!(definitions_in("a.py", python, "Config").iter().map(|d| d.line).collect::<Vec<_>>(), [0]);
        assert_eq!(definitions_in("a.py", python, "load").iter().map(|d| d.line).collect::<Vec<_>>(), [1]);
        let script = "usage() {\n  echo \"usage\"\n}\nusage\n";
        assert_eq!(definitions_in("run.sh", script, "usage").iter().map(|d| d.line).collect::<Vec<_>>(), [0]);
        assert!(definitions_in("notes.txt", "class Config", "Config").is_empty());
    }

    #[test]
    fn finds_methods_without_a_keyword() {
        let ts = "class Store {\n  async load(id: string): Promise<void> {\n  }\n  handle = (e) => {\n  }\n}\nuseEffect(() => {\n});\nif (load(1)) {\n}\n";
        assert_eq!(definitions_in("a.ts", ts, "load").iter().map(|d| d.line).collect::<Vec<_>>(), [1]);
        assert_eq!(definitions_in("a.ts", ts, "handle").iter().map(|d| d.line).collect::<Vec<_>>(), [3]);
        assert!(definitions_in("a.ts", ts, "useEffect").is_empty());
        let go = "func (s *Server) Start(port int) error {\n}\n";
        assert_eq!(definitions_in("a.go", go, "Start").len(), 1);
        let java = "  public static int parse(String s) {\n    return parse(s, 10);\n  }\n";
        assert_eq!(definitions_in("A.java", java, "parse").iter().map(|d| d.line).collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn word_under_a_position() {
        assert_eq!(word_at("let x = Engine::new();", 10).as_deref(), Some("Engine"));
        assert_eq!(word_at("let x = Engine::new();", 8).as_deref(), Some("Engine"));
        assert_eq!(word_at("a + b", 2), None);
        assert_eq!(word_at_char("é = Config", 6).as_deref(), Some("Config"));
    }
}
