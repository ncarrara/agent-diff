use super::*;
use std::path::{Path, PathBuf};

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

pub(super) fn is_image(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub(super) struct ImagePair {
    key: (String, u64),
    local: Option<Picture>,
    remote: Option<Picture>,
    identical: bool,
}

struct Picture {
    texture: Result<egui::TextureHandle, String>,
    bytes: usize,
}

impl ImagePair {
    pub(super) fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    pub(super) fn load(ctx: &egui::Context, e: &FileEntry, local_root: &str, mirror: &Path) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        let (local, remote) = (Path::new(local_root).join(local_path), mirror.join(remote_path));
        let identical = e.origin == Origin::Synced || (local.exists() && crate::engine::same_content(&local, &remote));
        let picture = |side: &str, file: PathBuf| {
            let bytes = std::fs::metadata(&file).ok().filter(|m| m.is_file())?.len() as usize;
            let texture = if bytes > MAX_IMAGE_FILE {
                Err(format!("too big to preview (over {})", human_size(MAX_IMAGE_FILE)))
            } else {
                std::fs::read(&file).map_err(|e| format!("cannot read: {e}")).and_then(|b| decode(ctx, &format!("{side}:{}:{}", e.path, e.version), &b))
            };
            Some(Picture { texture, bytes })
        };
        ImagePair {
            key: (e.path.clone(), e.version),
            local: picture("local", local),
            remote: if identical { None } else { picture("remote", remote) },
            identical,
        }
    }
}

const MAX_IMAGE_FILE: usize = 64 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 8192;
const MAX_IMAGE_ALLOC: u64 = 256 * 1024 * 1024;

fn decode(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Result<egui::TextureHandle, String> {
    let cannot = |e: &dyn std::fmt::Display| format!("cannot decode: {e}");
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| cannot(&e))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| cannot(&e))?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Ok(ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR))
}

pub(super) fn image_view(ui: &mut egui::Ui, pair: &ImagePair) {
    if pair.identical {
        picture_ui(ui, "Identical locally and on the remote", Color32::GRAY, pair.local.as_ref());
        return;
    }
    ui.columns(2, |columns| {
        picture_ui(&mut columns[0], "Local (yours)", YOU, pair.local.as_ref());
        picture_ui(&mut columns[1], "Remote (Claude)", CLAUDE, pair.remote.as_ref());
    });
}

fn picture_ui(ui: &mut egui::Ui, title: &str, color: Color32, picture: Option<&Picture>) {
    ui.label(RichText::new(title).color(color).strong());
    let Some(picture) = picture else {
        ui.weak("not present on this side");
        return;
    };
    match &picture.texture {
        Err(e) => {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        Ok(texture) => {
            let [w, h] = texture.size();
            ui.weak(format!("{w} × {h} px · {}", human_size(picture.bytes)));
            let room = ui.available_size() - egui::vec2(0.0, 8.0);
            let scale = (room.x / w as f32).min(room.y / h as f32).min(1.0).max(0.01);
            let size = egui::vec2(w as f32, h as f32) * scale;
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            checkerboard(ui, rect);
            ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, ui.visuals().weak_text_color()), egui::StrokeKind::Outside);
        }
    }
}

fn checkerboard(ui: &egui::Ui, rect: egui::Rect) {
    const CELL: f32 = 8.0;
    let (light, dark) = (Color32::from_gray(200), Color32::from_gray(160));
    ui.painter().rect_filled(rect, 0.0, light);
    let painter = ui.painter().with_clip_rect(rect);
    let (cols, rows) = ((rect.width() / CELL).ceil() as usize, (rect.height() / CELL).ceil() as usize);
    for row in 0..rows {
        for col in (row % 2..cols).step_by(2) {
            let min = rect.min + egui::vec2(col as f32 * CELL, row as f32 * CELL);
            painter.rect_filled(egui::Rect::from_min_size(min, egui::vec2(CELL, CELL)), 0.0, dark);
        }
    }
}

fn human_size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f32 / 1024.0),
        b => format!("{:.1} MB", b as f32 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 128])).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn entry(origin: Origin, local_exists: bool) -> FileEntry {
        FileEntry {
            path: "img/logo.PNG".into(),
            rename: None,
            origin,
            local_change: None,
            remote_change: None,
            local_exists,
            remote_exists: true,
            body: Body::Binary,
            version: 7,
        }
    }

    fn texts(ctx: &egui::Context, pair: &ImagePair) -> Vec<String> {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| image_view(ui, pair));
        out.textures_delta.clear();
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn image_files_by_extension() {
        assert!(is_image("a/b.png") && is_image("x.JPG") && is_image("y.webp"));
        assert!(!is_image("main.rs") && !is_image("png") && !is_image("logo.svg"));
    }

    #[test]
    fn both_sides_side_by_side() {
        let root = std::env::temp_dir().join(format!("agent-diff-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (local, mirror) = (root.join("local"), root.join("mirror"));
        for dir in [&local, &mirror] {
            std::fs::create_dir_all(dir.join("img")).unwrap();
        }
        std::fs::write(local.join("img/logo.PNG"), png(2, 3)).unwrap();
        std::fs::write(mirror.join("img/logo.PNG"), png(40, 10)).unwrap();
        let ctx = egui::Context::default();
        let local_root = local.to_str().unwrap();

        let pair = ImagePair::load(&ctx, &entry(Origin::Remote, true), local_root, &mirror);
        assert!(pair.is_for(&entry(Origin::Remote, true)));
        let shown = texts(&ctx, &pair);
        for needle in ["Local (yours)", "Remote (Claude)", "2 × 3 px", "40 × 10 px"] {
            assert!(shown.iter().any(|t| t.contains(needle)), "{needle}: {shown:?}");
        }

        std::fs::remove_file(local.join("img/logo.PNG")).unwrap();
        let shown = texts(&ctx, &ImagePair::load(&ctx, &entry(Origin::Remote, false), local_root, &mirror));
        assert!(shown.iter().any(|t| t.contains("not present")), "{shown:?}");

        std::fs::write(local.join("img/logo.PNG"), b"not an image").unwrap();
        let shown = texts(&ctx, &ImagePair::load(&ctx, &entry(Origin::Both, true), local_root, &mirror));
        assert!(shown.iter().any(|t| t.contains("cannot decode")), "{shown:?}");

        std::fs::write(local.join("img/logo.PNG"), png(40, 10)).unwrap();
        let shown = texts(&ctx, &ImagePair::load(&ctx, &entry(Origin::Synced, true), local_root, &mirror));
        assert!(shown.iter().any(|t| t.contains("Identical")) && !shown.iter().any(|t| t.contains("Remote (Claude)")), "{shown:?}");
    }
}

impl Preview for ImagePair {
    fn is_for(&self, e: &FileEntry) -> bool {
        ImagePair::is_for(self, e)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        image_view(ui, self);
    }
}
