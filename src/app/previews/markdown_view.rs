use super::rich_text::{Block, Document, Span, push_span};
use std::path::Path;

pub(super) fn is_markdown(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd"))
}

fn closing_bracket(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn link_at(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let close = closing_bracket(chars, open)?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let target: String = chars[close + 2..end].iter().collect();
    let url = target.split_whitespace().next().unwrap_or_default().trim_matches(['<', '>']).to_string();
    Some((chars[open + 1..close].iter().collect(), url, end + 1))
}

fn has_closing(chars: &[char], from: usize, marker: &[char]) -> bool {
    (from..chars.len().saturating_sub(marker.len() - 1)).any(|i| chars[i..].starts_with(marker))
}

fn toggle(style: &mut Span, marker: &[char]) {
    match marker {
        ['~', '~'] => style.strike = !style.strike,
        [_, _] => style.bold = !style.bold,
        _ => style.italic = !style.italic,
    }
}

fn inline_into(text: &str, base: &Span, spans: &mut Vec<Span>) {
    let chars: Vec<char> = text.chars().collect();
    let mut style = base.clone();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\\' && next.is_some_and(|n| n.is_ascii_punctuation()) {
            push_span(spans, &next.unwrap().to_string(), &style);
            i += 2;
            continue;
        }
        if c == '`' {
            let run = chars[i..].iter().take_while(|&&c| c == '`').count();
            let fence: Vec<char> = vec!['`'; run];
            if let Some(end) = (i + run..chars.len()).find(|&j| chars[j..].starts_with(&fence)) {
                let code: String = chars[i + run..end].iter().collect();
                push_span(spans, code.trim(), &Span { code: true, ..style.clone() });
                i = end + run;
                continue;
            }
        }
        if (c == '[' || (c == '!' && next == Some('['))) && let Some((label, url, end)) = link_at(&chars, if c == '!' { i + 1 } else { i }) {
            if c == '!' {
                push_span(spans, &format!("[{}]", if label.is_empty() { "image" } else { &label }), &style);
            } else {
                inline_into(&label, &Span { link: Some(url), underline: true, ..style.clone() }, spans);
            }
            i = end;
            continue;
        }
        if c == '<' && let Some(end) = chars[i..].iter().position(|&c| c == '>').map(|e| i + e) {
            let inside: String = chars[i + 1..end].iter().collect();
            if inside.starts_with("http://") || inside.starts_with("https://") || inside.starts_with("mailto:") {
                push_span(spans, &inside, &Span { link: Some(inside.clone()), underline: true, ..style.clone() });
                i = end + 1;
                continue;
            }
        }
        if matches!(c, '*' | '_' | '~') {
            let doubled = next == Some(c);
            let marker: Vec<char> = if doubled { vec![c, c] } else { vec![c] };
            let around_word = c == '_' && i > 0 && chars[i - 1].is_alphanumeric() && chars.get(i + marker.len()).is_some_and(|n| n.is_alphanumeric());
            let usable = (c != '~' || doubled) && !around_word;
            let closes = match marker.len() {
                2 => if c == '~' { style.strike } else { style.bold },
                _ => style.italic,
            };
            if usable && (closes || has_closing(&chars, i + marker.len(), &marker)) {
                toggle(&mut style, &marker);
                i += marker.len();
                continue;
            }
        }
        push_span(spans, c.encode_utf8(&mut [0; 4]), &style);
        i += 1;
    }
}

fn inline(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    inline_into(text.trim(), &Span::default(), &mut spans);
    spans.retain(|s| !s.text.is_empty());
    spans
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    let rest = &line[hashes..];
    ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with(' '))).then(|| (hashes.min(4) as u8, rest.trim().trim_end_matches('#').trim()))
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ["-", "*", "_"].iter().any(|m| compact.chars().all(|c| c.to_string() == *m))
}

