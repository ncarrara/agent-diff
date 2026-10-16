type Rgb = [f32; 3];

const BACKGROUND: Rgb = [0.122, 0.141, 0.188];
const YOURS: Rgb = [0.424, 0.659, 0.910];
const CLAUDES: Rgb = [0.851, 0.541, 0.357];
const LINE: Rgb = [0.96, 0.97, 0.99];
const ADDED: Rgb = [0.30, 0.75, 0.42];
const REMOVED: Rgb = [0.91, 0.35, 0.35];

fn rounded_box(p: [f32; 2], center: [f32; 2], half: [f32; 2], radius: f32) -> f32 {
    let q = [(p[0] - center[0]).abs() - half[0] + radius, (p[1] - center[1]).abs() - half[1] + radius];
    let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
    outside + q[0].max(q[1]).min(0.0) - radius
}

fn segment(p: [f32; 2], a: [f32; 2], b: [f32; 2], width: f32) -> f32 {
    let (pa, ba) = ([p[0] - a[0], p[1] - a[1]], [b[0] - a[0], b[1] - a[1]]);
    let t = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1])).clamp(0.0, 1.0);
    let (dx, dy) = (pa[0] - ba[0] * t, pa[1] - ba[1] * t);
    (dx * dx + dy * dy).sqrt() - width * 0.5
}

fn circle(p: [f32; 2], center: [f32; 2], radius: f32) -> f32 {
    ((p[0] - center[0]).powi(2) + (p[1] - center[1]).powi(2)).sqrt() - radius
}

struct Canvas {
    size: f32,
    pixel: [f32; 2],
    color: Rgb,
    alpha: f32,
}

impl Canvas {
    fn paint(&mut self, distance: f32, color: Rgb, opacity: f32) {
        let coverage = (0.5 - distance * self.size).clamp(0.0, 1.0) * opacity;
        if coverage <= 0.0 {
            return;
        }
        for k in 0..3 {
            self.color[k] = color[k] * coverage + self.color[k] * (1.0 - coverage);
        }
        self.alpha = coverage + self.alpha * (1.0 - coverage);
    }

    fn card(&mut self, left: f32, color: Rgb, changed_line: usize, change: Rgb) {
        let p = self.pixel;
        self.paint(rounded_box(p, [left + 0.17, 0.5], [0.17, 0.29], 0.05), color, 1.0);
        let lengths = [0.20, 0.13, 0.22, 0.16, 0.10];
        for (i, length) in lengths.iter().enumerate() {
            let y = 0.31 + i as f32 * 0.095;
            let (start, end) = ([left + 0.06, y], [left + 0.06 + length, y]);
            if i == changed_line {
                self.paint(rounded_box(p, [left + 0.17, y], [0.135, 0.035], 0.015), change, 0.9);
            }
            self.paint(segment(p, start, end, 0.035), LINE, 0.92);
        }
    }

    fn badge(&mut self) {
        let p = self.pixel;
        self.paint(circle(p, [0.5, 0.5], 0.135), BACKGROUND, 1.0);
        self.paint(circle(p, [0.5, 0.5], 0.135).abs() - 0.012, LINE, 1.0);
        let (top, bottom, w) = (0.465, 0.535, 0.03);
        self.paint(segment(p, [0.43, top], [0.57, top], w), LINE, 1.0);
        self.paint(segment(p, [0.57, top], [0.535, top - 0.035], w), LINE, 1.0);
        self.paint(segment(p, [0.57, top], [0.535, top + 0.035], w), LINE, 1.0);
        self.paint(segment(p, [0.43, bottom], [0.57, bottom], w), LINE, 1.0);
        self.paint(segment(p, [0.43, bottom], [0.465, bottom - 0.035], w), LINE, 1.0);
        self.paint(segment(p, [0.43, bottom], [0.465, bottom + 0.035], w), LINE, 1.0);
    }
}

pub fn icon_rgba(size: u32) -> Vec<u8> {
    let scale = size as f32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let pixel = [(x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale];
            let mut canvas = Canvas { size: scale, pixel, color: [0.0; 3], alpha: 0.0 };
            canvas.paint(rounded_box(pixel, [0.5, 0.5], [0.47, 0.47], 0.11), BACKGROUND, 1.0);
            canvas.card(0.11, YOURS, 1, REMOVED);
            canvas.card(0.55, CLAUDES, 3, ADDED);
            canvas.badge();
            let alpha = canvas.alpha.max(1e-6);
            rgba.extend(canvas.color.map(|c| ((c / alpha).clamp(0.0, 1.0) * 255.0) as u8));
            rgba.push((canvas.alpha * 255.0) as u8);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_the_right_size_and_transparent_corners() {
        let rgba = icon_rgba(64);
        assert_eq!(rgba.len(), 64 * 64 * 4);
        assert_eq!(rgba[3], 0);
        let center = (32 * 64 + 32) * 4;
        assert_eq!(rgba[center + 3], 255);
    }

    #[test]
    #[ignore]
    #[cfg(feature = "images")]
    fn export_icon() {
        let size = 256;
        let image = image::RgbaImage::from_raw(size, size, icon_rgba(size)).unwrap();
        image.save(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/agent-diff.png")).unwrap();
    }
}
