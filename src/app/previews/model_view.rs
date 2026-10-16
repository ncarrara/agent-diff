use super::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

const MODEL_EXTENSIONS: [&str; 2] = ["glb", "gltf"];

pub(super) fn is_model(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| MODEL_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

type Vec3 = [f32; 3];

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(a: Vec3) -> Vec3 {
    let length = dot(a, a).sqrt();
    if length > 0.0 { scale(a, 1.0 / length) } else { a }
}

type Matrix = [[f32; 4]; 4];

const IDENTITY: Matrix = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0; 4]; 4];
    for (col, out_col) in out.iter_mut().enumerate() {
        for (row, cell) in out_col.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][row] * b[col][k]).sum();
        }
    }
    out
}

fn transform_point(m: &Matrix, p: Vec3) -> Vec3 {
    let at = |row: usize| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row];
    [at(0), at(1), at(2)]
}

struct Texture {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Texture {
    fn sample(&self, uv: [f32; 2]) -> [f32; 4] {
        let wrap = |v: f32| v - v.floor();
        let x = ((wrap(uv[0]) * self.width as f32) as usize).min(self.width - 1);
        let y = ((wrap(uv[1]) * self.height as f32) as usize).min(self.height - 1);
        let i = (y * self.width + x) * 4;
        let c = |k: usize| self.rgba[i + k] as f32 / 255.0;
        [c(0), c(1), c(2), c(3)]
    }
}

struct Triangle {
    corners: [Vec3; 3],
    uvs: Option<[[f32; 2]; 3]>,
    color: [f32; 4],
    texture: Option<usize>,
}

pub(super) struct Model {
    triangles: Vec<Triangle>,
    textures: Vec<Option<Texture>>,
    min: Vec3,
    max: Vec3,
    stats: Stats,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(super) struct Stats {
    pub(super) meshes: usize,
    pub(super) vertices: usize,
    pub(super) triangles: usize,
    pub(super) materials: usize,
    pub(super) size: Vec3,
}

fn texture_from(image: &gltf::image::Data) -> Option<Texture> {
    let (width, height) = (image.width as usize, image.height as usize);
    let rgba = match image.format {
        gltf::image::Format::R8G8B8A8 => image.pixels.clone(),
        gltf::image::Format::R8G8B8 => image.pixels.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        gltf::image::Format::R8 => image.pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        _ => return None,
    };
    (width > 0 && height > 0 && rgba.len() == width * height * 4).then_some(Texture { width, height, rgba })
}

pub(super) fn load_model(path: &Path) -> Result<Model, String> {
    let (document, buffers, images) = gltf::import(path).map_err(|e| format!("cannot read the model: {e}"))?;
    let mut model = Model {
        triangles: Vec::new(),
        textures: images.iter().map(texture_from).collect(),
        min: [f32::MAX; 3],
        max: [f32::MIN; 3],
        stats: Stats { materials: document.materials().len(), ..Stats::default() },
    };
    let roots: Vec<(gltf::Node, Matrix)> = match document.default_scene().or_else(|| document.scenes().next()) {
        Some(scene) => scene.nodes().map(|n| (n, IDENTITY)).collect(),
        None => document.nodes().map(|n| (n, IDENTITY)).collect(),
    };
    let mut stack = roots;
    while let Some((node, parent)) = stack.pop() {
        let world = multiply(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            model.stats.meshes += 1;
            for primitive in mesh.primitives() {
                add_primitive(&mut model, &primitive, &buffers, &world);
            }
        }
        stack.extend(node.children().map(|child| (child, world)));
    }
    if model.triangles.is_empty() {
        return Err("no triangles to show in this model".into());
    }
    model.stats.size = sub(model.max, model.min);
    Ok(model)
}

fn add_primitive(model: &mut Model, primitive: &gltf::Primitive, buffers: &[gltf::buffer::Data], world: &Matrix) {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return;
    }
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
    let Some(positions) = reader.read_positions() else { return };
    let positions: Vec<Vec3> = positions.map(|p| transform_point(world, p)).collect();
    model.stats.vertices += positions.len();
    let indices: Vec<u32> = match reader.read_indices() {
        Some(indices) => indices.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };
    let material = primitive.material();
    let pbr = material.pbr_metallic_roughness();
    let base = pbr.base_color_factor();
    let texture_info = pbr.base_color_texture();
    let texture = texture_info.as_ref().map(|t| t.texture().source().index()).filter(|&i| model.textures.get(i).is_some_and(Option::is_some));
    let uvs: Option<Vec<[f32; 2]>> = texture_info.as_ref().and_then(|t| reader.read_tex_coords(t.tex_coord())).map(|uv| uv.into_f32().collect());
    let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
    for p in &positions {
        for k in 0..3 {
            model.min[k] = model.min[k].min(p[k]);
            model.max[k] = model.max[k].max(p[k]);
        }
    }
    for corner in indices.chunks_exact(3) {
        let [a, b, c] = [corner[0] as usize, corner[1] as usize, corner[2] as usize];
        if [a, b, c].iter().any(|&i| i >= positions.len()) {
            continue;
        }
        let vertex_color = colors.as_ref().filter(|per_vertex| per_vertex.len() == positions.len()).map(|per_vertex| {
            let mut mean = [0.0; 4];
            for i in [a, b, c] {
                for k in 0..4 {
                    mean[k] += per_vertex[i][k] / 3.0;
                }
            }
            mean
        });
        let color = match vertex_color {
            Some(v) => [base[0] * v[0], base[1] * v[1], base[2] * v[2], base[3] * v[3]],
            None => base,
        };
        let corner_uvs = uvs.as_ref().filter(|uv| uv.len() == positions.len()).map(|uv| [uv[a], uv[b], uv[c]]);
        model.triangles.push(Triangle {
            corners: [positions[a], positions[b], positions[c]],
            uvs: corner_uvs,
            color,
            texture: texture.filter(|_| corner_uvs.is_some()),
        });
        model.stats.triangles += 1;
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Camera {
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    pub(super) zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { yaw: 0.6, pitch: 0.35, zoom: 1.0 }
    }
}

pub(super) fn render(model: &Model, camera: &Camera, width: usize, height: usize, background: [u8; 3]) -> egui::ColorImage {
    let mut pixels = vec![egui::Color32::from_rgb(background[0], background[1], background[2]); width * height];
    let mut depth = vec![f32::INFINITY; width * height];
    let center = scale(add(model.min, model.max), 0.5);
    let radius = (dot(sub(model.max, model.min), sub(model.max, model.min)).sqrt() * 0.5).max(1e-6);
    let fov = 45f32.to_radians();
    let distance = radius / (fov * 0.5).sin() * camera.zoom;
    let direction = [camera.pitch.cos() * camera.yaw.sin(), camera.pitch.sin(), camera.pitch.cos() * camera.yaw.cos()];
    let eye = add(center, scale(direction, distance));
    let forward = normalize(sub(center, eye));
    let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    let focal = (height as f32 * 0.5) / (fov * 0.5).tan();
    let near = distance * 1e-3;
    let project = |p: Vec3| {
        let rel = sub(p, eye);
        let z = dot(rel, forward);
        [width as f32 * 0.5 + dot(rel, right) * focal / z, height as f32 * 0.5 - dot(rel, up) * focal / z, z]
    };
    for triangle in &model.triangles {
        let [a, b, c] = triangle.corners;
        let normal = normalize(cross(sub(b, a), sub(c, a)));
        let light = 0.25 + 0.75 * dot(normal, forward).abs();
        let screen = [project(a), project(b), project(c)];
        if screen.iter().any(|p| p[2] <= near) {
            continue;
        }
        fill_triangle(&mut pixels, &mut depth, width, height, &screen, |w| shade(model, triangle, w, light));
    }
    egui::ColorImage::new([width, height], pixels)
}

fn shade(model: &Model, triangle: &Triangle, weights: [f32; 3], light: f32) -> egui::Color32 {
    let mut color = triangle.color;
    if let (Some(uvs), Some(Some(texture))) = (triangle.uvs, triangle.texture.map(|i| model.textures.get(i).and_then(Option::as_ref))) {
        let uv = [0, 1].map(|k| uvs[0][k] * weights[0] + uvs[1][k] * weights[1] + uvs[2][k] * weights[2]);
        let texel = texture.sample(uv);
        color = [color[0] * texel[0], color[1] * texel[1], color[2] * texel[2], color[3] * texel[3]];
    }
    let channel = |v: f32| ((v * light).clamp(0.0, 1.0) * 255.0) as u8;
    egui::Color32::from_rgb(channel(color[0]), channel(color[1]), channel(color[2]))
}

fn fill_triangle(pixels: &mut [egui::Color32], depth: &mut [f32], width: usize, height: usize, p: &[[f32; 3]; 3], color_at: impl Fn([f32; 3]) -> egui::Color32) {
    let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
    let area = edge(p[0], p[1], p[2][0], p[2][1]);
    if area.abs() < 1e-9 {
        return;
    }
    let min_x = p.iter().map(|v| v[0]).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let max_x = p.iter().map(|v| v[0]).fold(f32::MIN, f32::max).ceil().min(width as f32 - 1.0);
    let min_y = p.iter().map(|v| v[1]).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let max_y = p.iter().map(|v| v[1]).fold(f32::MIN, f32::max).ceil().min(height as f32 - 1.0);
    if max_x < 0.0 || max_y < 0.0 {
        return;
    }
    for y in min_y..=max_y as usize {
        for x in min_x..=max_x as usize {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let w = [edge(p[1], p[2], px, py) / area, edge(p[2], p[0], px, py) / area, edge(p[0], p[1], px, py) / area];
            if w.iter().any(|&v| v < 0.0) {
                continue;
            }
            let z = w[0] * p[0][2] + w[1] * p[1][2] + w[2] * p[2][2];
            let i = y * width + x;
            if z < depth[i] {
                depth[i] = z;
                pixels[i] = color_at(w);
            }
        }
    }
}

enum Side {
    Loading(Receiver<Result<Model, String>>),
    Ready(Result<Model, String>),
}

struct Pane {
    side: Option<Side>,
    shown: Option<(Camera, [usize; 2], egui::TextureHandle)>,
}

impl Pane {
    fn load(path: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let side = path.map(|path| {
            let (tx, rx) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(load_model(&path));
                ctx.request_repaint();
            });
            Side::Loading(rx)
        });
        Pane { side, shown: None }
    }

