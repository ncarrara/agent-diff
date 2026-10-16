use super::css::{self, Element, Paint, Stylesheet};
use super::rich_text::{BODY_SIZE, Block, Document, HEADING_SIZES, Span, push_span};
use std::path::Path;

pub(super) fn is_html(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml"))
}

const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const AUTO_CLOSED: [&str; 8] = ["li", "p", "td", "th", "tr", "dt", "dd", "option"];

fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "copy" => Some('©'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    let at = loop {
        let at = from + lower[from..].find(&format!("{name}="))?;
        if at == 0 || lower[..at].ends_with(char::is_whitespace) {
            break at + name.len() + 1;
        }
        from = at + 1;
    };
    let rest = &tag[at..];
    let value = match rest.chars().next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next()?,
        _ => rest.split(|c: char| c.is_whitespace() || c == '>').next()?,
    };
    Some(decode_entities(value))
}

#[derive(Clone)]
struct Computed {
    span: Span,
    size: f32,
    hidden: bool,
}

impl Default for Computed {
    fn default() -> Self {
        Computed { span: Span::default(), size: BODY_SIZE, hidden: false }
    }
}

fn is_bold(weight: &str) -> bool {
    matches!(weight, "bold" | "bolder") || weight.parse::<u32>().is_ok_and(|w| w >= 600)
}

fn is_monospace(family: &str) -> bool {
    let family = family.to_ascii_lowercase();
    ["mono", "courier", "consolas", "menlo"].iter().any(|m| family.contains(m))
}

impl Computed {
    fn set_size(&mut self, size: f32) {
        self.size = size;
        self.span.size = Some(size);
    }

    fn apply(&mut self, property: &str, value: &str, root_size: f32) {
        let lower = value.to_ascii_lowercase();
        match property {
            "color" => {
                if let Some(Paint::Color(c)) = css::color(value) {
                    self.span.color = Some(c);
                }
            }
            "background" | "background-color" => match css::color_in(value) {
                Some(Paint::Color(c)) => self.span.background = Some(c),
                Some(Paint::Transparent) => self.span.background = None,
                None => {}
            },
            "font-weight" => self.span.bold = is_bold(&lower),
            "font-style" => self.span.italic = lower.contains("italic") || lower.contains("oblique"),
            "font-size" => {
                if let Some(size) = css::length(value, self.size, root_size) {
                    self.set_size(size);
                }
            }
            "font-family" => self.span.code = is_monospace(value),
            "font" => {
                for word in lower.split_whitespace() {
                    if is_bold(word) {
                        self.span.bold = true;
                    } else if word == "italic" || word == "oblique" {
                        self.span.italic = true;
                    } else if let Some(size) = css::length(word.split('/').next().unwrap_or(word), self.size, root_size) {
                        self.set_size(size);
                    }
                }
                self.span.code = is_monospace(value);
            }
            "text-decoration" | "text-decoration-line" => {
                self.span.underline = lower.contains("underline");
                self.span.strike = lower.contains("line-through");
            }
            "display" => self.hidden |= lower == "none",
            "visibility" => self.hidden |= lower == "hidden" || lower == "collapse",
            _ => {}
        }
    }

    fn tag_defaults(&mut self, name: &str, tag: &str) {
        match name {
            "b" | "strong" | "th" => self.span.bold = true,
            "i" | "em" | "cite" | "var" => self.span.italic = true,
            "code" | "kbd" | "tt" | "samp" => self.span.code = true,
            "u" | "ins" => self.span.underline = true,
            "s" | "del" | "strike" => self.span.strike = true,
            "mark" => self.span.background = Some(eframe::egui::Color32::from_rgb(0xff, 0xf0, 0x80)),
            "a" => {
                self.span.link = attribute(tag, "href");
                self.span.underline = self.span.link.is_some();
                self.span.color = None;
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name[1..].parse::<usize>().unwrap_or(4).min(4);
                let size = HEADING_SIZES[level - 1] * self.size / BODY_SIZE;
                self.size = size;
                if self.span.size.is_some() {
                    self.span.size = Some(size);
                }
            }
            _ => {}
        }
    }
}

type MakeBlock = Box<dyn FnOnce(Vec<Span>) -> Block>;

#[derive(Default)]
struct Builder {
    sheet: Stylesheet,
    stack: Vec<(Element, Computed)>,
    root_size: f32,
    doc: Document,
    spans: Vec<Span>,
    pending: Option<MakeBlock>,
    lists: Vec<Option<usize>>,
    row: Option<Vec<Vec<Span>>>,
    preformatted: Option<String>,
}