fn list_marker(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    let (marker, after) = if let Some(after) = rest.strip_prefix(['-', '*', '+']) {
        ("•".to_string(), after)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let after = rest[digits..].strip_prefix(['.', ')']).filter(|_| (1..=9).contains(&digits))?;
        (format!("{}.", &rest[..digits]), after)
    };
    if !after.starts_with(' ') && !after.is_empty() {
        return None;
    }
    let after = after.trim_start();
    let (marker, after) = match after.get(..3) {
        Some("[ ]") => ("☐".to_string(), &after[3..]),
        Some("[x]" | "[X]") => ("☑".to_string(), &after[3..]),
        _ => (marker, after),
    };
    Some((indent, marker, after))
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_table_separator(line: &str) -> bool {
    is_table_row(line) && line.contains('-') && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn table_cells(line: &str) -> Vec<&str> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(str::trim).collect()
}

fn fence(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f))
}

fn starts_block(line: &str) -> bool {
    let trimmed = line.trim_start();
    heading(trimmed).is_some() || is_rule(trimmed) || trimmed.starts_with('>') || fence(line).is_some() || list_marker(line).is_some() || is_table_row(line)
}

fn paragraph_text(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| {
            let hard = l.ends_with("  ") || l.ends_with('\\');
            let text = l.trim().trim_end_matches('\\');
            if hard { format!("{text}\n") } else { format!("{text} ") }
        })
        .collect()
}

struct Lines<'a> {
    lines: Vec<&'a str>,
    at: usize,
    doc: Document,
}

impl<'a> Lines<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.at).copied()
    }

    fn front_matter(&mut self) {
        if self.peek().map(str::trim_end) != Some("---") {
            return;
        }
        let Some(end) = self.lines.iter().skip(1).position(|l| matches!(l.trim_end(), "---" | "...")) else { return };
        self.doc.blocks.push(Block::Code(self.lines[1..=end].join("\n")));
        self.at = end + 2;
    }

    fn code(&mut self, marker: &str) {
        self.at += 1;
        let start = self.at;
        while self.peek().is_some_and(|l| !l.trim_start().starts_with(marker)) {
            self.at += 1;
        }
        self.doc.blocks.push(Block::Code(self.lines[start..self.at].join("\n")));
        self.at += 1;
    }

    fn quote(&mut self) {
        let mut text = Vec::new();
        while let Some(line) = self.peek().and_then(|l| l.trim_start().strip_prefix('>')) {
            text.push(line.strip_prefix(' ').unwrap_or(line));
            self.at += 1;
        }
        self.doc.blocks.push(Block::Quote(inline(&paragraph_text(&text))));
    }

    fn item(&mut self, indent: usize, marker: String, first: &str) {
        self.at += 1;
        let mut text = vec![first];
        while let Some(line) = self.peek().filter(|l| !l.trim().is_empty() && !starts_block(l) && l.starts_with(char::is_whitespace)) {
            text.push(line);
            self.at += 1;
        }
        self.doc.blocks.push(Block::Item((indent / 2 + 1).min(3), marker, inline(&paragraph_text(&text))));
    }

    fn table(&mut self) {
        let mut header = true;
        while let Some(line) = self.peek().filter(|l| is_table_row(l)) {
            self.at += 1;
            if is_table_separator(line) {
                continue;
            }
            let cells = table_cells(line).into_iter().map(|cell| {
                let mut spans = inline(cell);
                if header && self.peek().is_some_and(is_table_separator) {
                    spans.iter_mut().for_each(|s| s.bold = true);
                }
                spans
            });
            self.doc.blocks.push(Block::Row(cells.collect()));
            header = false;
        }
    }

    fn paragraph(&mut self) {
        let start = self.at;
        self.at += 1;
        while let Some(line) = self.peek().filter(|l| !l.trim().is_empty() && !starts_block(l)) {
            let underline = line.trim();
            if !underline.is_empty() && (underline.chars().all(|c| c == '=') || underline.chars().all(|c| c == '-')) {
                let level = if underline.starts_with('=') { 1 } else { 2 };
                self.doc.blocks.push(Block::Heading(level, inline(&paragraph_text(&self.lines[start..self.at]))));
                self.at += 1;
                return;
            }
            self.at += 1;
        }
        if self.peek().map(str::trim).is_some_and(|u| u.len() >= 2 && u.chars().all(|c| c == '-')) {
            self.doc.blocks.push(Block::Heading(2, inline(&paragraph_text(&self.lines[start..self.at]))));
            self.at += 1;
            return;
        }
        self.doc.blocks.push(Block::Paragraph(inline(&paragraph_text(&self.lines[start..self.at]))));
    }

    fn block(&mut self, line: &'a str) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            self.at += 1;
        } else if let Some(marker) = fence(line) {
            self.code(marker);
        } else if let Some((level, text)) = heading(trimmed) {
            self.doc.blocks.push(Block::Heading(level, inline(text)));
            self.at += 1;
        } else if is_rule(trimmed) {
            self.doc.blocks.push(Block::Rule);
            self.at += 1;
        } else if trimmed.starts_with('>') {
            self.quote();
        } else if let Some((indent, marker, text)) = list_marker(line) {
            self.item(indent, marker, text);
        } else if is_table_row(line) {
            self.table();
        } else {
            self.paragraph();
        }
    }
}

