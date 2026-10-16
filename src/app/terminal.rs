//! A terminal running Claude in the remote workspace: a pty (ssh for a remote
//! host) whose output is interpreted by a vt100 parser and drawn cell by cell.

use super::*;
use egui::{Key, Modifiers, Sense, Stroke, vec2};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The program started in the remote workspace. When it exits, a login shell
/// takes over so the terminal stays usable.
const PROGRAM: &str = "claude";
const SCROLLBACK: usize = 10_000;

/// Replies to the queries programs send to the terminal (cursor position,
/// device attributes), which the vt100 parser leaves to us.
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.0.extend(format!("\x1b[{};{}R", row + 1, col + 1).bytes());
            }
            (None, 'n', 5) => self.0.extend(b"\x1b[0n"),
            (None, 'c', 0) => self.0.extend(b"\x1b[?62;22c"),
            (Some(b'>'), 'c', 0) => self.0.extend(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }
}

pub(super) struct Terminal {
    parser: Arc<Mutex<vt100::Parser<Replies>>>,
    writer: Sender<Vec<u8>>,
    drawn_at_frame: Arc<AtomicU64>,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
    size: (u16, u16),
    /// Fraction of a line scrolled by the mouse wheel, not applied yet.
    scroll_rest: f32,
    /// Text to paste once the program accepts pastes.
    pending_paste: Option<String>,
    selection: Option<Selection>,
    key_anchor: Option<(u16, u16)>,
    last_command: Arc<Mutex<CommandCapture>>,
    tab_steps: Vec<isize>,
    sized: Option<Sender<()>>,
}


const CAPTURE_LIMIT: usize = 4 * 1024 * 1024;

const PROMPT_MARK: &[u8] = b"\x1b]7;";
const QUIET_AFTER_DONE: Duration = Duration::from_secs(3);
const ANSWER_WORTH_NOTIFYING: Duration = Duration::from_secs(20);
const COMMAND_WORTH_NOTIFYING: Duration = Duration::from_secs(10);

fn worth_notifying(kind: TermKind) -> Duration {
    match kind {
        TermKind::Claude => ANSWER_WORTH_NOTIFYING,
        TermKind::Remote | TermKind::Local => COMMAND_WORTH_NOTIFYING,
    }
}

#[derive(Default)]
struct CommandCapture {
    recording: bool,
    bytes: Vec<u8>,
    started: Option<Instant>,
    last_output: Option<Instant>,
    marks_prompts: bool,
    finished: Option<Duration>,
}

impl CommandCapture {
    fn start(&mut self) {
        self.recording = true;
        self.bytes.clear();
        self.started = Some(Instant::now());
        self.finished = None;
    }

    fn watch(&mut self, output: &[u8]) {
        self.last_output = Some(Instant::now());
        if output.windows(PROMPT_MARK.len()).any(|w| w == PROMPT_MARK) {
            self.marks_prompts = true;
            if let Some(started) = self.started.take() {
                self.finished = Some(started.elapsed());
            }
        }
    }

    fn take_finished(&mut self) -> Option<Duration> {
        if let Some(took) = self.finished.take() {
            return Some(took);
        }
        let (started, last_output) = (self.started?, self.last_output?);
        let quiet_since_answer = !self.marks_prompts && last_output > started && last_output.elapsed() >= QUIET_AFTER_DONE;
        quiet_since_answer.then(|| {
            self.started = None;
            last_output - started
        })
    }

    fn waiting(&self) -> bool {
        self.started.is_some()
    }

    fn record(&mut self, output: &[u8]) {
        self.watch(output);
        if !self.recording {
            return;
        }
        self.bytes.extend_from_slice(output);
        if self.bytes.len() > 2 * CAPTURE_LIMIT {
            let excess = self.bytes.len() - CAPTURE_LIMIT;
            self.bytes.drain(..excess);
        }
    }
}

fn writer_thread(mut pty: Box<dyn Write + Send>) -> Sender<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for bytes in rx {
            if pty.write_all(&bytes).is_err() {
                break;
            }
        }
    });
    tx
}

fn root_frame(ctx: &egui::Context) -> u64 {
    ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT)
}

fn drawn_recently(ctx: &egui::Context, drawn_at_frame: &AtomicU64) -> bool {
    drawn_at_frame.load(Ordering::Relaxed) + 2 >= root_frame(ctx)
}

const SAVED_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

struct SavedOutput {
    path: PathBuf,
    file: std::fs::File,
    len: usize,
}

impl SavedOutput {
    fn open(path: PathBuf) -> std::io::Result<(Self, Vec<u8>)> {
        let previous = recent_lines(&std::fs::read(&path).unwrap_or_default(), SAVED_OUTPUT_LIMIT).to_vec();
        private_file(&path)?.write_all(&previous)?;
        let file = std::fs::OpenOptions::new().append(true).open(&path)?;
        Ok((SavedOutput { path, file, len: previous.len() }, previous))
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.file.write_all(bytes).is_err() {
            return;
        }
        self.len += bytes.len();
        if self.len > 2 * SAVED_OUTPUT_LIMIT {
            self.trim();
        }
    }

    fn trim(&mut self) {
        let Ok(all) = std::fs::read(&self.path) else { return };
        let kept = recent_lines(&all, SAVED_OUTPUT_LIMIT);
        if private_file(&self.path).and_then(|mut f| f.write_all(kept)).is_ok() {
            self.len = kept.len();
        }
    }

    fn restore(&mut self, parser: &mut vt100::Parser<Replies>, previous: &[u8]) {
        parser.process(previous);
        parser.process(RESET_MODES.as_bytes());
        parser.callbacks_mut().0.clear();
        let separator = restored_separator(parser.screen());
        parser.process(&separator);
        self.append(RESET_MODES.as_bytes());
        self.append(&separator);
    }
}

const RESET_MODES: &str = "\x1b[?1049l\x1b[0m\x1b[?25h\x1b[?1l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l";

fn rows_with_content(screen: &vt100::Screen) -> u16 {
    let columns = screen.size().1;
    screen.rows(0, columns).enumerate().filter(|(_, row)| !row.trim().is_empty()).map(|(i, _)| i as u16 + 1).last().unwrap_or(0)
}

fn restored_separator(screen: &vt100::Screen) -> Vec<u8> {
    let rows = screen.size().0;
    let below_content = match rows_with_content(screen) {
        used if used < rows => format!("\x1b[{};1H\x1b[J", used + 1),
        _ => format!("\x1b[{rows};1H\r\n"),
    };
    format!("{below_content}\x1b[2m── restored from the previous session ──\x1b[0m\r\n").into_bytes()
}

fn recent_lines(bytes: &[u8], limit: usize) -> &[u8] {
    if bytes.len() <= limit {
        return bytes;
    }
    let tail = &bytes[bytes.len() - limit..];
    tail.iter().position(|&b| b == b'\n').map_or(tail, |i| &tail[i + 1..])
}

fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

pub(super) fn saved_dir(id: &str) -> PathBuf {
    crate::engine::app_dir().join("terminals").join(id)
}

fn saved_shell_dir(tab: &TermTab) -> Option<PathBuf> {
    match tab.kind {
        TermKind::Claude => None,
        TermKind::Remote | TermKind::Local => Some(saved_dir(&tab.id)),
    }
}

fn last_cwd(tab: &TermTab) -> Option<String> {
    let text = std::fs::read_to_string(saved_shell_dir(tab)?.join("cwd")).ok()?;
    let dir = text.trim_end_matches('\n');
    dir.starts_with('/').then(|| dir.to_string())
}

const OSC_CWD: &[u8] = b"\x1b]7;";
const REPORT_CWD: &str = r#"printf '\033]7;file://%s\007' "$PWD""#;

struct CwdTracker {
    path: PathBuf,
    pending: Vec<u8>,
    last: Option<String>,
}

impl CwdTracker {
    fn new(path: PathBuf) -> Self {
        CwdTracker { path, pending: Vec::new(), last: None }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let Some(dir) = take_reported_cwd(&mut self.pending) else { return };
        if self.last.as_ref() != Some(&dir) && private_file(&self.path).and_then(|mut f| f.write_all(dir.as_bytes())).is_ok() {
            self.last = Some(dir);
        }
    }
}

fn take_reported_cwd(pending: &mut Vec<u8>) -> Option<String> {
    let mut latest = None;
    loop {
        let Some(start) = find(pending, OSC_CWD) else {
            let keep = pending.len().saturating_sub(OSC_CWD.len() - 1);
            pending.drain(..keep);
            return latest;
        };
        let body = start + OSC_CWD.len();
        match pending[body..].iter().position(|&b| b == 0x07 || b == 0x1b) {
            Some(end) => {
                latest = cwd_of_report(&pending[body..body + end]).or(latest);
                pending.drain(..body + end);
            }
            None => {
                pending.drain(..start);
                if pending.len() > 4096 {
                    pending.clear();
                }
                return latest;
            }
        }
    }
}

fn cwd_of_report(url: &[u8]) -> Option<String> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&b| b == b'/')?..];
    Some(String::from_utf8_lossy(&percent_decoded(path)).into_owned())
}