fn element_of(name: &str, tag: &str) -> Element {
    let classes = attribute(tag, "class").map(|c| c.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
    Element { name: name.to_string(), id: attribute(tag, "id"), classes }
}

impl Builder {
    fn current(&self) -> Computed {
        self.stack.last().map(|(_, c)| c.clone()).unwrap_or_default()
    }

    fn text(&mut self, text: &str) {
        if self.current().hidden {
            return;
        }
        if let Some(code) = &mut self.preformatted {
            code.push_str(&decode_entities(text));
            return;
        }
        let collapsed = decode_entities(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        let leading = text.starts_with(char::is_whitespace) && !collapsed.is_empty();
        let trailing = text.ends_with(char::is_whitespace) && !collapsed.is_empty();
        if collapsed.is_empty() {
            if !text.is_empty() && !self.spans.is_empty() {
                self.push(" ");
            }
            return;
        }
        if leading && !self.spans.is_empty() {
            self.push(" ");
        }
        self.push(&collapsed);
        if trailing {
            self.push(" ");
        }
    }

    fn push(&mut self, text: &str) {
        let style = self.current().span;
        push_span(&mut self.spans, text, &style);
    }

    fn flush(&mut self) {
        let mut spans = std::mem::take(&mut self.spans);
        if let Some(first) = spans.first_mut() {
            first.text = first.text.trim_start().to_string();
        }
        if let Some(last) = spans.last_mut() {
            last.text = last.text.trim_end().to_string();
        }
        spans.retain(|s| !s.text.is_empty());
        let make = self.pending.take().unwrap_or_else(|| Box::new(Block::Paragraph));
        if spans.is_empty() {
            return;
        }
        match &mut self.row {
            Some(row) => row.push(spans),
            None => self.doc.blocks.push(make(spans)),
        }
    }

    fn computed_for(&mut self, element: &Element, tag: &str) -> Computed {
        let mut style = self.current();
        style.tag_defaults(&element.name, tag);
        let mut path: Vec<Element> = self.stack.iter().map(|(e, _)| e.clone()).collect();
        path.push(element.clone());
        let inline = attribute(tag, "style").map(|s| css::declarations(&s)).unwrap_or_default();
        self.doc.styled |= !inline.is_empty();
        for (property, value) in self.sheet.matching(&path).into_iter().chain(inline) {
            let value = self.sheet.resolve(&value);
            style.apply(&property, &value, self.root_size);
        }
        if element.name == "html" {
            self.root_size = style.size;
        }
        if matches!(element.name.as_str(), "html" | "body") && let Some(background) = style.span.background.take() {
            self.doc.background = Some(background);
        }
        style
    }

    fn open(&mut self, name: &str, tag: &str) {
        if AUTO_CLOSED.contains(&name) && self.stack.last().is_some_and(|(e, _)| e.name == name) {
            self.close(name);
        }
        let element = element_of(name, tag);
        let style = self.computed_for(&element, tag);
        let hidden = style.hidden;
        if !VOID.contains(&name) && !tag.ends_with('/') {
            self.stack.push((element, style));
        }
        if !hidden {
            self.structure_open(name, tag);
        }
    }

    fn close(&mut self, name: &str) {
        let Some(at) = self.stack.iter().rposition(|(e, _)| e.name == name) else { return };
        while self.stack.len() > at {
            let (element, style) = self.stack.pop().unwrap();
            if !style.hidden {
                self.structure_close(&element.name);
            }
        }
    }

    fn list_item(&mut self) -> MakeBlock {
        let depth = self.lists.len().clamp(1, 3);
        let marker = match self.lists.last_mut() {
            Some(Some(n)) => {
                *n += 1;
                format!("{n}.")
            }
            _ => "•".to_string(),
        };
        Box::new(move |spans| Block::Item(depth, marker, spans))
    }

    fn structure_open(&mut self, name: &str, tag: &str) {
        match name {
            "br" => self.push("\n"),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                let level = name[1..].parse::<u8>().unwrap_or(4).min(4);
                self.pending = Some(Box::new(move |s| Block::Heading(level, s)));
            }
            "ul" | "ol" => {
                self.flush();
                let start = attribute(tag, "start").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
                self.lists.push((name == "ol").then_some(start - 1));
            }
            "li" => {
                self.flush();
                self.pending = Some(self.list_item());
            }
            "blockquote" => {
                self.flush();
                self.pending = Some(Box::new(Block::Quote));
            }
            "pre" => {
                self.flush();
                self.preformatted = Some(String::new());
            }
            "hr" => {
                self.flush();
                self.doc.blocks.push(Block::Rule);
            }
            "tr" => {
                self.flush();
                self.row = Some(Vec::new());
            }
            "td" | "th" => self.flush(),
            "img" => {
                let alt = attribute(tag, "alt").unwrap_or_else(|| "image".into());
                self.push(&format!("[{alt}]"));
            }
            "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside" | "table" | "dl" | "dt" | "dd" | "figure" => self.flush(),
            _ => {}
        }
    }

    fn structure_close(&mut self, name: &str) {
        match name {
            "ul" | "ol" => {
                self.flush();
                self.lists.pop();
            }
            "pre" => {
                if let Some(code) = self.preformatted.take() {
                    self.doc.blocks.push(Block::Code(code.trim_matches('\n').to_string()));
                }
            }
            "tr" => {
                self.flush();
                if let Some(row) = self.row.take().filter(|r| !r.is_empty()) {
                    self.doc.blocks.push(Block::Row(row));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "li" | "blockquote" | "div" | "section" | "article" | "aside" | "header" | "footer" | "main" | "nav" | "table" | "td" | "th" | "dt" | "dd" => {
                self.flush()
            }
            _ => {}
        }
    }
}

fn skip_until<'a>(rest: &'a str, end: &str) -> &'a str {
    let lower = rest.to_ascii_lowercase();
    lower.find(end).map_or("", |at| &rest[at + end.len()..])
}

fn is_local_href(href: &str) -> bool {
    !href.contains("://") && !href.starts_with("//") && !href.starts_with("data:")
}

fn stylesheet(html: &str, linked: &dyn Fn(&str) -> Option<String>) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    loop {
        let style = lower[at..].find("<style").map(|i| at + i);
        let link = lower[at..].find("<link").map(|i| at + i);
        let next = match (style, link) {
            (Some(s), Some(l)) => s.min(l),
            (Some(s), None) => s,
            (None, Some(l)) => l,
            (None, None) => break,
        };
        let tag_end = lower[next..].find('>').map_or(lower.len(), |i| next + i);
        let tag = &html[next..tag_end];
        if Some(next) == style {
            let close = lower[tag_end..].find("</style").map_or(lower.len(), |i| tag_end + i);
            sheet.add(&html[(tag_end + 1).min(close)..close]);
            at = close;
        } else {
            if attribute(tag, "rel").is_some_and(|rel| rel.to_ascii_lowercase().contains("stylesheet"))
                && let Some(href) = attribute(tag, "href").filter(|h| is_local_href(h))
                && let Some(text) = linked(href.split(['?', '#']).next().unwrap_or(&href))
            {
                sheet.add(&text);
            }
            at = tag_end;
        }
        if at >= lower.len() {
            break;
        }
    }
    sheet
}

