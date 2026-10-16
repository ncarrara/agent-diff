use super::*;
use std::path::{Path, PathBuf};

pub(super) const BODY_SIZE: f32 = 14.0;
pub(super) const HEADING_SIZES: [f32; 4] = [26.0, 21.0, 18.0, 16.0];

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Span {
    pub(super) text: String,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) code: bool,
    pub(super) underline: bool,
    pub(super) strike: bool,
    pub(super) link: Option<String>,
    pub(super) color: Option<Color32>,
    pub(super) background: Option<Color32>,
    pub(super) size: Option<f32>,
}

impl Span {
    pub(super) fn same_style(&self, other: &Span) -> bool {
        Span { text: String::new(), ..self.clone() } == Span { text: String::new(), ..other.clone() }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) enum Block {
    #[cfg_attr(not(feature = "html"), allow(dead_code))]
    Title(String),
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    Item(usize, String, Vec<Span>),
    Quote(Vec<Span>),
    Code(String),
    Row(Vec<Vec<Span>>),
    Rule,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Document {
    pub(super) blocks: Vec<Block>,
    pub(super) background: Option<Color32>,
    pub(super) styled: bool,
}

pub(super) fn push_span(spans: &mut Vec<Span>, text: &str, style: &Span) {
    match spans.last_mut() {
        Some(last) if last.same_style(style) => last.text.push_str(text),
        _ => spans.push(Span { text: text.to_string(), ..style.clone() }),
    }
}

struct Palette {
    text: Color32,
    strong: Color32,
    weak: Color32,
    link: Color32,
    code_background: Color32,
    background: Color32,
}

impl Palette {
    fn of(ui: &egui::Ui, doc: &Document) -> Self {
        if doc.styled {
            return Palette {
                text: Color32::from_gray(20),
                strong: Color32::BLACK,
                weak: Color32::from_gray(90),
                link: Color32::from_rgb(0, 0, 0xee),
                code_background: Color32::from_gray(238),
                background: doc.background.unwrap_or(Color32::WHITE),
            };
        }
        let v = ui.visuals();
        Palette { text: v.text_color(), strong: v.strong_text_color(), weak: v.weak_text_color(), link: v.hyperlink_color, code_background: v.faint_bg_color, background: doc.background.unwrap_or(v.extreme_bg_color) }
    }
}

fn spans_job(spans: &[Span], size: f32, color: Color32, palette: &Palette, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for span in spans {
        let size = span.size.unwrap_or(size);
        let font = if span.code { egui::FontId::monospace(size * 0.92) } else { egui::FontId::proportional(size) };
        let fallback = if span.link.is_some() { palette.link } else if span.bold { palette.strong } else { color };
        let color = span.color.unwrap_or(fallback);
        let line = |on: bool| if on { egui::Stroke::new(1.0, color) } else { egui::Stroke::NONE };
        let background = span.background.or(span.code.then_some(palette.code_background)).unwrap_or(Color32::TRANSPARENT);
        job.append(
            &span.text,
            0.0,
            egui::TextFormat { font_id: font, color, background, italics: span.italic, underline: line(span.underline), strikethrough: line(span.strike), ..Default::default() },
        );
    }
    job.wrap.max_width = width;
    job
}

fn blocks_ui(ui: &mut egui::Ui, doc: &Document) {
    let palette = Palette::of(ui, doc);
    let width = |ui: &egui::Ui| ui.available_width();
    for block in &doc.blocks {
        match block {
            Block::Title(title) => {
                ui.label(RichText::new(title).color(palette.weak).italics());
            }
            Block::Heading(level, spans) => {
                ui.add_space(6.0);
                let size = HEADING_SIZES[(*level as usize).clamp(1, 4) - 1];
                ui.label(spans_job(spans, size, palette.strong, &palette, width(ui)));
            }
            Block::Paragraph(spans) => {
                ui.add_space(3.0);
                ui.label(spans_job(spans, BODY_SIZE, palette.text, &palette, width(ui)));
            }
            Block::Item(depth, marker, spans) => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(*depth as f32 * 16.0);
                    ui.label(RichText::new(marker).color(palette.text));
                    ui.label(spans_job(spans, BODY_SIZE, palette.text, &palette, width(ui)));
                });
            }
            Block::Quote(spans) => {
                ui.horizontal(|ui| {
                    let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, BODY_SIZE + 4.0), egui::Sense::hover());
                    ui.painter().rect_filled(bar, 1.0, palette.weak);
                    ui.label(spans_job(spans, BODY_SIZE, palette.weak, &palette, width(ui)));
                });
            }
            Block::Code(code) => {
                egui::Frame::NONE.fill(palette.code_background).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                    ui.label(RichText::new(code).monospace().color(palette.text));
                });
            }
            Block::Row(cells) => {
                ui.horizontal_wrapped(|ui| {
                    for (i, cell) in cells.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new("│").color(palette.weak));
                        }
                        ui.label(spans_job(cell, BODY_SIZE, palette.text, &palette, width(ui)));
                    }
                });
            }
            Block::Rule => {
                ui.separator();
            }
        }
    }
}

