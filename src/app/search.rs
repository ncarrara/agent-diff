use super::*;
use std::ops::Range;

pub(super) const QUERY_ID: &str = "content-search-query";
pub(super) const JUMP_ID: &str = "content-search-jump";
pub(super) const JUMP_FRAMES: u8 = 4;
pub(super) const CURRENT_LINE_ID: &str = "content-search-current-line";
pub(super) const HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(0x90, 0x78, 0x00, 0x90);
pub(super) const CURRENT: Color32 = Color32::from_rgb(0xe8, 0x70, 0x10);
const MIN_QUERY: usize = 2;
const MAX_FILE: u64 = 2 * 1024 * 1024;
const TYPING_PAUSE: Duration = Duration::from_millis(500);

pub(super) fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.to_ascii_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let lower = text.to_ascii_lowercase();
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&query) {
        let start = from + at;
        ranges.push(start..start + query.len());
        from = start + query.len();
    }
    ranges
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Hit {
    pub(super) path: String,
    pub(super) count: usize,
    pub(super) lines: Vec<usize>,
}

impl Hit {
    pub(super) fn first_line(&self) -> usize {
        self.lines[0]
    }
}

pub(super) fn search_text(path: &str, text: &str, query: &str) -> Option<Hit> {
    let mut count = 0;
    let mut lines = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let found = match_ranges(line, query).len();
        if found > 0 {
            count += found;
            lines.push(number);
        }
    }
    (!lines.is_empty()).then(|| Hit { path: path.to_string(), count, lines })
}

fn search_file(path: &str, file: &Path, query: &str) -> Option<Hit> {
    if std::fs::metadata(file).ok()?.len() > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(file).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    search_text(path, &String::from_utf8_lossy(&bytes), query)
}

