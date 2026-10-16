use eframe::egui::Color32;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Element {
    pub(super) name: String,
    pub(super) id: Option<String>,
    pub(super) classes: Vec<String>,
}

#[derive(Debug)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Compound {
    fn matches(&self, e: &Element) -> bool {
        self.tag.as_ref().is_none_or(|t| *t == e.name) && self.id.as_ref().is_none_or(|id| e.id.as_ref() == Some(id)) && self.classes.iter().all(|c| e.classes.contains(c))
    }
}

#[derive(Debug)]
struct Rule {
    selector: Vec<Compound>,
    specificity: (usize, usize, usize),
    order: usize,
    declarations: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub(super) struct Stylesheet {
    rules: Vec<Rule>,
    vars: HashMap<String, String>,
}

fn strip_comments(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        rest = rest[at + 2..].find("*/").map_or("", |end| &rest[at + 2 + end + 2..]);
    }
    out + rest
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

fn media_applies(rule: &str) -> bool {
    let rule = rule.to_ascii_lowercase();
    if let Some(query) = rule.strip_prefix("@media") {
        return !["print", "dark", "max-width", "speech"].iter().any(|skip| query.contains(skip));
    }
    rule.starts_with("@supports") || rule.starts_with("@layer")
}

fn compound(part: &str) -> Option<Compound> {
    if part == ":root" {
        return Some(Compound { tag: Some("html".into()), id: None, classes: Vec::new() });
    }
    if part.contains(':') {
        return None;
    }
    let mut compound = Compound { tag: None, id: None, classes: Vec::new() };
    let name_end = part.find(['.', '#']).unwrap_or(part.len());
    let tag = &part[..name_end];
    if !tag.is_empty() && tag != "*" {
        compound.tag = Some(tag.to_ascii_lowercase());
    }
    let mut rest = &part[name_end..];
    while let Some(kind) = rest.chars().next() {
        let body = &rest[1..];
        let end = body.find(['.', '#']).unwrap_or(body.len());
        let name = body[..end].to_string();
        if name.is_empty() {
            return None;
        }
        if kind == '#' {
            compound.id = Some(name);
        } else {
            compound.classes.push(name);
        }
        rest = &body[end..];
    }
    Some(compound)
}

fn selector(text: &str) -> Option<Vec<Compound>> {
    let text = text.trim();
    if text.is_empty() || text.contains(['+', '~', '[']) {
        return None;
    }
    text.replace('>', " ").split_whitespace().map(compound).collect()
}

fn specificity(selector: &[Compound]) -> (usize, usize, usize) {
    selector.iter().fold((0, 0, 0), |(ids, classes, tags), c| (ids + c.id.is_some() as usize, classes + c.classes.len(), tags + c.tag.is_some() as usize))
}

pub(super) fn declarations(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter_map(|d| d.split_once(':'))
        .map(|(property, value)| {
            let value = value.trim();
            let value = value.strip_suffix("!important").unwrap_or(value).trim();
            (property.trim().to_ascii_lowercase(), value.to_string())
        })
        .filter(|(property, value)| !property.is_empty() && !value.is_empty())
        .collect()
}

fn selector_matches(selector: &[Compound], path: &[Element]) -> bool {
    let (Some((target, outer)), Some((element, ancestors))) = (selector.split_last(), path.split_last()) else { return false };
    if !target.matches(element) {
        return false;
    }
    let mut remaining = ancestors.len();
    for wanted in outer.iter().rev() {
        loop {
            if remaining == 0 {
                return false;
            }
            remaining -= 1;
            if wanted.matches(&ancestors[remaining]) {
                break;
            }
        }
    }
    true
}

impl Stylesheet {
    pub(super) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(super) fn add(&mut self, css: &str) {
        self.add_rules(&strip_comments(css));
    }