fn percent_decoded(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(high), Some(low)) => {
                out.push(high << 4 | low);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

fn forget_saved(tab: &TermTab) {
    let _ = std::fs::remove_dir_all(saved_dir(&tab.id));
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn command_output(raw: &[u8]) -> &[u8] {
    const OUTPUT_START: &[u8] = b"\x1b]133;C";
    const COMMAND_END: &[u8] = b"\x1b]133;D";
    const PROMPT_START: &[u8] = b"\x1b]133;A";
    if let Some(at) = find(raw, OUTPUT_START) {
        let after = &raw[at..];
        let body = &after[after.iter().position(|&b| b == 0x07 || b == b'\\').map_or(after.len(), |end| end + 1)..];
        let end = [COMMAND_END, PROMPT_START].iter().filter_map(|marker| find(body, marker)).min().unwrap_or(body.len());
        return &body[..end];
    }
    let start = raw.iter().position(|&b| b == b'\n').map_or(raw.len(), |i| i + 1);
    let end = raw.iter().rposition(|&b| b == b'\n').map_or(start, |i| i + 1).max(start);
    &raw[start..end]
}

fn rendered_text(bytes: &[u8], cols: u16) -> String {
    let lines = bytes.iter().filter(|&&b| b == b'\n').count();
    let rows = (lines + 2).min(u16::MAX as usize) as u16;
    let mut parser = vt100::Parser::new(rows, cols.max(10), 0);
    parser.process(bytes);
    let text = parser.screen().contents();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let last = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    lines[..last].join("\n")
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
}

fn linear((row, col): (u16, u16), cols: u16) -> usize {
    row as usize * cols as usize + col as usize
}

fn cell_before((row, col): (u16, u16), cols: u16) -> (u16, u16) {
    if col > 0 { (row, col - 1) } else { (row.saturating_sub(1), cols.saturating_sub(1)) }
}

fn between_gaps(anchor: (u16, u16), cursor: (u16, u16), cols: u16) -> Option<Selection> {
    let (start, end) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
    (start != end).then(|| Selection { anchor: start, head: cell_before(end, cols) })
}

fn erase_bytes(anchor: (u16, u16), cursor: (u16, u16), cols: u16) -> Vec<u8> {
    let (a, c) = (linear(anchor, cols), linear(cursor, cols));
    if c > a { vec![0x7f; c - a] } else { b"\x1b[3~".repeat(a - c) }
}

fn selection_move(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    if !m.shift || m.alt {
        return None;
    }
    match key {
        Key::ArrowLeft if m.ctrl => Some(b"\x1bb".to_vec()),
        Key::ArrowRight if m.ctrl => Some(b"\x1bf".to_vec()),
        Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End => key_bytes(key, Modifiers::NONE, app_cursor),
        _ => None,
    }
}

impl Selection {
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    fn columns_on(&self, row: u16, cols: u16) -> Option<(u16, u16)> {
        let ((r0, c0), (r1, c1)) = self.ordered();
        if row < r0 || row > r1 {
            return None;
        }
        let first = if row == r0 { c0 } else { 0 };
        let last = if row == r1 { c1 } else { cols.saturating_sub(1) };
        Some((first, last))
    }
}

fn primary_selection() -> Option<std::sync::MutexGuard<'static, arboard::Clipboard>> {
    static CLIPBOARD: std::sync::OnceLock<Option<Mutex<arboard::Clipboard>>> = std::sync::OnceLock::new();
    CLIPBOARD.get_or_init(|| arboard::Clipboard::new().ok().map(Mutex::new)).as_ref()?.lock().ok()
}

#[cfg(target_os = "linux")]
pub(super) fn primary_text() -> Option<String> {
    use arboard::{GetExtLinux, LinuxClipboardKind};
    primary_selection()?.get().clipboard(LinuxClipboardKind::Primary).text().ok()
}

#[cfg(target_os = "linux")]
pub(super) fn set_primary_text(text: String) {
    use arboard::{LinuxClipboardKind, SetExtLinux};
    if let Some(mut clipboard) = primary_selection() {
        let _ = clipboard.set().clipboard(LinuxClipboardKind::Primary).text(text);
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn primary_text() -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
pub(super) fn set_primary_text(_text: String) {}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        [b"\x1b[200~", text.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

fn word_at(line: &[char], col: usize) -> (usize, usize) {
    let is_word = |c: char| !c.is_whitespace();
    if !line.get(col).copied().is_some_and(is_word) {
        return (col, col);
    }
    let start = (0..col).rev().take_while(|&i| is_word(line[i])).last().unwrap_or(col);
    let end = (col..line.len()).take_while(|&i| is_word(line[i])).last().unwrap_or(col);
    (start, end)
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            std::thread::spawn(move || child.wait());
        }
    }
}

pub(super) use crate::remote::shell_quote as quote;

/// A new random Claude conversation ID (a version 4 UUID).
pub(super) fn new_session_id() -> String {
    use std::hash::BuildHasher;
    let random = |i: u8| std::collections::hash_map::RandomState::new().hash_one((i, SystemTime::now()));
    let a = random(0) & !0xf000 | 0x4000;
    let b = random(1) & !(0xc000 << 48) | 0x8000 << 48;
    format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", a >> 32, a >> 16 & 0xffff, a & 0xffff, b >> 48, b & 0xffff_ffff_ffff)
}

const FORKED_CONVERSATION_MISSING: &str = "agent-diff: the conversation to fork is not saved on the remote yet (nothing sent in it?), so this tab starts a new one";

fn conversation_saved(session: &str) -> String {
    format!("ls \"${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}\"/projects/*/{session}.jsonl >/dev/null 2>&1")
}

pub(super) fn is_session_id(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// The command opening Claude in the remote workspace on conversation
/// `session`: resumed, or created under that ID the first time. An
/// interactive login shell runs it, so the PATH set up in the user's shell
/// files applies.
pub(super) fn command(remote: &str, session: &str, forked_from: Option<&str>) -> anyhow::Result<CommandBuilder> {
    anyhow::ensure!(is_session_id(session), "invalid Claude session ID {session:?}");
    let create = match forked_from {
        Some(parent) => {
            anyhow::ensure!(is_session_id(parent), "invalid Claude session ID {parent:?}");
            format!(
                "if {}; then {PROGRAM} --resume {parent} --fork-session --session-id {session}; else echo '{FORKED_CONVERSATION_MISSING}'; {PROGRAM} --session-id {session}; fi",
                conversation_saved(parent)
            )
        }
        None => format!("{PROGRAM} --session-id {session}"),
    };
    let script = format!("if {}; then {PROGRAM} --resume {session}; else {create}; fi; exec \"${{SHELL:-/bin/sh}}\" -li", conversation_saved(session));
    in_remote(remote, Some(&script), None)
}

/// The command of a terminal tab: Claude, or a shell in one of the workspaces.
pub(super) fn tab_command(tab: &TermTab, forked_from: Option<&str>, local: &str, remote: &str) -> anyhow::Result<CommandBuilder> {
    match tab.kind {
        TermKind::Claude => command(remote, &tab.id, forked_from),
        TermKind::Remote => in_remote(remote, None, last_cwd(tab).as_deref()),
        TermKind::Local => {
            let mut cmd = CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
            cmd.arg("-li");
            cmd.cwd(existing_dir(last_cwd(tab)).unwrap_or_else(|| local.into()));
            cmd.env("PROMPT_COMMAND", REPORT_CWD);
            let dir = saved_dir(&tab.id);
            let _ = std::fs::create_dir_all(&dir);
            cmd.env("HISTFILE", dir.join("history"));
            Ok(with_term(cmd))
        }
    }
}

fn existing_dir(dir: Option<String>) -> Option<PathBuf> {
    dir.map(PathBuf::from).filter(|d| d.is_dir())
}

/// The user's interactive login shell in the remote workspace, running
/// `script` if given.
fn in_remote(remote: &str, script: Option<&str>, last_dir: Option<&str>) -> anyhow::Result<CommandBuilder> {
    let shell = match script {
        Some(script) => format!("exec \"${{SHELL:-/bin/sh}}\" -lic {}", quote(script)),
        None => format!("PROMPT_COMMAND={} exec \"${{SHELL:-/bin/sh}}\" -li", quote(REPORT_CWD)),
    };
    let cmd = match Remote::parse(remote)? {
        Remote::Ssh { host, path } => {
            let mut cmd = CommandBuilder::new("ssh");
            cmd.arg("-t");
            cmd.args(crate::remote::SHARED_CONNECTION);
            cmd.arg(host);
            let back = last_dir.map(|d| format!("cd {} 2>/dev/null || ", quote(d))).unwrap_or_default();
            cmd.arg(format!("{back}cd {} && {shell}", quote(&path)));
            if let Some(home) = dirs::home_dir() {
                cmd.cwd(home);
            }
            cmd
        }
        Remote::Local(dir) => {
            let mut cmd = CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
            cmd.arg(if script.is_some() { "-lic" } else { "-li" });
            match script {
                Some(script) => {
                    cmd.arg(script);
                    cmd.cwd(dir);
                }
                None => {
                    cmd.env("PROMPT_COMMAND", REPORT_CWD);
                    cmd.cwd(existing_dir(last_dir.map(String::from)).unwrap_or(dir));
                }
            }
            cmd
        }
    };
    Ok(with_term(cmd))
}

fn with_term(mut cmd: CommandBuilder) -> CommandBuilder {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd
}

impl Terminal {
    #[cfg(test)]
    pub(super) fn start(cmd: CommandBuilder, ctx: &egui::Context) -> anyhow::Result<Self> {
        Self::start_saved(cmd, ctx, None)
    }

    pub(super) fn start_saved(cmd: CommandBuilder, ctx: &egui::Context, saved: Option<PathBuf>) -> anyhow::Result<Self> {
        let size = (24, 80);
        let pair = native_pty_system().openpty(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 })?;
        let child = pair.slave.spawn_command(cmd)?;
        // Without the parent's copy of the slave, reads end when the child exits.
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = writer_thread(pair.master.take_writer()?);
        let drawn_at_frame = Arc::new(AtomicU64::new(0));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(size.0, size.1, SCROLLBACK, Replies::default())));
        let exited = Arc::new(AtomicBool::new(false));

        let last_command = Arc::new(Mutex::new(CommandCapture::default()));
        let (sized, sized_rx) = mpsc::channel::<()>();
        let mut cwd = saved.as_ref().map(|dir| CwdTracker::new(dir.join("cwd")));
        let saved = saved.and_then(|dir| SavedOutput::open(dir.join("output")).ok());
        let (p, w, done, ctx, capture, drawn) = (parser.clone(), writer.clone(), exited.clone(), ctx.clone(), last_command.clone(), drawn_at_frame.clone());
        std::thread::spawn(move || {
            let mut saved = saved.map(|(mut output, previous)| {
                if !previous.is_empty() && sized_rx.recv().is_ok() {
                    output.restore(&mut p.lock().unwrap(), &previous);
                }
                output
            });
            let mut buf = [0u8; 64 * 1024];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                let replies = {
                    let mut p = p.lock().unwrap();
                    p.process(&buf[..n]);
                    std::mem::take(&mut p.callbacks_mut().0)
                };
                capture.lock().unwrap().record(&buf[..n]);
                if let Some(output) = &mut saved {
                    output.append(&buf[..n]);
                }
                if let Some(cwd) = &mut cwd {
                    cwd.feed(&buf[..n]);
                }
                if !replies.is_empty() {
                    let _ = w.send(replies);
                }
                if drawn_recently(&ctx, &drawn) {
                    ctx.request_repaint();
                }
            }
            done.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });

        Ok(Terminal { parser, writer, drawn_at_frame, master: pair.master, child: Some(child), exited, size, scroll_rest: 0.0, pending_paste: None, selection: None, key_anchor: None, last_command, tab_steps: Vec::new(), sized: Some(sized) })
    }

    fn cursor(&self) -> (u16, u16) {
        self.parser.lock().unwrap().screen().cursor_position()
    }

    fn clear_selection(&mut self) {
        self.key_anchor = None;
        self.selection = None;
    }

    fn follow_cursor(&mut self) {
        if let Some(anchor) = self.key_anchor {
            self.selection = between_gaps(anchor, self.cursor(), self.size.1);
        }
    }

    fn erase_key_selection(&mut self) -> Option<Vec<u8>> {
        let anchor = self.key_anchor?;
        let cursor = self.cursor();
        self.clear_selection();
        (anchor != cursor).then(|| erase_bytes(anchor, cursor, self.size.1))
    }

    fn key_selection_input(&mut self, event: &egui::Event, app_cursor: bool, out: &mut Vec<u8>) -> bool {
        match event {
            egui::Event::Key { key, pressed: true, modifiers, .. } if let Some(bytes) = selection_move(*key, *modifiers, app_cursor) => {
                if self.key_anchor.is_none() {
                    self.selection = None;
                    self.key_anchor = Some(self.cursor());
                }
                out.extend(bytes);
                true
            }
            _ if self.key_anchor.is_none() => false,
            egui::Event::Key { key: Key::Backspace | Key::Delete, pressed: true, .. } => {
                out.extend(self.erase_key_selection().unwrap_or_default());
                true
            }
            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                out.extend(self.erase_key_selection().unwrap_or_default());
                false
            }
            egui::Event::Copy => false,
            egui::Event::Key { key, pressed: true, .. } if !matches!(key, Key::C | Key::V | Key::X) && !is_modifier(*key) => {
                self.clear_selection();
                false
            }
            _ => false,
        }
    }

    pub(super) fn take_tab_steps(&mut self) -> Vec<isize> {
        std::mem::take(&mut self.tab_steps)
    }


    fn focus(&self, ctx: &egui::Context) {
        ctx.memory_mut(|m| m.request_focus(self.widget_id()));
    }

    fn has_focus(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(self.widget_id()))
    }

    fn widget_id(&self) -> egui::Id {
        egui::Id::new(("terminal", Arc::as_ptr(&self.parser) as usize))
    }

    fn take_finished(&self) -> Option<Duration> {
        self.last_command.lock().unwrap().take_finished()
    }

    fn waiting(&self) -> bool {
        self.last_command.lock().unwrap().waiting()
    }

    pub(super) fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    fn send(&self, bytes: &[u8]) {
        let _ = self.writer.send(bytes.to_vec());
    }

    fn type_command(&mut self, command: &str) {
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
        self.send(format!("{command}\r").as_bytes());
    }

    /// Paste `text` and submit it. It waits until the program turns on
    /// bracketed paste, which Claude does once its prompt is ready, so it is
    /// not typed into a starting program or interpreted as keys.
    pub(super) fn paste(&mut self, text: String) {
        self.pending_paste = Some(text);
        self.flush_paste();
    }

    fn flush_paste(&mut self) {
        if self.pending_paste.is_none() || !self.parser.lock().unwrap().screen().bracketed_paste() {
            return;
        }
        let text = self.pending_paste.take().unwrap();
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
        self.send(&[b"\x1b[200~", text.as_bytes(), b"\x1b[201~"].concat());
        // Enter a moment later, once the program has taken in the paste:
        // in the same read, it could count as part of it.
        let writer = self.writer.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let _ = writer.send(b"\r".to_vec());
        });
    }

    /// Screen text, for tests.
    #[cfg(test)]
    fn contents(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// Draw the terminal in all the available space, and forward input to it
    /// while it has the keyboard focus (click it to focus).
    pub(super) fn ui(&mut self, ui: &mut egui::Ui) {
        self.drawn_at_frame.store(root_frame(ui.ctx()), Ordering::Relaxed);
        self.flush_paste();
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let (cw, rh) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let response = ui.interact(rect, self.widget_id(), Sense::click_and_drag());
        let size = (((rect.height() / rh) as u16).max(2), ((rect.width() / cw) as u16).max(10));
        if size != self.size {
            self.size = size;
            let _ = self.master.resize(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 });
            self.parser.lock().unwrap().screen_mut().set_size(size.0, size.1);
        }
        if let Some(sized) = self.sized.take() {
            let _ = sized.send(());
        }
        if response.clicked() {
            response.request_focus();
            self.clear_selection();
        }
        self.select_with_mouse(ui, &response, rect, cw, rh);
        if (response.drag_stopped_by(egui::PointerButton::Primary) || response.double_clicked())
            && let Some(text) = self.selected_text()
        {
            set_primary_text(text);
        }
        if response.clicked_by(egui::PointerButton::Middle) {
            response.request_focus();
            if let Some(text) = primary_text() {
                self.paste_text(&text);
            }
        }
        response.context_menu(|ui| {
            if ui.add_enabled(self.selection.is_some(), egui::Button::new("📋 Copy")).on_hover_text("Ctrl+C or Ctrl+Shift+C").clicked() {
                self.copy_selection(ui.ctx());
                ui.close();
            }
        });
        let focused = response.has_focus();
        if focused {
            let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
            ui.memory_mut(|m| m.set_focus_lock_filter(response.id, filter));
            self.input(ui);
        }
        if ui.rect_contains_pointer(rect) {
            self.wheel(ui, rect, cw, rh);
        }

        self.follow_cursor();
        let fg0 = ui.visuals().text_color();
        let bg0 = ui.visuals().panel_fill;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, bg0);
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let at = |row: u16, col: u16| rect.min + vec2(col as f32 * cw, row as f32 * rh);
        let (rows, cols) = screen.size();
        for row in 0..rows {
            let mut col = 0;
            while col < cols {
                let Some(cell) = screen.cell(row, col) else { break };
                let style = Style::of(cell);
                // Plain ASCII cells with the same style are drawn as one run;
                // any other character gets its own cell(s) so columns line up
                // whatever the font's width for it.
                let start = col;
                let mut text = String::new();
                if is_ascii(cell) {
                    while let Some(c) = screen.cell(row, col)
                        && col < cols
                        && is_ascii(c)
                        && Style::of(c) == style
                    {
                        text.push_str(if c.has_contents() { c.contents() } else { " " });
                        col += 1;
                    }
                } else {
                    text.push_str(cell.contents());
                    col += if cell.is_wide() { 2 } else { 1 };
                }
                let (fg, bg) = style.colors(fg0, bg0);
                let run = egui::Rect::from_min_size(at(row, start), vec2((col - start) as f32 * cw, rh));
                if bg != bg0 {
                    painter.rect_filled(run, 0.0, bg);
                }
                let text = text.trim_end();
                if !text.is_empty() {
                    painter.text(run.min, egui::Align2::LEFT_TOP, text, font.clone(), fg);
                }
                if style.underline {
                    painter.hline(run.x_range(), run.bottom() - 1.0, Stroke::new(1.0, fg));
                }
            }
        }
        if let Some(selection) = self.selection {
            let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.6);
            for row in 0..rows {
                if let Some((first, last)) = selection.columns_on(row, cols) {
                    let band = egui::Rect::from_min_size(at(row, first), vec2((last - first + 1) as f32 * cw, rh));
                    painter.rect_filled(band, 0.0, fill);
                }
            }
        }
        if !screen.hide_cursor() && screen.scrollback() == 0 {
            let (row, col) = screen.cursor_position();
            let cursor = egui::Rect::from_min_size(at(row, col), vec2(cw, rh));
            if focused {
                painter.rect_filled(cursor, 0.0, fg0.gamma_multiply(0.6));
            } else {
                painter.rect_stroke(cursor, 0.0, Stroke::new(1.0, fg0.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
        }
        if screen.scrollback() > 0 {
            let label = format!("↑ {} lines back · scroll down or type to return", screen.scrollback());
            painter.text(rect.right_top() + vec2(-8.0, 4.0), egui::Align2::RIGHT_TOP, label, font, Color32::GRAY);
        }
    }

    /// Mouse wheel over the terminal, in whole lines.
    fn wheel(&mut self, ui: &egui::Ui, rect: egui::Rect, cw: f32, rh: f32) {
        let (dy, pos) = ui.input(|i| (i.smooth_scroll_delta.y, i.pointer.hover_pos()));
        if dy == 0.0 {
            return;
        }
        self.scroll_rest += dy / rh;
        let lines = self.scroll_rest.trunc();
        self.scroll_rest -= lines;
        if lines != 0.0 {
            let p = pos.unwrap_or(rect.min) - rect.min;
            self.scroll(lines as i64, ((p.y / rh) as u16, (p.x / cw) as u16));
        }
    }

    /// Scroll up (`lines` > 0) or down, the pointer being over `cell` (row,
    /// col). A program that asked for mouse events gets wheel events; on the
    /// alternate screen (full-screen programs) the wheel sends arrow keys, as
    /// in xterm; otherwise it browses the scrollback.
    fn scroll(&mut self, lines: i64, cell: (u16, u16)) {
        self.selection = None;
        let n = lines.unsigned_abs() as usize;
        let up = lines > 0;
        let mut parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let bytes = if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
            let (button, row, col) = (if up { 64 } else { 65 }, cell.0 + 1, cell.1 + 1);
            match screen.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{button};{col};{row}M").into_bytes(),
                _ => vec![0x1b, b'[', b'M', 32 + button, 32 + col.min(223) as u8, 32 + row.min(223) as u8],
            }
        } else if screen.alternate_screen() {
            let arrow = if up { 'A' } else { 'B' };
            if screen.application_cursor() { format!("\x1bO{arrow}") } else { format!("\x1b[{arrow}") }.into_bytes()
        } else {
            let back = (screen.scrollback() as i64 + lines).max(0) as usize;
            parser.screen_mut().set_scrollback(back);
            return;
        };
        drop(parser);
        self.send(&bytes.repeat(n));
    }

    fn select_with_mouse(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect, cw: f32, rh: f32) {
        let (rows, cols) = self.size;
        let cell = |pos: egui::Pos2| {
            let row = ((pos.y - rect.top()) / rh).clamp(0.0, (rows - 1) as f32) as u16;
            let col = ((pos.x - rect.left()) / cw).clamp(0.0, (cols - 1) as f32) as u16;
            (row, col)
        };
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.key_anchor = None;
            self.selection = Some(Selection { anchor: cell(origin), head: cell(origin) });
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let (Some(selection), Some(pos)) = (&mut self.selection, response.interact_pointer_pos())
        {
            selection.head = cell(pos);
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let (row, col) = cell(pos);
            let line: Vec<char> = {
                let parser = self.parser.lock().unwrap();
                let screen = parser.screen();
                (0..cols).map(|c| screen.cell(row, c).map(|x| x.contents().chars().next().unwrap_or(' ')).unwrap_or(' ')).collect()
            };
            let (start, end) = word_at(&line, col as usize);
            self.selection = Some(Selection { anchor: (row, start as u16), head: (row, end as u16) });
        }
    }

    pub(super) fn last_output(&self) -> Option<String> {
        let capture = self.last_command.lock().unwrap();
        if !capture.recording {
            return None;
        }
        Some(rendered_text(command_output(&capture.bytes), self.size.1))
    }

    fn paste_text(&mut self, text: &str) {
        let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
        self.selection = None;
        self.send(&paste_bytes(text, bracketed));
    }

    fn selected_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.selection?.ordered();
        let text = self.parser.lock().unwrap().screen().contents_between(r0, c0, r1, c1 + 1);
        Some(text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")).filter(|t| !t.is_empty())
    }

    fn copy_selection(&mut self, ctx: &egui::Context) -> bool {
        match self.selected_text() {
            Some(text) => {
                ctx.copy_text(text);
                true
            }
            None => false,
        }
    }

    fn input(&mut self, ui: &egui::Ui) {
        let (app_cursor, paste_mode) = {
            let p = self.parser.lock().unwrap();
            (p.screen().application_cursor(), p.screen().bracketed_paste())
        };
        let mut out = Vec::new();
        for event in ui.input(|i| i.events.clone()) {
            if self.key_selection_input(&event, app_cursor, &mut out) {
                continue;
            }
            match event {
                egui::Event::Text(t) | egui::Event::Ime(egui::ImeEvent::Commit(t)) => out.extend(t.bytes()),
                egui::Event::Paste(t) => out.extend(paste_bytes(&t, paste_mode)),
                egui::Event::Copy => {
                    let shift = ui.input(|i| i.modifiers.shift);
                    if !self.copy_selection(ui.ctx()) && !shift {
                        out.push(0x03);
                    }
                }
                egui::Event::Cut => out.push(0x18),
                egui::Event::Key { key: key @ (Key::PageUp | Key::PageDown), pressed: true, modifiers, .. } if modifiers.ctrl && !modifiers.shift && !modifiers.alt => {
                    self.tab_steps.push(if key == Key::PageUp { -1 } else { 1 });
                }
                egui::Event::Key { key: key @ (Key::PageUp | Key::PageDown), pressed: true, modifiers, .. } if modifiers.shift => {
                    let page = self.size.0 as i64 - 1;
                    self.scroll(if key == Key::PageUp { page } else { -page }, (0, 0));
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if key == Key::Enter && !modifiers.shift && !modifiers.alt {
                        self.last_command.lock().unwrap().start();
                    }
                    out.extend(key_bytes(key, modifiers, app_cursor).unwrap_or_default());
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.parser.lock().unwrap().screen_mut().set_scrollback(0);
            if self.key_anchor.is_none() {
                self.selection = None;
            }
            self.send(&out);
        }
    }
}

fn is_ascii(cell: &vt100::Cell) -> bool {
    !cell.has_contents() || cell.contents().is_ascii()
}

#[derive(PartialEq, Clone, Copy)]
struct Style {
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    dim: bool,
    inverse: bool,
    underline: bool,
}

impl Style {
    fn of(c: &vt100::Cell) -> Self {
        Style { fg: c.fgcolor(), bg: c.bgcolor(), bold: c.bold(), dim: c.dim(), inverse: c.inverse(), underline: c.underline() }
    }

    fn colors(&self, fg0: Color32, bg0: Color32) -> (Color32, Color32) {
        // Bold text in one of the 8 basic colors uses its bright variant.
        let fg = match self.fg {
            vt100::Color::Idx(i) if self.bold && i < 8 => vt100::Color::Idx(i + 8),
            c => c,
        };
        let mut fg = color(fg, fg0);
        if self.dim {
            fg = fg.gamma_multiply(0.6);
        }
        let bg = color(self.bg, bg0);
        if self.inverse { (bg, fg) } else { (fg, bg) }
    }
}

pub(super) fn ansi_index_color(index: u8, default: Color32) -> Color32 {
    color(vt100::Color::Idx(index), default)
}

fn color(c: vt100::Color, default: Color32) -> Color32 {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x1d, 0x1f, 0x21), (0xcc, 0x44, 0x44), (0x5f, 0xb3, 0x4f), (0xd7, 0xaf, 0x37),
        (0x4f, 0x8f, 0xd8), (0xb3, 0x6c, 0xd0), (0x3e, 0xb3, 0xb0), (0xc5, 0xc8, 0xc6),
        (0x66, 0x66, 0x66), (0xff, 0x6b, 0x6b), (0x8a, 0xe2, 0x34), (0xff, 0xd7, 0x5f),
        (0x7a, 0xb4, 0xff), (0xd7, 0x8a, 0xff), (0x66, 0xe0, 0xdc), (0xff, 0xff, 0xff),
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i @ 0..16) => {
            let (r, g, b) = ANSI[i as usize];
            Color32::from_rgb(r, g, b)
        }
        vt100::Color::Idx(i @ 16..232) => {
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            let i = i - 16;
            Color32::from_rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => Color32::from_gray(8 + 10 * (i - 232)),
    }
}