    fn poll(&mut self) {
        if let Some(Side::Loading(rx)) = &self.side
            && let Ok(done) = rx.try_recv()
        {
            self.side = Some(Side::Ready(done));
        }
    }

    fn stats(&mut self) -> Option<Stats> {
        self.poll();
        match &self.side {
            Some(Side::Ready(Ok(model))) => Some(model.stats),
            _ => None,
        }
    }
}

pub(super) struct ModelPair {
    key: (String, u64),
    local: Pane,
    remote: Pane,
    identical: bool,
    camera: Camera,
}

impl ModelPair {
    pub(super) fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    pub(super) fn load(ctx: &egui::Context, e: &FileEntry, local_root: &str, mirror: &Path) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        let local = Path::new(local_root).join(local_path);
        let remote = mirror.join(remote_path);
        let identical = e.origin == Origin::Synced || (local.exists() && crate::engine::same_content(&local, &remote));
        ModelPair {
            key: (e.path.clone(), e.version),
            local: Pane::load(local.exists().then_some(local), ctx),
            remote: Pane::load((!identical && remote.exists()).then_some(remote), ctx),
            identical,
            camera: Camera::default(),
        }
    }
}

pub(super) fn model_view(ui: &mut egui::Ui, pair: &mut ModelPair) {
    let mut camera = pair.camera;
    if pair.identical {
        pane_ui(ui, "Identical on both sides", Color32::GRAY, &mut pair.local, &mut camera, None);
    } else {
        let other_local = pair.local.stats();
        let other_remote = pair.remote.stats();
        ui.columns(2, |columns| {
            pane_ui(&mut columns[0], "Local (yours)", YOU, &mut pair.local, &mut camera, other_remote);
            pane_ui(&mut columns[1], "Remote (Claude)", CLAUDE, &mut pair.remote, &mut camera, other_local);
        });
    }
    pair.camera = camera;
}