    fn add_rules(&mut self, css: &str) {
        let mut rest = css;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                return;
            }
            let Some(open) = rest.find('{') else { return };
            if rest.starts_with('@') {
                if let Some(semi) = rest.find(';').filter(|&semi| semi < open) {
                    rest = &rest[semi + 1..];
                    continue;
                }
                let Some(close) = matching_brace(rest, open) else { return };
                if media_applies(&rest[..open]) {
                    self.add_rules(&rest[open + 1..close]);
                }
                rest = &rest[close + 1..];
                continue;
            }
            let Some(close) = rest[open..].find('}').map(|i| open + i) else { return };
            let mut own = Vec::new();
            for (property, value) in declarations(&rest[open + 1..close]) {
                if property.starts_with("--") {
                    self.vars.insert(property, value);
                } else {
                    own.push((property, value));
                }
            }
            for text in rest[..open].split(',') {
                if let Some(selector) = selector(text) {
                    let order = self.rules.len();
                    self.rules.push(Rule { specificity: specificity(&selector), selector, order, declarations: own.clone() });
                }
            }
            rest = &rest[close + 1..];
        }
    }

    pub(super) fn matching(&self, path: &[Element]) -> Vec<(String, String)> {
        let mut rules: Vec<&Rule> = self.rules.iter().filter(|r| selector_matches(&r.selector, path)).collect();
        rules.sort_by_key(|r| (r.specificity, r.order));
        rules.into_iter().flat_map(|r| r.declarations.iter().cloned()).collect()
    }

    pub(super) fn resolve(&self, value: &str) -> String {
        let mut value = value.to_string();
        for _ in 0..8 {
            let Some(at) = value.find("var(") else { break };
            let Some(close) = value[at..].find(')').map(|i| at + i) else { break };
            let inner = &value[at + 4..close];
            let (name, fallback) = inner.split_once(',').map_or((inner.trim(), ""), |(n, f)| (n.trim(), f.trim()));
            let replacement = self.vars.get(name).cloned().unwrap_or_else(|| fallback.to_string());
            value.replace_range(at..=close, &replacement);
        }
        value
    }
}

fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    Some(match name {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "silver" => (192, 192, 192),
        "whitesmoke" => (245, 245, 245),
        "gainsboro" => (220, 220, 220),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "slategray" | "slategrey" => (112, 128, 144),
        "maroon" => (128, 0, 0),
        "darkred" => (139, 0, 0),
        "crimson" => (220, 20, 60),
        "tomato" => (255, 99, 71),
        "coral" => (255, 127, 80),
        "orange" => (255, 165, 0),
        "gold" => (255, 215, 0),
        "yellow" => (255, 255, 0),
        "olive" => (128, 128, 0),
        "lime" => (0, 255, 0),
        "darkgreen" => (0, 100, 0),
        "teal" => (0, 128, 128),
        "aqua" | "cyan" => (0, 255, 255),
        "navy" => (0, 0, 128),
        "darkblue" => (0, 0, 139),
        "steelblue" => (70, 130, 180),
        "royalblue" => (65, 105, 225),
        "purple" => (128, 0, 128),
        "indigo" => (75, 0, 130),
        "violet" => (238, 130, 238),
        "fuchsia" | "magenta" => (255, 0, 255),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        "beige" => (245, 245, 220),
        "ivory" => (255, 255, 240),
        _ => return None,
    })
}