/// What a terminal sends for a key press (text keys arrive as text events).
fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let cursor = |c: char| {
        Some(if modifier > 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        })
    };
    let tilde = |n: u8| Some(if modifier > 1 { format!("\x1b[{n};{modifier}~") } else { format!("\x1b[{n}~") }.into_bytes());
    let word_jump = m.ctrl || m.alt;
    match key {
        Key::ArrowLeft if word_jump && !m.shift => Some(b"\x1bb".to_vec()),
        Key::ArrowRight if word_jump && !m.shift => Some(b"\x1bf".to_vec()),
        Key::Delete if word_jump => Some(b"\x1bd".to_vec()),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        // Shift/Alt+Enter: a new line in Claude's prompt instead of sending it.
        Key::Enter if m.shift || m.alt => Some(b"\x1b\r".to_vec()),
        Key::Enter => Some(b"\r".to_vec()),
        Key::Tab if m.shift => Some(b"\x1b[Z".to_vec()),
        Key::Tab => Some(b"\t".to_vec()),
        Key::Backspace if m.ctrl => Some(vec![0x17]),
        Key::Backspace if m.alt => Some(b"\x1b\x7f".to_vec()),
        Key::Backspace => Some(vec![0x7f]),
        Key::Escape => Some(b"\x1b".to_vec()),
        Key::F1 => Some(b"\x1bOP".to_vec()),
        Key::F2 => Some(b"\x1bOQ".to_vec()),
        Key::F3 => Some(b"\x1bOR".to_vec()),
        Key::F4 => Some(b"\x1bOS".to_vec()),
        Key::Space if m.ctrl => Some(vec![0]),
        _ if m.ctrl => {
            let ctrl = match (key, key.name().as_bytes()) {
                (_, [c @ b'A'..=b'Z']) => c - b'A' + 1,
                (Key::OpenBracket, _) => 0x1b,
                (Key::Backslash, _) => 0x1c,
                (Key::CloseBracket, _) => 0x1d,
                _ => return None,
            };
            Some(if m.alt { vec![0x1b, ctrl] } else { vec![ctrl] })
        }
        _ => None,
    }
}

