use eframe::egui::Color32;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Above this size files are shown without highlighting, to keep the UI fast.
const MAX_HIGHLIGHT: usize = 1024 * 1024;

/// One display line: colored spans, without the trailing newline.
pub type Line = Vec<(Color32, String)>;

/// Cheap to clone (the syntax and theme sets are shared), so a copy can be
/// sent to a background thread.
#[derive(Clone)]
pub struct Highlighter {
    syntaxes: Arc<SyntaxSet>,
    themes: Arc<ThemeSet>,
    theme: String,
}

impl Highlighter {
    pub fn new() -> Self {
        Highlighter {
            syntaxes: Arc::new(two_face::syntax::extra_newlines()),
            themes: Arc::new(ThemeSet::load_defaults()),
            theme: "base16-ocean.dark".into(),
        }
    }

    /// Switch to one of syntect's bundled themes (ignored if unknown).
    pub fn set_theme(&mut self, name: &str) {
        if self.themes.themes.contains_key(name) {
            self.theme = name.into();
        }
    }

    fn theme(&self) -> &Theme {
        &self.themes.themes[&self.theme]
    }

    /// The theme's plain text color, or a readable default for its background.
    pub fn text_color(&self) -> Color32 {
        match (self.theme().settings.foreground, self.background()) {
            (Some(c), _) => Color32::from_rgb(c.r, c.g, c.b),
            (None, Some(bg)) if bg.r() as u32 + bg.g() as u32 + bg.b() as u32 > 3 * 128 => Color32::from_gray(40),
            _ => Color32::LIGHT_GRAY,
        }
    }

    pub fn background(&self) -> Option<Color32> {
        self.theme().settings.background.map(|c| Color32::from_rgb(c.r, c.g, c.b))
    }

    fn syntax(&self, path: &str, text: &str) -> &SyntaxReference {
        let p = Path::new(path);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.syntaxes
            .find_syntax_by_extension(ext)
            .or_else(|| self.syntaxes.find_syntax_by_extension(name))

            .or_else(|| self.syntaxes.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    /// Colored byte ranges covering all of `text`, unchanged, for an editor.
    pub fn spans(&self, path: &str, text: &str, colored: bool) -> Vec<(Range<usize>, Color32)> {
        let plain = self.text_color();
        if !colored || text.len() > MAX_HIGHLIGHT {
            return vec![(0..text.len(), plain)];
        }
        let mut h = HighlightLines::new(self.syntax(path, text), self.theme());
        let mut out = Vec::new();
        let mut at = 0;
        for line in LinesWithEndings::from(text) {
            match h.highlight_line(line, &self.syntaxes) {
                Ok(ranges) => {
                    for (style, s) in ranges {
                        let c = style.foreground;
                        out.push((at..at + s.len(), if c.a == 0 { plain } else { Color32::from_rgb(c.r, c.g, c.b) }));
                        at += s.len();
                    }
                }
                Err(_) => {
                    out.push((at..at + line.len(), plain));
                    at += line.len();
                }
            }
        }
        out
    }

    /// Split `text` into lines, highlighted with the syntax matching `path`
    /// when `colored`, otherwise in the theme's plain text color.
    pub fn lines(&self, path: &str, text: &str, colored: bool) -> Vec<Line> {
        let plain = self.text_color();
        let mut h = HighlightLines::new(self.syntax(path, text), self.theme());

        LinesWithEndings::from(text)
            .map(|line| {
                let ranges = if !colored || text.len() > MAX_HIGHLIGHT {
                    None
                } else {
                    h.highlight_line(line, &self.syntaxes).ok()
                };
                // Uncolored text uses the theme's text color.
                let spans: Vec<(Color32, &str)> = match ranges {
                    Some(r) => r
                        .into_iter()
                        .map(|(style, s)| {
                            let c = style.foreground;
                            (if c.a == 0 { plain } else { Color32::from_rgb(c.r, c.g, c.b) }, s)
                        })
                        .collect(),
                    None => vec![(plain, line)],
                };
                let mut out: Line = spans
                    .into_iter()
                    .map(|(color, s)| (color, s.trim_end_matches(['\n', '\r']).replace('\t', "    ")))
                    .filter(|(_, s)| !s.is_empty())
                    .collect();
                if !line.ends_with('\n') {
                    out.push((Color32::GRAY, "  ⟨no newline at end of file⟩".into()));
                }
                out
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cover_the_text() {
        let text = "fn main() {\r\n\tlet s = \"é\";\n}";
        let spans = Highlighter::new().spans("x.rs", text, true);
        assert!(spans.len() > 3);
        let mut at = 0;
        for (r, _) in &spans {
            assert_eq!(r.start, at);
            at = r.end;
        }
        assert_eq!(at, text.len());
    }

    #[test]
    fn rust_gets_colors() {
        let lines = Highlighter::new().lines("src/x.rs", "fn main() {\n    let s = \"hi\"; // note\n}", true);
        assert_eq!(lines.len(), 3);
        let colors: std::collections::HashSet<_> = lines[1].iter().map(|(c, _)| *c).collect();
        assert!(colors.len() >= 3, "{lines:?}");
        let text: String = lines[1].iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(text, "    let s = \"hi\"; // note");
        assert!(lines[2].last().unwrap().1.contains("no newline"));
    }

    #[test]
    fn plain_text_is_readable_on_dark_themes() {
        let mut h = Highlighter::new();
        for theme in ["base16-ocean.dark", "base16-eighties.dark", "base16-mocha.dark", "Solarized (dark)"] {
            h.set_theme(theme);
            let bg = h.background().unwrap();
            for colored in [false, true] {
                for (c, _) in &h.lines("x.rs", "let x = 1;\n", colored)[0] {
                    let lum = |c: Color32| c.r() as i32 + c.g() as i32 + c.b() as i32;
                    assert!(lum(*c) - lum(bg) > 150, "{theme} colored={colored}: {c:?} on {bg:?}");
                }
            }
        }
    }
}