pub(super) fn parse_html(html: &str, linked: &dyn Fn(&str) -> Option<String>) -> Document {
    let sheet = stylesheet(html, linked);
    let mut builder = Builder { doc: Document { styled: !sheet.is_empty(), ..Document::default() }, sheet, root_size: BODY_SIZE, ..Builder::default() };
    let mut rest = html;
    let mut title = None;
    while !rest.is_empty() {
        let Some(at) = rest.find('<') else {
            builder.text(rest);
            break;
        };
        builder.text(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with("<!--") {
            rest = skip_until(rest, "-->");
            continue;
        }
        let end = rest.find('>').unwrap_or(rest.len() - 1);
        let tag = &rest[1..end];
        rest = &rest[(end + 1).min(rest.len())..];
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        match (closing, name.as_str()) {
            (_, "") => {}
            (false, "script" | "style" | "noscript" | "template") => rest = skip_until(skip_until(rest, &format!("</{name}")), ">"),
            (false, "title") => {
                let lower = rest.to_ascii_lowercase();
                let close = lower.find("</title").unwrap_or(rest.len());
                title = Some(decode_entities(rest[..close].trim()));
                rest = &rest[close..];
            }
            (true, name) => builder.close(name),
            (false, name) => builder.open(name, tag),
        }
    }
    builder.flush();
    let mut doc = builder.doc;
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        doc.blocks.insert(0, Block::Title(title));
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn span(text: &str) -> Span {
        Span { text: text.into(), ..Span::default() }
    }

    fn no_files(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn reads_the_structure_of_a_page() {
        let html = r#"<!doctype html><html><head><title>My &amp; page</title>
            <script>alert("<p>no</p>")</script></head><body>
            <h1>Hello <em>world</em></h1>
            <!-- a comment <p>hidden</p> -->
            <p>Some <b>bold</b> text and a <a href="https://example.com">link</a>.</p>
            <ul><li>one</li><li>two<ul><li>nested</li></ul></li></ul>
            <ol><li>first<li>second</ol>
            <pre>  fn main() {
    x &lt; 3
}</pre>
            <table><tr><th>Name</th><td>Value</td></tr></table>
            <hr><p>caf&#233; &#x2764;</p></body></html>"#;
        let blocks = parse_html(html, &no_files).blocks;
        assert_eq!(blocks[0], Block::Title("My & page".into()));
        assert_eq!(blocks[1], Block::Heading(1, vec![span("Hello "), Span { text: "world".into(), italic: true, ..Span::default() }]));
        assert_eq!(
            blocks[2],
            Block::Paragraph(vec![
                span("Some "),
                Span { text: "bold".into(), bold: true, ..Span::default() },
                span(" text and a "),
                Span { text: "link".into(), link: Some("https://example.com".into()), underline: true, ..Span::default() },
                span("."),
            ])
        );
        assert_eq!(blocks[3], Block::Item(1, "•".into(), vec![span("one")]));
        assert_eq!(blocks[4], Block::Item(1, "•".into(), vec![span("two")]));
        assert_eq!(blocks[5], Block::Item(2, "•".into(), vec![span("nested")]));
        assert_eq!(blocks[6], Block::Item(1, "1.".into(), vec![span("first")]));
        assert_eq!(blocks[7], Block::Item(1, "2.".into(), vec![span("second")]));
        assert_eq!(blocks[8], Block::Code("  fn main() {\n    x < 3\n}".into()));
        assert_eq!(blocks[9], Block::Row(vec![vec![Span { text: "Name".into(), bold: true, ..Span::default() }], vec![span("Value")]]));
        assert_eq!(blocks[10], Block::Rule);
        assert_eq!(blocks[11], Block::Paragraph(vec![span("café ❤")]));
        assert_eq!(blocks.len(), 12);
        assert!(!format!("{blocks:?}").contains("alert") && !format!("{blocks:?}").contains("hidden"));
    }

    #[test]
    fn applies_the_page_css() {
        let html = r#"<html><head><style>
            body { background: #102030; color: #eee; font-size: 16px }
            .warn { color: rgb(255, 0, 0); font-weight: 700 }
            #secret, .hidden { display: none }
            p em { font-size: 2em }
        </style><link rel="stylesheet" href="site.css?v=2"></head><body>
            <p class="warn">Careful <em>now</em></p>
            <p id="secret">nope</p>
            <p style="text-decoration: line-through">old</p>
            <div class="code">mono</div>
        </body></html>"#;
        let linked = |href: &str| (href == "site.css").then(|| ".code { font-family: 'Fira Mono', monospace }".to_string());
        let doc = parse_html(html, &linked);
        assert!(doc.styled);
        assert_eq!(doc.background, Some(Color32::from_rgb(0x10, 0x20, 0x30)));
        let Block::Paragraph(warn) = &doc.blocks[0] else { panic!("{:?}", doc.blocks[0]) };
        assert_eq!((warn[0].color, warn[0].bold, warn[0].size), (Some(Color32::RED), true, Some(16.0)));
        assert_eq!((warn[1].text.as_str(), warn[1].italic, warn[1].size), ("now", true, Some(32.0)));
        assert!(!format!("{:?}", doc.blocks).contains("nope"));
        let Block::Paragraph(old) = &doc.blocks[1] else { panic!() };
        assert!(old[0].strike && old[0].color == Some(Color32::from_rgb(0xee, 0xee, 0xee)));
        let Block::Paragraph(mono) = &doc.blocks[2] else { panic!() };
        assert!(mono[0].code);
    }

    #[test]
    fn html_files_by_extension() {
        assert!(is_html("site/index.html") && is_html("a.HTM"));
        assert!(!is_html("a.md"));
    }
}