/// The Claude terminals of a workspace, one per conversation in its
/// `sessions`. Hidden ones keep running.
#[derive(Default)]
pub(super) struct Terminals {
    running: HashMap<String, Result<Terminal, String>>,
    /// Index of the shown one in `sessions`.
    shown: usize,
    /// The restart confirmation is open.
    confirm_restart: bool,
    /// The new conversation confirmation is open.
    confirm_renew: bool,
    /// The tab whose close confirmation is open.
    confirm_close: Option<usize>,
    copied_at: Option<Instant>,
    /// Text for the shown terminal, handed to it once it is running.
    paste: Option<String>,
    command: Option<String>,
    renaming: Option<(usize, String)>,
    fork_graph: bool,
    refocus: bool,
    done: std::collections::HashSet<String>,
    pub(super) window_toggle: bool,
    pub(super) maximized: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum TermActivity {
    Idle,
    Working,
    Done,
}

pub(super) fn terminal_activity_mark(ui: &mut egui::Ui, activity: TermActivity) {
    match activity {
        TermActivity::Working => {
            ui.add(egui::Spinner::new().size(ui.text_style_height(&egui::TextStyle::Body))).on_hover_text("Working…");
        }
        TermActivity::Done => tab_label(ui, RichText::new("✔").color(Color32::from_rgb(0x4c, 0xc0, 0x6a)).strong()),
        TermActivity::Idle => {}
    }
}

impl Terminals {
    pub(super) fn activity(&self) -> TermActivity {
        let working = self.running.values().flatten().any(|t| !t.exited() && t.waiting());
        if working {
            TermActivity::Working
        } else if self.done.is_empty() {
            TermActivity::Idle
        } else {
            TermActivity::Done
        }
    }

