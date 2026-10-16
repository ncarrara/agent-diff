use super::*;
use std::io::Read;
use std::path::Path;

const SHOWN_BYTES: usize = 16 * 1024;
const ROW: usize = 16;

pub(super) struct HexPair {
    key: (String, u64),
    local: Option<(Vec<u8>, u64)>,
    remote: Option<(Vec<u8>, u64)>,
}

fn read_start(path: &Path) -> Option<(Vec<u8>, u64)> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut bytes = Vec::with_capacity(SHOWN_BYTES);
    file.take(SHOWN_BYTES as u64).read_to_end(&mut bytes).ok()?;
    Some((bytes, size))
}

pub(super) fn hex_row(offset: usize, bytes: &[u8]) -> String {
    let hex: Vec<String> = (0..ROW).map(|i| bytes.get(i).map_or("  ".to_string(), |b| format!("{b:02x}"))).collect();
    let ascii: String = bytes.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }).collect();
    format!("{offset:08x}  {}  {}  |{ascii}|", hex[..8].join(" "), hex[8..].join(" "))
}

impl HexPair {
    pub(super) fn load(e: &FileEntry, at: &FilePlace) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        HexPair {
            key: (e.path.clone(), e.version),
            local: read_start(&Path::new(at.local_root).join(local_path)),
            remote: if e.origin == Origin::Synced { None } else { read_start(&at.mirror.join(remote_path)) },
        }
    }
}

fn column(ui: &mut egui::Ui, title: &str, color: Color32, side: Option<&(Vec<u8>, u64)>, other: Option<&(Vec<u8>, u64)>, id: &str) {
    let Some((bytes, size)) = side else {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(color).strong());
            ui.weak("not present");
        });
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).color(color).strong());
        let shown = if *size as usize > bytes.len() { format!(" · first {} KB shown", bytes.len() / 1024) } else { String::new() };
        ui.weak(format!("{size} bytes{shown}"));
    });
    egui::ScrollArea::both().id_salt(id).auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (row, chunk) in bytes.chunks(ROW).enumerate() {
            let start = row * ROW;
            let differs = other.is_some_and(|(o, _)| o.get(start..(start + chunk.len()).min(o.len())) != Some(chunk));
            let text = RichText::new(hex_row(start, chunk)).monospace();
            ui.label(if differs { text.color(BOTH) } else { text });
        }
    });
}

impl Preview for HexPair {
    fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.remote.is_none() && self.local.is_some() {
            column(ui, "Binary file", Color32::GRAY, self.local.as_ref(), None, "hex-only");
            return;
        }
        let (local, remote) = (self.local.as_ref(), self.remote.as_ref());
        ui.columns(2, |columns| {
            column(&mut columns[0], "Local (yours)", YOU, local, remote, "hex-local");
            column(&mut columns[1], "Remote (Claude)", CLAUDE, remote, local, "hex-remote");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_hex_and_text() {
        assert_eq!(hex_row(16, b"PNG\r\n\x1a\n\0abc"), "00000010  50 4e 47 0d 0a 1a 0a 00  61 62 63                 |PNG.....abc|");
    }
}