pub(super) fn parse_markdown(text: &str, _linked: &dyn Fn(&str) -> Option<String>) -> Document {
    let mut lines = Lines { lines: text.lines().collect(), at: 0, doc: Document::default() };
    lines.front_matter();
    while let Some(line) = lines.peek() {
        lines.block(line);
    }
    lines.doc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> Span {
        Span { text: text.into(), ..Span::default() }
    }

    #[test]
    fn reads_a_markdown_file() {
        let md = "---\ntitle: x\n---\n# Title\n\nSome **bold**, *italic*, `code` and a [link](https://a.b \"t\").\nSame paragraph.\n\n- one\n  continued\n  - nested\n1. first\n- [x] done\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n***\nSub\n---\nsnake_case_name ~~old~~\n";
        let blocks = parse_markdown(md, &|_| None).blocks;
        assert_eq!(blocks[0], Block::Code("title: x".into()));
        assert_eq!(blocks[1], Block::Heading(1, vec![span("Title")]));
        assert_eq!(
            blocks[2],
            Block::Paragraph(vec![
                span("Some "),
                Span { text: "bold".into(), bold: true, ..Span::default() },
                span(", "),
                Span { text: "italic".into(), italic: true, ..Span::default() },
                span(", "),
                Span { text: "code".into(), code: true, ..Span::default() },
                span(" and a "),
                Span { text: "link".into(), link: Some("https://a.b".into()), underline: true, ..Span::default() },
                span(". Same paragraph."),
            ])
        );
        assert_eq!(blocks[3], Block::Item(1, "•".into(), vec![span("one continued")]));
        assert_eq!(blocks[4], Block::Item(2, "•".into(), vec![span("nested")]));
        assert_eq!(blocks[5], Block::Item(1, "1.".into(), vec![span("first")]));
        assert_eq!(blocks[6], Block::Item(1, "☑".into(), vec![span("done")]));
        assert_eq!(blocks[7], Block::Quote(vec![span("quoted")]));
        assert_eq!(blocks[8], Block::Code("fn main() {}".into()));
        assert_eq!(blocks[9], Block::Row(vec![vec![Span { text: "A".into(), bold: true, ..Span::default() }], vec![Span { text: "B".into(), bold: true, ..Span::default() }]]));
        assert_eq!(blocks[10], Block::Row(vec![vec![span("1")], vec![span("2")]]));
        assert_eq!(blocks[11], Block::Rule);
        assert_eq!(blocks[12], Block::Heading(2, vec![span("Sub")]));
        assert_eq!(blocks[13], Block::Paragraph(vec![span("snake_case_name "), Span { text: "old".into(), strike: true, ..Span::default() }]));
        assert_eq!(blocks.len(), 14);
    }

    #[test]
    fn markdown_files_by_extension() {
        assert!(is_markdown("README.md") && is_markdown("docs/a.Markdown"));
        assert!(!is_markdown("a.html"));
    }
}