    pub(super) fn poll_finished(&mut self, sessions: &[TermTab], watched: bool, ctx: &egui::Context) -> Vec<String> {
        let mut finished = Vec::new();
        for (i, tab) in sessions.iter().enumerate() {
            let Some(Ok(terminal)) = self.running.get(&tab.id) else { continue };
            if terminal.waiting() {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
            let Some(took) = terminal.take_finished().filter(|took| *took >= worth_notifying(tab.kind)) else { continue };
            if watched && i == self.shown {
                continue;
            }
            self.done.insert(tab.id.clone());
            let name = if tab.name.is_empty() { default_name(sessions, i) } else { tab.name.clone() };
            let what = if tab.kind == TermKind::Claude { "answered" } else { "finished its command" };
            finished.push(format!("{name} {what} (after {}s)", took.as_secs()));
        }
        finished
    }

    /// Show the first tab of `kind`, opening one if there is none.
    pub(super) fn show_kind(&mut self, tabs: &mut Vec<TermTab>, kind: TermKind) {
        self.shown = tabs.iter().position(|t| t.kind == kind).unwrap_or_else(|| {
            tabs.push(TermTab::new(kind));
            tabs.len() - 1
        });
    }

    pub(super) fn has_focus(&self, sessions: &[TermTab], ctx: &egui::Context) -> bool {
        let shown = sessions.get(self.shown).and_then(|t| self.running.get(&t.id));
        shown.and_then(|r| r.as_ref().ok()).is_some_and(|t| ctx.memory(|m| m.has_focus(t.widget_id())))
    }

    pub(super) fn focus_shown(&mut self) {
        self.refocus = true;
    }

    pub(super) fn open_new(&mut self, sessions: &mut Vec<TermTab>, kind: TermKind) {
        sessions.push(TermTab::new(kind));
        self.shown = sessions.len() - 1;
        self.refocus = true;
    }

    pub(super) fn picker(&self, sessions: &[TermTab]) -> TerminalPicker {
        let kind = sessions.get(self.shown).map_or(TermKind::Claude, |tab| tab.kind);
        TerminalPicker { selected: NEW_TERMINAL_KINDS.iter().position(|k| *k == kind).unwrap_or(0), in_terminal_window: false }
    }

    /// Paste `text` in a Claude terminal and submit it.
    pub(super) fn paste(&mut self, text: String) {
        self.paste = Some(text);
    }

    pub(super) fn run_in_shell(&mut self, sessions: &mut Vec<TermTab>, kind: TermKind, command: String) {
        self.show_kind(sessions, kind);
        self.command = Some(command);
        self.refocus = true;
    }

    /// End the running terminals of `kind`; they start again when shown.
    pub(super) fn end(&mut self, tabs: &[TermTab], kind: TermKind) {
        for t in tabs.iter().filter(|t| t.kind == kind) {
            self.running.remove(&t.id);
        }
    }
}

/// The terminal panel of a tab: one tab per Claude conversation, and the
/// shown terminal, started the first time it is shown.
pub(super) fn terminal_panel(terms: &mut Terminals, sessions: &mut Vec<TermTab>, forks: &mut Vec<Fork>, (local, remote): (&str, &str), ui: &mut egui::Ui, show: &mut bool) {
    if sessions.is_empty() {
        sessions.push(TermTab::new(TermKind::Claude));
    }
    terms.shown = terms.shown.min(sessions.len() - 1);
    // A paste is for Claude: show a Claude tab, opening one if needed.
    if terms.paste.is_some() && sessions[terms.shown].kind != TermKind::Claude {
        terms.shown = sessions.iter().position(|t| t.kind == TermKind::Claude).unwrap_or_else(|| {
            sessions.push(TermTab::new(TermKind::Claude));
            sessions.len() - 1
        });
    }
    let defaults: Vec<String> = (0..sessions.len()).map(|i| default_name(sessions, i)).collect();
    let names: Vec<String> = sessions.iter().zip(&defaults).map(|(t, d)| if t.name.is_empty() { d.clone() } else { t.name.clone() }).collect();
    let (mut restart, mut renew, mut close, mut fork, mut tab_move) = (false, false, None, None, None);
    ui.horizontal(|ui| {
        ui.label(RichText::new("⌨").color(CLAUDE).strong());
        let several = sessions.len() > 1;
        for (i, tab) in sessions.iter_mut().enumerate() {
            let id = &tab.id;
            let ended = matches!(terms.running.get(id), Some(Ok(t)) if t.exited());
            let busy = !ended && matches!(terms.running.get(id), Some(Ok(t)) if t.waiting());
            if let Some((j, text)) = &mut terms.renaming
                && *j == i
            {
                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(120.0).hint_text(&defaults[i]));
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    terms.renaming = None;
                } else if edit.lost_focus() {
                    tab.name = text.trim().to_string();
                    terms.renaming = None;
                }
            } else {
                let what = match tab.kind {
                    TermKind::Claude => format!("conversation {id}"),
                    TermKind::Remote => format!("shell in {remote}"),
                    TermKind::Local => format!("shell in {local}"),
                };
                let close_help = match tab.kind {
                    TermKind::Claude => format!("End this Claude and close its tab. The conversation is kept on the remote ({PROGRAM} --resume {id})."),
                    _ => "End this shell and close its tab".to_string(),
                };
                let name = RichText::new(&names[i]).color(kind_color(tab.kind, ui)).strong();
                let shown = closable_tab(
                    ui,
                    i == terms.shown,
                    |ui| {
                        tab_label(ui, name);
                        let activity = if busy {
                            TermActivity::Working
                        } else if terms.done.contains(id) {
                            TermActivity::Done
                        } else {
                            TermActivity::Idle
                        };
                        terminal_activity_mark(ui, activity);
                        if ended {
                            tab_label(ui, RichText::new("ended").color(Color32::LIGHT_RED));
                        }
                    },
                    several.then_some(close_help.as_str()),
                );
                if shown.close {
                    terms.confirm_close = Some(i);
                }
                if let Some(moved) = dragged_onto(ui, egui::Id::new("terminal-tabs"), i, &shown.response) {
                    tab_move = Some(moved);
                }
                let label = shown.response.on_hover_text(format!("{what}\ndouble-click to rename · drag to reorder"));
                if label.clicked() {
                    terms.shown = i;
                    terms.refocus = true;
                    *show = true;
                }
                if label.double_clicked() {
                    terms.renaming = Some((i, tab.name.clone()));
                }
                label.context_menu(|ui| {
                    if ui.button("✏ Rename").clicked() {
                        terms.renaming = Some((i, tab.name.clone()));
                    }
                    if tab.kind == TermKind::Claude {
                        if ui.button("⑂ Fork").on_hover_text(FORK_HELP).clicked() {
                            fork = Some(i);
                        }
                        if ui.button("🌳 Fork graph").clicked() {
                            terms.fork_graph = true;
                        }
                    }
                });
            }
        }
        ui.menu_button("➕", |ui| {
            for kind in NEW_TERMINAL_KINDS {
                if ui.button(RichText::new(new_kind_text(kind)).color(kind_color(kind, ui))).on_hover_text(new_kind_help(kind, local, remote)).clicked() {
                    terms.open_new(sessions, kind);
                }
            }
        })
        .response
        .on_hover_text("New terminal tab: Claude, or a shell on the remote or local side");
        let kind = sessions[terms.shown].kind;
        ui.weak(format!("in {}", if kind == TermKind::Local { local } else { remote }));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !*show {
                if ui.small_button("🗗").on_hover_text("Show the terminals").clicked() {
                    *show = true;
                }
            } else {
                if ui.small_button("🗕").on_hover_text("Minimize the terminals to this bar (they keep running)").clicked() {
                    *show = false;
                }
                let (icon, help) = if terms.maximized { ("🗗", "Restore the terminals to the bottom panel") } else { ("🗖", "Enlarge the terminals to the whole window") };
                if ui.small_button(icon).on_hover_text(help).clicked() {
                    terms.maximized = !terms.maximized;
                }
                if ui.small_button("⧉").on_hover_text(WINDOW_HELP).clicked() {
                    terms.window_toggle = true;
                }
            }
            let help = match kind {
                TermKind::Claude => "End this session and start again, resuming its conversation (e.g. after a lost connection)",
                _ => "End this shell and start a new one (e.g. after a lost connection)",
            };
            if kind != TermKind::Claude
                && let Some(Ok(terminal)) = terms.running.get(&sessions[terms.shown].id)
            {
                let copied = terms.copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                let label = if copied { "✔ Copied" } else { "📋 Last output" };
                let button = ui.small_button(label).on_hover_text("Copy the output of the last command run in this shell");
                if button.clicked() {
                    match terminal.last_output() {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            terms.copied_at = Some(Instant::now());
                            ui.ctx().request_repaint_after(Duration::from_secs(2));
                        }
                        None => terms.copied_at = None,
                    }
                }
            }
            if ui.small_button("⟲ Reconnect").on_hover_text(help).clicked() {
                terms.confirm_restart = true;
            }
            if kind == TermKind::Claude {
                if ui.small_button("🌳 Forks").on_hover_text("The graph of the forked conversations").clicked() {
                    terms.fork_graph = true;
                }
                if ui.small_button("⑂ Fork").on_hover_text(FORK_HELP).clicked() {
                    fork = Some(terms.shown);
                }
            }
            if kind == TermKind::Claude
                && ui
                    .small_button("🗋 Clear")
                    .on_hover_text("Restart this tab on a new conversation. Use it instead of /clear, whose new conversation would not be resumed.")
                    .clicked()
            {
                terms.confirm_renew = true;
            }
            if *show {
                ui.weak("click to type · wheel to scroll back");
            }
        });
    });
    let shown_name = match &sessions[terms.shown].name {
        n if n.is_empty() => default_name(sessions, terms.shown),
        n => n.clone(),
    };
    let shown_kind = sessions[terms.shown].kind;
    if terms.confirm_restart
        && let Some(yes) = confirm(
            ui,
            egui::Id::new(("confirm-restart", remote)),
            &format!("Reconnect {shown_name}?"),
            &match shown_kind {
                TermKind::Claude => format!("The {PROGRAM} running in this terminal is ended, interrupting what it is doing, then started again on the same conversation."),
                _ => "The shell running in this terminal is ended, with whatever runs in it, and a new one is started.".to_string(),
            },
            "⟲ Reconnect",
        )
    {
        terms.confirm_restart = false;
        restart = yes;
    }
    if terms.confirm_renew
        && let Some(yes) = confirm(
            ui,
            egui::Id::new(("confirm-renew", remote)),
            &format!("Clear {shown_name}?"),
            &format!(
                "The {PROGRAM} running in this terminal is ended, interrupting what it is doing, and started again on a new conversation. \
                 The current one stays on the remote ({PROGRAM} --resume {}) but this tab no longer resumes it.",
                sessions[terms.shown].id
            ),
            "🗋 Clear",
        )
    {
        terms.confirm_renew = false;
        renew = yes;
    }
    if let Some(moved) = tab_move {
        move_tab(sessions, moved);
        terms.shown = index_after_move(terms.shown, moved);
        terms.confirm_close = terms.confirm_close.map(|i| index_after_move(i, moved));
        terms.renaming = None;
    }
    if let Some(i) = terms.confirm_close.filter(|&i| i < sessions.len())
        && let Some(yes) = confirm(
            ui,
            egui::Id::new(("confirm-close-terminal", remote)),
            &format!(
                "Close {}?",
                match &sessions[i].name {
                    n if n.is_empty() => default_name(sessions, i),
                    n => n.clone(),
                }
            ),
            &match sessions[i].kind {
                TermKind::Claude => format!(
                    "The {PROGRAM} running in this tab is ended, interrupting what it is doing. Its conversation stays on the remote ({PROGRAM} --resume {}), but no tab resumes it any more.",
                    sessions[i].id
                ),
                _ => "The shell running in this tab is ended, with whatever runs in it.".to_string(),
            },
            "× Close",
        )
    {
        terms.confirm_close = None;
        close = yes.then_some(i);
    }
    if let Some(i) = close {
        let closed = sessions.remove(i);
        terms.running.remove(&closed.id);
        forget_saved(&closed);
        terms.renaming = None;
        if terms.shown > i || terms.shown == sessions.len() {
            terms.shown -= 1;
        }
    }
    if renew {
        let old = std::mem::replace(&mut sessions[terms.shown].id, new_session_id());
        terms.running.remove(&old);
    }
    if let Some(i) = fork {
        terms.shown = fork_tab(sessions, forks, i, &names[i]);
        terms.refocus = true;
        *show = true;
    }
    if terms.fork_graph {
        let graph = ForkGraph { sessions, forks, names: &names, shown: terms.shown, remote };
        match fork_graph_window(ui.ctx(), &mut terms.fork_graph, &graph) {
            Some(GraphAction::Show(i)) => {
                terms.shown = i;
                *show = true;
                terms.refocus = true;
            }
            Some(GraphAction::Fork(i)) => {
                terms.shown = fork_tab(sessions, forks, i, &names[i]);
                *show = true;
                terms.refocus = true;
            }
            Some(GraphAction::Reopen(id)) => {
                sessions.push(TermTab { id, name: String::new(), kind: TermKind::Claude });
                terms.shown = sessions.len() - 1;
                *show = true;
                terms.refocus = true;
            }
            None => {}
        }
    }
    if !*show {
        return;
    }
    let tab = &sessions[terms.shown];
    terms.done.remove(&tab.id);
    if restart {
        terms.running.remove(&tab.id);
    }
    let terminal = terms.running.entry(tab.id.clone()).or_insert_with(|| {
        tab_command(tab, Fork::parent_of(forks, &tab.id), local, remote).and_then(|cmd| Terminal::start_saved(cmd, ui.ctx(), saved_shell_dir(tab))).map_err(|e| format!("{e:#}"))
    });
    match terminal {
        Ok(t) => {
            if let Some(text) = terms.paste.take() {
                t.paste(text);
            }
            if let Some(command) = terms.command.take() {
                t.type_command(&command);
            }
            let clicking = ui.input(|i| i.pointer.any_click());
            if clicking && terms.refocus {
                ui.ctx().request_repaint();
            } else if terms.refocus {
                t.focus(ui.ctx());
            }
            t.ui(ui);
            if terms.refocus && t.has_focus(ui.ctx()) {
                terms.refocus = false;
            } else if terms.refocus && !clicking {
                ui.ctx().request_repaint();
            }
            let steps = t.take_tab_steps();
            terms.refocus |= !steps.is_empty();
            for step in steps {
                switch_tab(terms, sessions.len(), step);
            }
        }
        Err(e) => {
            ui.colored_label(Color32::LIGHT_RED, format!("could not start the terminal: {e}"));
        }
    }
}