fn pane_ui(ui: &mut egui::Ui, title: &str, color: Color32, pane: &mut Pane, camera: &mut Camera, other: Option<Stats>) {
    pane.poll();
    let model = ui
        .horizontal(|ui| {
            ui.label(RichText::new(title).color(color).strong());
            match &pane.side {
                None => {
                    ui.weak("not present");
                    None
                }
                Some(Side::Ready(Ok(model))) => {
                    ui.weak(short_stats(&model.stats, other)).on_hover_text(stats_text(&model.stats, other));
                    Some(model)
                }
                Some(Side::Ready(Err(e))) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                    None
                }
                Some(Side::Loading(_)) => {
                    ui.spinner();
                    None
                }
            }
        })
        .inner;
    let Some(model) = model else { return };
    let room = ui.available_size() - egui::vec2(0.0, 6.0);
    let size = [room.x.max(32.0) as usize, room.y.max(32.0) as usize];
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size[0] as f32, size[1] as f32), egui::Sense::click_and_drag());
    if response.dragged() {
        let delta = response.drag_delta();
        camera.yaw -= delta.x * 0.01;
        camera.pitch = (camera.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            camera.zoom = (camera.zoom * (-scroll * 0.002).exp()).clamp(0.05, 20.0);
        }
    }
    if response.double_clicked() {
        *camera = Camera::default();
    }
    let stale = pane.shown.as_ref().is_none_or(|(c, s, _)| c != camera || *s != size);
    if stale {
        let bg = ui.visuals().extreme_bg_color;
        let image = render(model, camera, size[0], size[1], [bg.r(), bg.g(), bg.b()]);
        let texture = ui.ctx().load_texture(format!("model-{title}"), image, egui::TextureOptions::LINEAR);
        pane.shown = Some((*camera, size, texture));
    }
    if let Some((_, _, texture)) = &pane.shown {
        ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    let hint = "drag: turn · wheel: zoom · double-click: reset";
    let font = egui::FontId::proportional(11.0);
    ui.painter().text(rect.left_bottom() + egui::vec2(6.0, -4.0), egui::Align2::LEFT_BOTTOM, hint, font, ui.visuals().weak_text_color().gamma_multiply(0.7));
}

