use super::*;
use resvg::{tiny_skia, usvg};
use std::path::Path;
use std::sync::OnceLock;

pub(super) fn is_svg(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

fn options() -> usvg::Options<'static> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        Arc::new(fonts)
    });
    usvg::Options { fontdb: fonts.clone(), ..usvg::Options::default() }
}

pub(super) fn parse_svg(bytes: &[u8]) -> Result<usvg::Tree, String> {
    usvg::Tree::from_data(bytes, &options()).map_err(|e| format!("cannot read the SVG: {e}"))
}

pub(super) fn rasterize(tree: &usvg::Tree, room: [u32; 2]) -> Option<egui::ColorImage> {
    let size = tree.size();
    let scale = (room[0] as f32 / size.width()).min(room[1] as f32 / size.height());
    let (width, height) = ((size.width() * scale).round().max(1.0) as u32, (size.height() * scale).round().max(1.0) as u32);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied([width as usize, height as usize], pixmap.data()))
}

struct Picture {
    tree: Result<usvg::Tree, String>,
    bytes: usize,
    shown: Option<([u32; 2], egui::TextureHandle)>,
}

impl Picture {
    fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        Some(Picture { tree: parse_svg(&bytes), bytes: bytes.len(), shown: None })
    }
}

pub(super) struct SvgPair {
    key: (String, u64),
    local: Option<Picture>,
    remote: Option<Picture>,
    identical: bool,
}

impl SvgPair {
    pub(super) fn load(e: &FileEntry, at: &FilePlace) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        let local = Path::new(at.local_root).join(local_path);
        let remote = at.mirror.join(remote_path);
        let identical = e.origin == Origin::Synced || (local.exists() && crate::engine::same_content(&local, &remote));
        SvgPair {
            key: (e.path.clone(), e.version),
            local: Picture::load(&local),
            remote: if identical { None } else { Picture::load(&remote) },
            identical,
        }
    }
}

fn picture_ui(ui: &mut egui::Ui, title: &str, color: Color32, picture: Option<&mut Picture>) {
    let Some(picture) = picture else {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(color).strong());
            ui.weak("not present");
        });
        return;
    };
    let tree = match &picture.tree {
        Ok(tree) => tree,
        Err(e) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).color(color).strong());
                ui.colored_label(Color32::LIGHT_RED, e);
            });
            return;
        }
    };
    let size = tree.size();
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).color(color).strong());
        ui.weak(format!("{:.0} × {:.0} · {} bytes", size.width(), size.height(), picture.bytes));
    });
    let room = ui.available_size() - egui::vec2(0.0, 6.0);
    let room = [room.x.max(16.0) as u32, room.y.max(16.0) as u32];
    if picture.shown.as_ref().is_none_or(|(at, _)| *at != room)
        && let Some(image) = rasterize(tree, room)
    {
        let texture = ui.ctx().load_texture(format!("svg-{title}"), image, egui::TextureOptions::LINEAR);
        picture.shown = Some((room, texture));
    }
    let Some((_, texture)) = &picture.shown else { return };
    let [w, h] = texture.size();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w as f32, h as f32), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, Color32::from_gray(235));
    ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, ui.visuals().weak_text_color()), egui::StrokeKind::Outside);
}

impl Preview for SvgPair {
    fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.identical {
            picture_ui(ui, "Identical on both sides", Color32::GRAY, self.local.as_mut());
            return;
        }
        let (local, remote) = (self.local.as_mut(), self.remote.as_mut());
        ui.columns(2, |columns| {
            picture_ui(&mut columns[0], "Local (yours)", YOU, local);
            picture_ui(&mut columns[1], "Remote (Claude)", CLAUDE, remote);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CIRCLE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#ffffff"/><circle cx="25" cy="25" r="20" fill="#ff0000"/></svg>"##;

    #[test]
    fn svg_files_by_extension() {
        assert!(is_svg("icons/logo.svg") && is_svg("A.SVG"));
        assert!(!is_svg("a.png") && !is_svg("svg"));
    }

    #[test]
    fn renders_scaled_to_the_room() {
        let tree = parse_svg(CIRCLE).unwrap();
        let image = rasterize(&tree, [400, 400]).unwrap();
        assert_eq!(image.size, [400, 200]);
        let at = |x: usize, y: usize| image.pixels[y * 400 + x];
        assert!(at(100, 100).r() > 200 && at(100, 100).g() < 50, "{:?}", at(100, 100));
        assert!(at(350, 100).g() > 200, "{:?}", at(350, 100));
        assert!(parse_svg(b"<svg").is_err());
    }
}