pub(super) const WINDOW_HELP: &str = "Open this panel in its own window, or put it back in the main window (closing the window puts it back too)";

const NEW_TERMINAL_KINDS: [TermKind; 3] = [TermKind::Claude, TermKind::Remote, TermKind::Local];

fn new_kind_text(kind: TermKind) -> &'static str {
    match kind {
        TermKind::Claude => "✳ Claude",
        TermKind::Remote => "🖧 Remote shell",
        TermKind::Local => "🖥 Local shell",
    }
}

fn new_kind_help(kind: TermKind, local: &str, remote: &str) -> String {
    match kind {
        TermKind::Claude => format!("Another Claude in {remote}, with its own conversation"),
        TermKind::Remote => format!("A shell in {remote}"),
        TermKind::Local => format!("A shell in {local}"),
    }
}

pub(super) enum PickerOutcome {
    Open,
    Closed,
    Chosen(TermKind),
}

pub(super) struct TerminalPicker {
    selected: usize,
    pub(super) in_terminal_window: bool,
}

impl TerminalPicker {
    pub(super) fn keys(&mut self, ctx: &egui::Context) -> PickerOutcome {
        ctx.input_mut(|i| {
            let down = i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as isize;
            let up = i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as isize;
            let count = NEW_TERMINAL_KINDS.len() as isize;
            self.selected = (self.selected as isize + down - up).rem_euclid(count) as usize;
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                PickerOutcome::Closed
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                PickerOutcome::Chosen(NEW_TERMINAL_KINDS[self.selected])
            } else {
                PickerOutcome::Open
            }
        })
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, local: &str, remote: &str) -> PickerOutcome {
        let mut outcome = PickerOutcome::Open;
        let modal = egui::Modal::new(egui::Id::new("new-terminal")).show(ctx, |ui| {
            ui.heading("New terminal");
            for (i, kind) in NEW_TERMINAL_KINDS.into_iter().enumerate() {
                let text = RichText::new(new_kind_text(kind)).color(kind_color(kind, ui)).strong();
                let row = ui.selectable_label(i == self.selected, text).on_hover_text(new_kind_help(kind, local, remote));
                if row.clicked() {
                    outcome = PickerOutcome::Chosen(kind);
                }
            }
            ui.weak("↑ ↓ to choose · Enter to open · Esc to close");
        });
        match outcome {
            PickerOutcome::Open if modal.should_close() => PickerOutcome::Closed,
            outcome => outcome,
        }
    }
}