fn short_stats(stats: &Stats, other: Option<Stats>) -> String {
    let difference = match other {
        Some(o) if o.triangles != stats.triangles => format!(" ({:+})", stats.triangles as i64 - o.triangles as i64),
        _ => String::new(),
    };
    format!("{} triangles{difference}", stats.triangles)
}

fn stats_text(stats: &Stats, other: Option<Stats>) -> String {
    let change = |mine: usize, theirs: Option<usize>| match theirs {
        Some(theirs) if theirs != mine => format!(" ({:+})", mine as i64 - theirs as i64),
        _ => String::new(),
    };
    let [x, y, z] = stats.size;
    format!(
        "{} mesh(es){}\n{} vertices{}\n{} triangles{}\n{} material(s){}\nsize {x:.3} × {y:.3} × {z:.3}",
        stats.meshes,
        change(stats.meshes, other.map(|o| o.meshes)),
        stats.vertices,
        change(stats.vertices, other.map(|o| o.vertices)),
        stats.triangles,
        change(stats.triangles, other.map(|o| o.triangles)),
        stats.materials,
        change(stats.materials, other.map(|o| o.materials)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glb(quads: usize) -> Vec<u8> {
        let mut positions = Vec::new();
        let mut indices: Vec<u16> = Vec::new();
        for q in 0..quads {
            let x = q as f32 * 1.5;
            let base = (q * 4) as u16;
            for p in [[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x + 1.0, 1.0, 0.0], [x, 1.0, 0.0f32]] {
                positions.extend(p);
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let mut bin: Vec<u8> = positions.iter().flat_map(|v| v.to_le_bytes()).collect();
        let positions_len = bin.len();
        bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
        let indices_len = bin.len() - positions_len;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let max_x = (quads - 1) as f32 * 1.5 + 1.0;
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"translation":[1,0,0]}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],"materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1,0,0,1]}}}}],"buffers":[{{"byteLength":{}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{positions_len}}},{{"buffer":0,"byteOffset":{positions_len},"byteLength":{indices_len}}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":{},"type":"VEC3","min":[0,0,0],"max":[{max_x},1,0]}},{{"bufferView":1,"componentType":5123,"count":{},"type":"SCALAR"}}]}}"#,
            bin.len(),
            quads * 4,
            quads * 6
        );
        let mut json = json.into_bytes();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + bin.len();
        let mut out = Vec::new();
        out.extend(b"glTF");
        out.extend(2u32.to_le_bytes());
        out.extend((total as u32).to_le_bytes());
        out.extend((json.len() as u32).to_le_bytes());
        out.extend(b"JSON");
        out.extend(json);
        out.extend((bin.len() as u32).to_le_bytes());
        out.extend(b"BIN\0");
        out.extend(bin);
        out
    }

    fn write(dir: &Path, name: &str, quads: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, glb(quads)).unwrap();
        path
    }

    #[test]
    fn model_files_by_extension() {
        assert!(is_model("assets/chair.glb") && is_model("a.GLTF"));
        assert!(!is_model("a.obj") && !is_model("glb"));
    }

    #[test]
    fn loads_a_glb_with_its_node_transform() {
        let dir = std::env::temp_dir().join(format!("agent-diff-models-{}", std::process::id()));
        let model = load_model(&write(&dir, "quad.glb", 1)).unwrap();
        assert_eq!(model.stats, Stats { meshes: 1, vertices: 4, triangles: 2, materials: 1, size: [1.0, 1.0, 0.0] });
        assert_eq!(model.min[0], 1.0);
        assert!(load_model(&dir.join("missing.glb")).is_err());
        std::fs::write(dir.join("broken.glb"), b"glTF nonsense").unwrap();
        assert!(load_model(&dir.join("broken.glb")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn renders_the_model_in_its_color() {
        let dir = std::env::temp_dir().join(format!("agent-diff-render-{}", std::process::id()));
        let model = load_model(&write(&dir, "quad.glb", 1)).unwrap();
        let front = Camera { yaw: 0.0, pitch: 0.0, zoom: 1.0 };
        let image = render(&model, &front, 64, 48, [10, 20, 30]);
        let center = image.pixels[24 * 64 + 32];
        assert!(center.r() > 150 && center.g() < 30 && center.b() < 30, "{center:?}");
        assert_eq!(image.pixels[0], Color32::from_rgb(10, 20, 30));
        let turned = render(&model, &Camera { yaw: 1.2, ..front }, 64, 48, [10, 20, 30]);
        let red = |img: &egui::ColorImage| img.pixels.iter().filter(|p| p.r() > 100 && p.g() < 30).count();
        assert!(red(&turned) < red(&image), "turning shows the square edge-on");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn side_by_side_with_differences() {
        let root = std::env::temp_dir().join(format!("agent-diff-model-pair-{}", std::process::id()));
        write(&root.join("local/assets"), "chair.glb", 1);
        write(&root.join("mirror/assets"), "chair.glb", 3);
        let entry = FileEntry {
            path: "assets/chair.glb".into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Binary,
            version: 1,
        };
        let ctx = egui::Context::default();
        let mut pair = ModelPair::load(&ctx, &entry, root.join("local").to_str().unwrap(), &root.join("mirror"));
        let mut texts = Vec::new();
        for _ in 0..200 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| model_view(ui, &mut pair));
            out.textures_delta.clear();
            texts = out.shapes.iter().filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some(t.galley.job.text.clone()) } else { None }).collect();
            if texts.iter().filter(|t| t.contains("triangles")).count() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(texts.iter().any(|t| t.contains("2 triangles (-4)")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("6 triangles (+4)")), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Remote (Claude)"), "{texts:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore]
    fn bench_render() {
        let dir = std::env::temp_dir().join(format!("agent-diff-bench-model-{}", std::process::id()));
        let model = load_model(&write(&dir, "many.glb", 50_000)).unwrap();
        let started = Instant::now();
        let _ = render(&model, &Camera::default(), 800, 600, [0, 0, 0]);
        println!("{} triangles rendered at 800×600 in {:?}", model.stats.triangles, started.elapsed());
        let _ = std::fs::remove_dir_all(dir);
    }
}

impl Preview for ModelPair {
    fn is_for(&self, e: &FileEntry) -> bool {
        ModelPair::is_for(self, e)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        model_view(ui, self);
    }
}