pub(super) fn with_highlights(job: egui::text::LayoutJob, ranges: &[Range<usize>], background: Color32) -> egui::text::LayoutJob {
    if ranges.is_empty() {
        return job;
    }
    let mut out = egui::text::LayoutJob { text: job.text.clone(), sections: Vec::new(), ..job.clone() };
    for section in &job.sections {
        let (start, end) = (section.byte_range.start.0, section.byte_range.end.0);
        let mut cuts = vec![start, end];
        for range in ranges {
            for edge in [range.start, range.end] {
                if (start..end).contains(&edge) {
                    cuts.push(edge);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for (i, piece) in cuts.windows(2).enumerate() {
            let mut format = section.format.clone();
            if ranges.iter().any(|r| r.start <= piece[0] && piece[1] <= r.end) {
                format.background = background;
            }
            let leading_space = if i == 0 { section.leading_space } else { 0.0 };
            let byte_range = egui::text::ByteIndex(piece[0])..egui::text::ByteIndex(piece[1]);
            out.sections.push(egui::text::LayoutSection { leading_space, byte_range, format });
        }
    }
    out
}

pub(super) fn current_query(ctx: &egui::Context) -> Option<Arc<str>> {
    ctx.data(|d| d.get_temp::<Arc<str>>(egui::Id::new(QUERY_ID))).filter(|q| !q.is_empty())
}

pub(super) fn current_line(ctx: &egui::Context) -> Option<usize> {
    ctx.data(|d| d.get_temp::<Option<usize>>(egui::Id::new(CURRENT_LINE_ID))).flatten()
}

pub(super) fn highlight_text(job: egui::text::LayoutJob, from: usize, query: &str, current: Option<Range<usize>>) -> egui::text::LayoutJob {
    let found: Vec<Range<usize>> = match_ranges(&job.text[from..], query).into_iter().map(|r| r.start + from..r.end + from).collect();
    let in_current: Vec<Range<usize>> = found.iter().filter(|r| current.as_ref().is_some_and(|c| c.start <= r.start && r.end <= c.end)).cloned().collect();
    with_highlights(with_highlights(job, &found, HIGHLIGHT), &in_current, CURRENT)
}

pub(super) fn line_bytes(text: &str, line: usize) -> Option<Range<usize>> {
    let mut start = 0;
    for (number, piece) in text.split_inclusive('\n').enumerate() {
        if number == line {
            return Some(start..start + piece.len());
        }
        start += piece.len();
    }
    None
}

#[derive(Default)]
pub(super) struct ContentSearch {
    pub(super) query: String,
    searched: String,
    searched_scope: u64,
    typed_at: Option<Instant>,
    job: Option<std::sync::mpsc::Receiver<(String, u64, Vec<Hit>)>>,
    pub(super) results: Option<Arc<Vec<Hit>>>,
    at: Option<(String, usize)>,
    shown: Option<(String, usize)>,
}

impl ContentSearch {
    pub(super) fn shown_query(&self) -> Option<&str> {
        self.results.as_ref().map(|_| self.searched.as_str()).filter(|q| !q.is_empty())
    }

    pub(super) fn active_query(&self) -> Option<&str> {
        Some(self.query.trim()).filter(|q| q.len() >= MIN_QUERY)
    }

    pub(super) fn typed(&mut self) {
        self.typed_at = Some(Instant::now());
        self.at = None;
        self.shown = None;
    }

    pub(super) fn target(&mut self, path: &str) -> Option<usize> {
        let hit = self.hit(path)?;
        let index = match &self.at {
            Some((at, index)) if at == path => (*index).min(hit.lines.len() - 1),
            _ => 0,
        };
        let line = hit.lines[index];
        self.at = Some((path.to_string(), index));
        if self.shown == self.at {
            return None;
        }
        self.shown = self.at.clone();
        Some(line)
    }

    pub(super) fn current_line(&self, path: Option<&str>) -> Option<usize> {
        let (at, index) = self.at.as_ref()?;
        let hit = self.hit(path.filter(|p| p == at)?)?;
        hit.lines.get(*index).copied()
    }

    pub(super) fn position(&self, path: Option<&str>) -> Option<(usize, usize)> {
        let path = path?;
        let hit = self.hit(path)?;
        let index = self.at.as_ref().filter(|(at, _)| at == path).map_or(0, |(_, i)| *i);
        Some((index + 1, hit.lines.len()))
    }

    pub(super) fn step(&mut self, current: Option<&str>, forward: bool) -> Option<String> {
        let results = self.results.clone()?;
        let file = current.and_then(|path| results.iter().position(|h| h.path == path));
        let Some(file) = file else {
            let first = if forward { results.first() } else { results.last() }?;
            self.at = Some((first.path.clone(), if forward { 0 } else { first.lines.len() - 1 }));
            return Some(first.path.clone());
        };
        let hit = &results[file];
        let index = self.at.as_ref().filter(|(at, _)| *at == hit.path).map_or(0, |(_, i)| *i);
        match (forward, index) {
            (true, i) if i + 1 < hit.lines.len() => self.at = Some((hit.path.clone(), i + 1)),
            (false, i) if i > 0 => self.at = Some((hit.path.clone(), i - 1)),
            _ => {
                let next = if forward { (file + 1) % results.len() } else { (file + results.len() - 1) % results.len() };
                let next_hit = &results[next];
                self.at = Some((next_hit.path.clone(), if forward { 0 } else { next_hit.lines.len() - 1 }));
                return Some(next_hit.path.clone()).filter(|p| p != &hit.path);
            }
        }
        None
    }

    pub(super) fn searching(&self) -> bool {
        self.job.is_some() || self.active_query().is_some_and(|q| q != self.searched)
    }

    pub(super) fn hit(&self, path: &str) -> Option<&Hit> {
        self.results.as_ref()?.iter().find(|h| h.path == path)
    }

    pub(super) fn update(&mut self, ctx: &egui::Context, scope: u64, files: impl FnOnce() -> Vec<(String, PathBuf)>) {
        if let Some(rx) = &self.job
            && let Ok((query, searched_scope, hits)) = rx.try_recv()
        {
            self.job = None;
            if Some(query.as_str()) == self.active_query() {
                self.results = Some(Arc::new(hits));
                self.searched = query;
                self.searched_scope = searched_scope;
            }
        }
        let Some(query) = self.active_query().map(str::to_string) else {
            self.results = None;
            self.searched.clear();
            return;
        };
        if (query == self.searched && scope == self.searched_scope) || self.job.is_some() {
            return;
        }
        let wait = TYPING_PAUSE.saturating_sub(self.typed_at.map_or(TYPING_PAUSE, |t| t.elapsed()));
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (files, ctx) = (files(), ctx.clone());
        std::thread::spawn(move || {
            let mut hits: Vec<Hit> = files.iter().filter_map(|(path, file)| search_file(path, file, &query)).collect();
            hits.sort_by(|a, b| a.path.cmp(&b.path));
            let _ = tx.send((query, scope, hits));
            ctx.request_repaint();
        });
        self.job = Some(rx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ignore_case() {
        assert_eq!(match_ranges("Foo foo FOO bar", "foo"), [0..3, 4..7, 8..11]);
        assert!(match_ranges("abc", "").is_empty());
        assert_eq!(search_text("a.rs", "one\ntwo Two\nthree two", "two"), Some(Hit { path: "a.rs".into(), count: 3, lines: vec![1, 2] }));
        assert_eq!(search_text("a.rs", "nothing here", "two"), None);
    }

    #[test]
    fn next_and_previous_go_through_lines_then_files() {
        let hit = |path: &str, lines: Vec<usize>| Hit { path: path.into(), count: lines.len(), lines };
        let mut search = ContentSearch { query: "x".into(), results: Some(Arc::new(vec![hit("a", vec![3, 9]), hit("b", vec![5])])), ..ContentSearch::default() };
        assert_eq!(search.target("a"), Some(3));
        assert_eq!(search.target("a"), None);
        assert_eq!(search.position(Some("a")), Some((1, 2)));
        assert_eq!(search.step(Some("a"), true), None);
        assert_eq!(search.target("a"), Some(9));
        assert_eq!(search.step(Some("a"), true), Some("b".into()));
        assert_eq!(search.target("b"), Some(5));
        assert_eq!(search.step(Some("b"), true), Some("a".into()));
        assert_eq!(search.target("a"), Some(3));
        assert_eq!(search.step(Some("a"), false), Some("b".into()));
        assert_eq!(search.target("b"), Some(5));
        assert_eq!(search.step(Some("b"), false), Some("a".into()));
        assert_eq!(search.target("a"), Some(9));
        assert_eq!(search.position(Some("a")), Some((2, 2)));
        assert_eq!(search.step(None, true), Some("a".into()));
        let mut single = ContentSearch { results: Some(Arc::new(vec![hit("only", vec![1, 4])])), ..ContentSearch::default() };
        single.target("only");
        single.step(Some("only"), true);
        assert_eq!(single.target("only"), Some(4));
        assert_eq!(single.step(Some("only"), true), None);
        assert_eq!(single.target("only"), Some(1));
    }

    #[test]
    fn the_current_line_stands_out() {
        let mut job = egui::text::LayoutJob::default();
        job.append("  1   ab ab\n  2   ab", 0.0, egui::TextFormat::default());
        let line_two = line_bytes("  1   ab ab\n  2   ab", 1);
        assert_eq!(line_two, Some(12..20));
        let job = highlight_text(job, 6, "AB", line_two);
        let colors: Vec<(String, Color32)> = job.sections.iter().map(|s| (job.text[s.byte_range.start.0..s.byte_range.end.0].to_string(), s.format.background)).filter(|(_, c)| *c != Color32::TRANSPARENT).collect();
        assert_eq!(colors.len(), 3, "{colors:?}");
        assert_eq!(colors[..2].iter().map(|(_, c)| *c).collect::<Vec<_>>(), [HIGHLIGHT, HIGHLIGHT]);
        assert_eq!(colors[2].1, CURRENT);
        assert_eq!(line_bytes("a\nbc\nd", 1), Some(2..5));
        assert_eq!(line_bytes("a", 3), None);
    }

    #[test]
    fn highlights_split_the_text_sections() {
        let mut job = egui::text::LayoutJob::default();
        job.append("let foo", 0.0, egui::TextFormat { color: Color32::RED, ..Default::default() });
        job.append(" = foo;", 0.0, egui::TextFormat { color: Color32::BLUE, ..Default::default() });
        let job = with_highlights(job, &match_ranges("let foo = foo;", "foo"), Color32::YELLOW);
        let pieces: Vec<(&str, bool)> = job.sections.iter().map(|s| (&job.text[s.byte_range.start.0..s.byte_range.end.0], s.format.background == Color32::YELLOW)).collect();
        assert_eq!(pieces, [("let ", false), ("foo", true), (" = ", false), ("foo", true), (";", false)]);
    }

    #[test]
    fn searches_files_in_the_background() {
        let dir = std::env::temp_dir().join(format!("agent-diff-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}\nlet needle = 1;\n").unwrap();
        std::fs::write(dir.join("b.rs"), "nothing\n").unwrap();
        std::fs::write(dir.join("c.bin"), b"needle\0binary").unwrap();
        let files = vec![("a.rs".to_string(), dir.join("a.rs")), ("b.rs".into(), dir.join("b.rs")), ("c.bin".into(), dir.join("c.bin"))];
        let ctx = egui::Context::default();
        let mut search = ContentSearch { query: "NEEDLE".into(), ..ContentSearch::default() };
        search.update(&ctx, 0, || files.clone());
        for _ in 0..200 {
            search.update(&ctx, 0, || files.clone());
            if search.results.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(search.results.as_deref().unwrap(), &[Hit { path: "a.rs".into(), count: 1, lines: vec![1] }]);
        assert!(!search.searching());
        assert_eq!(search.shown_query(), Some("NEEDLE"));
        search.query = "needles".into();
        search.typed();
        search.update(&ctx, 0, || files.clone());
        assert_eq!(search.shown_query(), Some("NEEDLE"));
        assert!(search.searching());
        search.query = "x".into();
        search.update(&ctx, 0, || files.clone());
        assert!(search.results.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