pub(super) type Parse = fn(&str, &dyn Fn(&str) -> Option<String>) -> Document;

pub(super) struct Page {
    path: PathBuf,
    doc: Document,
}

impl Page {
    fn load(path: PathBuf, root: &Path, parse: Parse) -> Option<Self> {
        let bytes = std::fs::read(&path).ok()?;
        let dir = path.parent().unwrap_or(root).to_path_buf();
        let linked = |href: &str| {
            let file = match href.strip_prefix('/') {
                Some(from_root) => root.join(from_root),
                None => dir.join(href),
            };
            std::fs::read_to_string(file).ok()
        };
        Some(Page { doc: parse(&String::from_utf8_lossy(&bytes), &linked), path })
    }
}

pub(super) struct DocumentPair {
    key: (String, u64),
    local: Option<Page>,
    remote: Option<Page>,
    identical: bool,
    browser: bool,
    error: Option<String>,
}

impl DocumentPair {
    pub(super) fn load(e: &FileEntry, at: &FilePlace, parse: Parse, browser: bool) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        let local_root = Path::new(at.local_root);
        let local = local_root.join(local_path);
        let remote = at.mirror.join(remote_path);
        let identical = e.origin == Origin::Synced || (local.exists() && crate::engine::same_content(&local, &remote));
        DocumentPair {
            key: (e.path.clone(), e.version),
            local: Page::load(local, local_root, parse),
            remote: if identical { None } else { Page::load(remote, at.mirror, parse) },
            identical,
            browser,
            error: None,
        }
    }
}

fn open_in_browser(path: &Path) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ()).map_err(|e| format!("cannot open a browser (xdg-open): {e}"))
}

struct Side<'a> {
    title: &'a str,
    color: Color32,
    page: Option<&'a Page>,
    id: &'a str,
}

fn page_ui(ui: &mut egui::Ui, side: Side, browser: bool, error: &mut Option<String>) {
    let Some(page) = side.page else {
        ui.horizontal(|ui| {
            ui.label(RichText::new(side.title).color(side.color).strong());
            ui.weak("not present");
        });
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(side.title).color(side.color).strong());
        if browser
            && ui.small_button("🌐 Open in browser").on_hover_text("See the real page, with its scripts and full layout").clicked()
            && let Err(e) = open_in_browser(&page.path)
        {
            *error = Some(e);
        }
    });
    let fill = Palette::of(ui, &page.doc).background;
    egui::Frame::NONE.fill(fill).inner_margin(10.0).corner_radius(4.0).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(side.id).auto_shrink(false).show(ui, |ui| blocks_ui(ui, &page.doc));
    });
}

impl Preview for DocumentPair {
    fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        let mut error = None;
        if self.identical {
            page_ui(ui, Side { title: "Identical on both sides", color: Color32::GRAY, page: self.local.as_ref(), id: "doc-only" }, self.browser, &mut error);
        } else {
            let (local, remote, browser) = (self.local.as_ref(), self.remote.as_ref(), self.browser);
            ui.columns(2, |columns| {
                page_ui(&mut columns[0], Side { title: "Local (yours)", color: YOU, page: local, id: "doc-local" }, browser, &mut error);
                page_ui(&mut columns[1], Side { title: "Remote (Claude)", color: CLAUDE, page: remote, id: "doc-remote" }, browser, &mut error);
            });
        }
        if error.is_some() {
            self.error = error;
        }
    }
}