fn channel(text: &str, scale: f32) -> Option<f32> {
    let text = text.trim();
    match text.strip_suffix('%') {
        Some(percent) => percent.trim().parse::<f32>().ok().map(|p| p / 100.0 * scale),
        None => text.parse().ok(),
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    ((r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0)
}

fn functional_color(value: &str) -> Option<Color32> {
    let open = value.find('(')?;
    let name = &value[..open];
    let inner = value[open + 1..].strip_suffix(')')?;
    let (channels, alpha) = match inner.split_once('/') {
        Some((channels, alpha)) => (channels.to_string(), Some(alpha)),
        None => (inner.replace(',', " "), None),
    };
    let parts: Vec<&str> = channels.split([' ', ',']).filter(|p| !p.is_empty()).collect();
    let alpha = alpha.or(parts.get(3).copied()).and_then(|a| channel(a, 1.0)).unwrap_or(1.0);
    let (r, g, b) = match name {
        "rgb" | "rgba" => (channel(parts.first()?, 255.0)?, channel(parts.get(1)?, 255.0)?, channel(parts.get(2)?, 255.0)?),
        "hsl" | "hsla" => hsl_to_rgb(parts.first()?.trim_end_matches("deg").parse().ok()?, channel(parts.get(1)?, 1.0)?, channel(parts.get(2)?, 1.0)?),
        _ => return None,
    };
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    Some(Color32::from_rgba_unmultiplied(byte(r), byte(g), byte(b), byte(alpha * 255.0)))
}

fn hex_color(hex: &str) -> Option<Color32> {
    let digits: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    let pair = |i: usize| digits[i] * 16 + digits[i + 1];
    match digits.len() {
        3 | 4 => {
            let alpha = if digits.len() == 4 { digits[3] * 17 } else { 255 };
            Some(Color32::from_rgba_unmultiplied(digits[0] * 17, digits[1] * 17, digits[2] * 17, alpha))
        }
        6 => Some(Color32::from_rgb(pair(0), pair(2), pair(4))),
        8 => Some(Color32::from_rgba_unmultiplied(pair(0), pair(2), pair(4), pair(6))),
        _ => None,
    }
}

pub(super) enum Paint {
    Color(Color32),
    Transparent,
}

pub(super) fn color(value: &str) -> Option<Paint> {
    let value = value.trim().to_ascii_lowercase();
    if value == "transparent" {
        return Some(Paint::Transparent);
    }
    if let Some(hex) = value.strip_prefix('#') {
        return hex_color(hex).map(Paint::Color);
    }
    if value.contains('(') {
        return functional_color(&value).map(|c| if c.a() == 0 { Paint::Transparent } else { Paint::Color(c) });
    }
    named_color(&value).map(|(r, g, b)| Paint::Color(Color32::from_rgb(r, g, b)))
}

pub(super) fn color_in(value: &str) -> Option<Paint> {
    color(value).or_else(|| value.split_whitespace().find_map(color))
}

pub(super) fn length(value: &str, parent: f32, root: f32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    let keyword = match value.as_str() {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "smaller" => Some(parent * 0.83),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    if keyword.is_some() {
        return keyword;
    }
    let number = |suffix: &str| value.strip_suffix(suffix).and_then(|n| n.trim().parse::<f32>().ok());
    number("rem").map(|n| n * root)
        .or_else(|| number("em").map(|n| n * parent))
        .or_else(|| number("px"))
        .or_else(|| number("pt").map(|n| n * 4.0 / 3.0))
        .or_else(|| number("%").map(|n| n / 100.0 * parent))
        .filter(|size| (4.0..=96.0).contains(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(name: &str, id: Option<&str>, classes: &[&str]) -> Element {
        Element { name: name.into(), id: id.map(str::to_string), classes: classes.iter().map(|c| c.to_string()).collect() }
    }

    #[test]
    fn rules_by_specificity_then_order() {
        let mut sheet = Stylesheet::default();
        sheet.add(":root { --accent: #f00 } /* c */ p { color: blue } .note p { color: var(--accent) } #main { color: green } a:hover { color: pink } @media print { p { color: black } }");
        let path = [element("html", None, &[]), element("div", None, &["note"]), element("p", None, &[])];
        let found = sheet.matching(&path);
        assert_eq!(found, [("color".to_string(), "blue".to_string()), ("color".to_string(), "var(--accent)".to_string())]);
        assert_eq!(sheet.resolve("var(--accent)"), "#f00");
        assert!(sheet.matching(&[element("p", Some("main"), &[])]).iter().any(|(_, v)| v == "green"));
    }

    #[test]
    fn colors_and_sizes() {
        let rgb = |p: Option<Paint>| match p {
            Some(Paint::Color(c)) => Some((c.r(), c.g(), c.b())),
            _ => None,
        };
        assert_eq!(rgb(color("#0a0")), Some((0, 170, 0)));
        assert_eq!(rgb(color("rgb(10, 20, 30)")), Some((10, 20, 30)));
        assert_eq!(rgb(color("hsl(0 100% 50%)")), Some((255, 0, 0)));
        assert_eq!(rgb(color_in("#fff url(a.png) no-repeat")), Some((255, 255, 255)));
        assert!(matches!(color("transparent"), Some(Paint::Transparent)));
        assert_eq!(length("1.5em", 14.0, 16.0), Some(21.0));
        assert_eq!(length("2rem", 14.0, 16.0), Some(32.0));
    }
}