pub(super) fn notify_desktop(title: &str, body: &str) {
    let sent = std::process::Command::new("notify-send").args(["--app-name=agent-diff", title, body]).stdin(std::process::Stdio::null()).spawn();
    if let Ok(mut child) = sent {
        std::thread::spawn(move || child.wait());
    }
}

fn switch_tab(terms: &mut Terminals, count: usize, step: isize) {
    terms.shown = (terms.shown as isize + step).rem_euclid(count as isize) as usize;
}

const FORK_HELP: &str = "A new tab with a copy of this conversation, its whole history, that then goes its own way (claude --resume --fork-session)";

fn fork_tab(sessions: &mut Vec<TermTab>, forks: &mut Vec<Fork>, i: usize, name: &str) -> usize {
    let child = TermTab { id: new_session_id(), name: format!("{name} ⑂"), kind: TermKind::Claude };
    forks.push(Fork { child: child.id.clone(), parent: sessions[i].id.clone() });
    let at = i + 1 + sessions[i + 1..].iter().take_while(|t| Fork::parent_of(forks, &t.id) == Some(&sessions[i].id)).count();
    sessions.insert(at, child);
    at
}

/// `claude N`, `remote N` or `local N`: numbered among the tabs of its kind.
fn default_name(tabs: &[TermTab], i: usize) -> String {
    let kind = tabs[i].kind;
    let n = tabs[..=i].iter().filter(|t| t.kind == kind).count();
    let word = match kind {
        TermKind::Claude => PROGRAM,
        TermKind::Remote => "remote",
        TermKind::Local => "local",
    };
    format!("{word} {n}")
}

pub(super) fn kind_color(kind: TermKind, ui: &egui::Ui) -> Color32 {
    match kind {
        TermKind::Claude => CLAUDE,
        TermKind::Remote => ui.visuals().text_color(),
        TermKind::Local => YOU,
    }
}

/// Asks before an action that ends something running, e.g. a Claude.
/// Returns `Some(true)` to go ahead, `Some(false)` to cancel.
pub(super) fn confirm(ui: &egui::Ui, id: egui::Id, heading: &str, text: &str, button: &str) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(id).show(ui.ctx(), |ui| {
        ui.set_width(420.0);
        ui.heading(heading);
        ui.add_space(6.0);
        ui.label(text);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let go = egui::Button::new(RichText::new(button).color(Color32::WHITE).strong()).fill(Color32::from_rgb(0xc0, 0x39, 0x2b));
            if ui.add(go).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_program_in_a_pty() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'hello \\033[31mred\\033[0m\\n'; printf '\\033[6n'; sleep 5"]);
        let t = Terminal::start(cmd, &egui::Context::default()).unwrap();
        for _ in 0..200 {
            if t.contents().contains("R") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let text = t.contents();
        assert!(text.contains("hello red"), "{text:?}");
        // The cursor position query was answered (the tty echoes the reply).
        assert!(text.contains("[2;1R"), "{text:?}");
        let p = t.parser.lock().unwrap();
        assert_eq!(p.screen().cell(0, 6).unwrap().fgcolor(), vt100::Color::Idx(1));
    }

    #[test]
    fn wheel_scrolls_back_or_reaches_the_program() {
        // `cat` echoes what the wheel sends, so we can see it on screen.
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "stty raw -echo; seq 100; cat -v"]);
        let mut t = Terminal::start(cmd, &egui::Context::default()).unwrap();
        let wait = |t: &Terminal, s: &str| {
            for _ in 0..200 {
                if t.contents().contains(s) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("{s:?} not in {:?}", t.contents());
        };
        wait(&t, "100");
        // Plain screen: the scrollback.
        t.scroll(3, (0, 0));
        assert_eq!(t.parser.lock().unwrap().screen().scrollback(), 3);
        t.scroll(-5, (0, 0));
        assert_eq!(t.parser.lock().unwrap().screen().scrollback(), 0);
        // Alternate screen: arrow keys.
        t.parser.lock().unwrap().process(b"\x1b[?1049h");
        t.scroll(1, (0, 0));
        wait(&t, "^[[A");
        // Mouse reporting (SGR): wheel events at the pointer's cell.
        t.parser.lock().unwrap().process(b"\x1b[?1000h\x1b[?1006h");
        t.scroll(-1, (4, 9));
        wait(&t, "^[[<65;10;5M");
    }

    #[test]
    fn paste_waits_for_bracketed_paste() {
        // `cat` echoes the paste once the program has turned bracketed paste on.
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "stty raw -echo; sleep 0.3; printf 'ready\\033[?2004h'; cat -v"]);
        let mut t = Terminal::start(cmd, &egui::Context::default()).unwrap();
        t.paste("explain\nthis".into());
        assert!(t.pending_paste.is_some());
        for _ in 0..300 {
            t.flush_paste();
            if t.contents().contains("201~") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        let text = t.contents();
        assert!(text.contains("ready^[[200~explain"), "{text:?}");
        assert!(text.contains("this^[[201~^M"), "{text:?}");
    }

    #[test]
    fn ssh_command_quotes_the_path() {
        let id = new_session_id();
        let cmd = command("me@host:/srv/my 'repo'", &id, None).unwrap();
        let argv: Vec<_> = cmd.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(argv[0], "ssh");
        assert!(argv.contains(&"me@host".to_string()));
        assert!(argv.last().unwrap().starts_with(r"cd '/srv/my '\''repo'\''' && exec"), "{argv:?}");
        assert!(argv.last().unwrap().contains(&format!("--resume {id}")), "{argv:?}");
        assert_eq!(quote("~/work/x"), "~/'work/x'");
        assert!(command("/tmp", "x; rm -rf ~", None).is_err());
    }

    #[test]
    fn shell_commands() {
        let argv = |tab: &TermTab, remote| {
            let cmd = tab_command(tab, None, "/home/me/repo", remote).unwrap();
            (cmd.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), cmd.get_cwd().cloned())
        };
        let (remote, _) = argv(&TermTab::new(TermKind::Remote), "me@host:/srv/repo");
        assert_eq!(*remote.last().unwrap(), format!("cd '/srv/repo' && PROMPT_COMMAND={} exec \"${{SHELL:-/bin/sh}}\" -li", quote(REPORT_CWD)));
        let (local, cwd) = argv(&TermTab::new(TermKind::Local), "me@host:/srv/repo");
        assert_eq!(local[1..], ["-li"]);
        assert_eq!(cwd.unwrap(), "/home/me/repo");
        let (claude, _) = argv(&TermTab::new(TermKind::Claude), "me@host:/srv/repo");
        assert!(claude.last().unwrap().contains("--resume"), "{claude:?}");
    }

    #[test]
    fn minimized_shows_only_the_bar() {
        let ctx = egui::Context::default();
        let mut terms = Terminals::default();
        let mut sessions = vec![TermTab::new(TermKind::Claude), TermTab::new(TermKind::Local)];
        let mut show = false;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            terminal_panel(&mut terms, &mut sessions, &mut Vec::new(), ("/tmp", "/tmp"), ui, &mut show);
        });
        out.textures_delta.clear();
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t == "claude 1") && texts.iter().any(|t| t == "local 1"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "🗗"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("click to type")), "{texts:?}");
        assert!(terms.running.is_empty());
        assert!(!show);
    }

    #[test]
    fn modifier_keys_keep_the_keyboard_selection() {
        let mut t = Terminal::start(CommandBuilder::new("cat"), &egui::Context::default()).unwrap();
        let ctx = egui::Context::default();
        let press = |t: &mut Terminal, key: Key, modifiers: Modifiers| {
            let event = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
            let input = egui::RawInput { events: vec![egui::Event::ModifiersChanged(modifiers), event], ..Default::default() };
            ctx.run_ui(input, |ui| t.input(ui)).textures_delta.clear();
        };
        t.key_anchor = Some((0, 3));
        press(&mut t, Key::ControlLeft, Modifiers::CTRL);
        press(&mut t, Key::ShiftLeft, Modifiers::SHIFT);
        assert_eq!(t.key_anchor, Some((0, 3)));
        press(&mut t, Key::Escape, Modifiers::NONE);
        assert_eq!(t.key_anchor, None);
    }

    #[test]
    fn keyboard_selection_spans_the_cells_between_anchor_and_cursor() {
        assert_eq!(between_gaps((2, 5), (2, 5), 80), None);
        assert_eq!(between_gaps((2, 5), (2, 8), 80), Some(Selection { anchor: (2, 5), head: (2, 7) }));
        assert_eq!(between_gaps((2, 5), (2, 1), 80), Some(Selection { anchor: (2, 1), head: (2, 4) }));
        assert_eq!(between_gaps((1, 78), (2, 0), 80), Some(Selection { anchor: (1, 78), head: (1, 79) }));
        assert_eq!(erase_bytes((2, 5), (2, 8), 80), vec![0x7f; 3]);
        assert_eq!(erase_bytes((2, 5), (2, 3), 80), b"\x1b[3~\x1b[3~".to_vec());
        assert_eq!(selection_move(Key::ArrowLeft, Modifiers::SHIFT | Modifiers::CTRL, false), Some(b"\x1bb".to_vec()));
        assert_eq!(selection_move(Key::ArrowRight, Modifiers::SHIFT, false), Some(b"\x1b[C".to_vec()));
        assert_eq!(selection_move(Key::ArrowRight, Modifiers::NONE, false), None);
    }

    #[test]
    fn selection_and_words() {
        let s = Selection { anchor: (3, 5), head: (1, 2) };
        assert_eq!(s.ordered(), ((1, 2), (3, 5)));
        assert_eq!(s.columns_on(0, 80), None);
        assert_eq!(s.columns_on(1, 80), Some((2, 79)));
        assert_eq!(s.columns_on(2, 80), Some((0, 79)));
        assert_eq!(s.columns_on(3, 80), Some((0, 5)));
        let line: Vec<char> = "ls  src/app/mod.rs  done".chars().collect();
        assert_eq!(word_at(&line, 8), (4, 17));
        assert_eq!(word_at(&line, 0), (0, 1));
        assert_eq!(word_at(&line, 2), (2, 2));
    }

    #[test]
    fn copy_takes_the_selection_or_interrupts() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "stty raw -echo; printf 'first line\\r\\nsecond line\\r\\n'; cat -v"]);
        let mut t = Terminal::start(cmd, &egui::Context::default()).unwrap();
        for _ in 0..200 {
            if t.contents().contains("second line") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let ctx = egui::Context::default();
        let copy = |t: &mut Terminal, shift: bool| {
            let modifiers = if shift { Modifiers::SHIFT | Modifiers::CTRL } else { Modifiers::CTRL };
            let input = egui::RawInput { events: vec![egui::Event::ModifiersChanged(modifiers), egui::Event::Copy], ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| t.input(ui));
            out.textures_delta.clear();
            out.platform_output.commands.into_iter().find_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text),
                _ => None,
            })
        };
        t.selection = Some(Selection { anchor: (0, 6), head: (1, 5) });
        assert_eq!(copy(&mut t, false).as_deref(), Some("line\nsecond"));
        assert_eq!(copy(&mut t, true).as_deref(), Some("line\nsecond"));
        assert!(t.selection.is_some());
        t.clear_selection();
        assert_eq!(copy(&mut t, true), None);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!t.contents().contains("^C"), "{:?}", t.contents());
        assert_eq!(copy(&mut t, false), None);
        for _ in 0..200 {
            if t.contents().contains("^C") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(t.contents().contains("^C"), "{:?}", t.contents());
    }

    #[test]
    fn pasted_text_is_bracketed_when_asked() {
        assert_eq!(paste_bytes("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(paste_bytes("a\nb", false), b"a\rb");
    }

    #[test]
    fn reported_cwd_across_reads() {
        let mut pending = b"ls\r\n\x1b]7;file://host/srv/my%20repo\x07$ \x1b]7;fi".to_vec();
        assert_eq!(take_reported_cwd(&mut pending).as_deref(), Some("/srv/my repo"));
        pending.extend_from_slice(b"le:///tmp/x\x1b\\");
        assert_eq!(take_reported_cwd(&mut pending).as_deref(), Some("/tmp/x"));
        pending.extend_from_slice(b"plain output\x1b");
        assert_eq!(take_reported_cwd(&mut pending), None);
        assert_eq!(cwd_of_report(b"file:///a%zz"), Some("/a%zz".into()));
    }

    #[test]
    fn output_between_shell_markers() {
        let raw = b"\r\n\x1b]133;C\x07hello\r\nworld\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
        assert_eq!(command_output(raw), b"hello\r\nworld\r\n");
        assert_eq!(command_output(b"\r\nline\r\nprompt $ "), b"line\r\n");
        assert_eq!(command_output(b"\r\nprompt $ "), b"");
        assert_eq!(rendered_text(b"\x1b[31mred\x1b[0m\r\n10%\r100%\r\n", 80), "red\n100%");
    }

    #[test]
    fn last_command_output_from_a_real_shell() {
        let mut cmd = CommandBuilder::new("bash");
        cmd.args(["--norc", "--noprofile", "-i"]);
        cmd.env("PS1", "prompt$ ");
        cmd.env("TERM", "xterm-256color");
        let mut t = Terminal::start(cmd, &egui::Context::default()).unwrap();
        let ctx = egui::Context::default();
        let wait_for = |t: &Terminal, s: &str| {
            for _ in 0..300 {
                if t.contents().contains(s) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("{s:?} not in {:?}", t.contents());
        };
        let run = |t: &mut Terminal, command: &str| {
            let enter = egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
            let input = egui::RawInput { events: vec![egui::Event::Text(command.into()), enter], ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| t.input(ui));
            out.textures_delta.clear();
        };
        wait_for(&t, "prompt$");
        assert_eq!(t.last_output(), None);
        run(&mut t, "printf 'first\\n\\033[32msecond\\033[0m\\n50%%\\r100%%\\n'; echo done-1");
        wait_for(&t, "done-1\nprompt$");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(t.last_output().as_deref(), Some("first\nsecond\n100%\ndone-1"));
        run(&mut t, "true");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(t.last_output().as_deref(), Some(""));
    }

    #[test]
    fn session_ids() {
        let (a, b) = (new_session_id(), new_session_id());
        assert!(is_session_id(&a), "{a}");
        assert_ne!(a, b);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert!(!is_session_id("not-an-id"));
    }

    #[test]
    fn conversation_names_round_trip() {
        let id = new_session_id();
        let list = vec![
            TermTab { id: id.clone(), name: String::new(), kind: TermKind::Claude },
            TermTab { id: id.clone(), name: "fix, tests\t100%".into(), kind: TermKind::Claude },
            TermTab { id: id.clone(), name: "build: release".into(), kind: TermKind::Remote },
            TermTab { id: id.clone(), name: String::new(), kind: TermKind::Local },
        ];
        let text = TermTab::list_to_string(&list);
        assert!(!text.contains('\t'), "{text}");
        assert!(TermTab::parse_list(&text) == list);
        assert!(TermTab::parse_list(&format!("{id},bad")) == list[..1]);
        assert!(text.contains(&format!(",remote/{id}:build: release,local/{id}")), "{text}");
        let names: Vec<_> = (0..list.len()).map(|i| default_name(&list, i)).collect();
        assert_eq!(names, ["claude 1", "claude 2", "remote 1", "local 1"]);
    }

    #[test]
    fn keys() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers::CTRL, false).unwrap(), b"\x1bb");
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers::CTRL | Modifiers::SHIFT, false).unwrap(), b"\x1b[1;6D");
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, false).unwrap(), vec![4]);
        assert_eq!(key_bytes(Key::Enter, Modifiers::SHIFT, false).unwrap(), b"\x1b\r");
        assert_eq!(key_bytes(Key::A, none, false), None);
    }
}
