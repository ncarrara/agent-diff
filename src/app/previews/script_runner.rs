#![cfg_attr(not(test), allow(dead_code))]

use super::*;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};

const OUTPUT_LIMIT: usize = 5000;

pub(super) fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (None, c) => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg_attr(not(feature = "python"), allow(dead_code))]
pub(super) struct Source<'a> {
    pub(super) text: &'a str,
    pub(super) path: &'a str,
    pub(super) read: &'a dyn Fn(&str) -> Option<String>,
}

pub(super) trait Form: 'static {
    fn program(&self) -> Vec<String>;
    fn arguments(&self) -> Vec<String>;
    fn environment(&self) -> Vec<(String, String)>;
    fn ui(&mut self, ui: &mut egui::Ui);
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Place {
    Local,
    Remote,
}

pub(super) struct Run {
    place: Place,
    child: Child,
    output: Arc<Mutex<Vec<(bool, String)>>>,
    started: Instant,
    pub(super) finished: Option<(Option<i32>, Duration)>,
}

pub(super) struct ScriptRunner<F: Form> {
    key: (String, u64),
    local_file: Option<String>,
    remote_file: Option<String>,
    local_root: String,
    remote: String,
    pub(super) form: F,
    pub(super) run: Option<Run>,
    error: Option<String>,
    request: Option<RunRequest>,
}

impl<F: Form> Drop for ScriptRunner<F> {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) fn strip_ansi(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else if chars.peek() == Some(&']') {
            for c in chars.by_ref() {
                if c == '\u{7}' {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

fn collect(stream: impl Read + Send + 'static, output: Arc<Mutex<Vec<(bool, String)>>>, is_error: bool, ctx: egui::Context) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let mut lines = output.lock().unwrap();
            lines.push((is_error, strip_ansi(&line)));
            if lines.len() > OUTPUT_LIMIT {
                let excess = lines.len() - OUTPUT_LIMIT;
                lines.drain(..excess);
            }
            drop(lines);
            ctx.request_repaint();
        }
    });
}

impl<F: Form> ScriptRunner<F> {
    pub(super) fn load(e: &FileEntry, place: &FilePlace, parse: fn(&Source) -> F) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.clone(), r.remote_path.clone()),
            None => (e.path.clone(), e.path.clone()),
        };
        let script = match &e.body {
            Body::Text(texts) => if e.local_exists { texts.diff().local.clone() } else { texts.diff().remote.clone() },
            _ => std::fs::read_to_string(Path::new(place.local_root).join(&local_path)).unwrap_or_default(),
        };
        let script_path = if e.local_exists { &local_path } else { &remote_path };
        let local_root = Path::new(place.local_root);
        let roots = if e.local_exists { [local_root, place.mirror] } else { [place.mirror, local_root] };
        let read = |rel: &str| roots.iter().find_map(|root| std::fs::read_to_string(root.join(rel)).ok());
        let form = parse(&Source { text: &script, path: script_path, read: &read });
        ScriptRunner {
            key: (e.path.clone(), e.version),
            local_file: e.local_exists.then_some(local_path),
            remote_file: e.remote_exists.then_some(remote_path),
            local_root: place.local_root.to_string(),
            remote: place.remote.to_string(),
            form,
            run: None,
            error: None,
            request: None,
        }
    }

    fn remote_root(&self) -> String {
        match crate::remote::Remote::parse(&self.remote) {
            Ok(crate::remote::Remote::Ssh { path, .. }) => path,
            Ok(crate::remote::Remote::Local(dir)) => dir.display().to_string(),
            Err(_) => self.remote.clone(),
        }
    }

    fn request_for(&self, remote: bool) -> Option<RunRequest> {
        let (file, root) = if remote { (self.remote_file.as_ref()?, self.remote_root()) } else { (self.local_file.as_ref()?, self.local_root.clone()) };
        Some(RunRequest { remote, command: format!("cd {} && {}", quote(&root), self.command_line(file)) })
    }

    fn command_line(&self, file: &str) -> String {
        let env: Vec<String> = self.form.environment().iter().map(|(k, v)| format!("{k}={}", quote(v))).collect();
        let program: Vec<String> = self.form.program().iter().map(|p| quote(p)).collect();
        let args: Vec<String> = self.form.arguments().iter().map(|a| quote(a)).collect();
        [env.join(" "), program.join(" "), quote(file), args.join(" ")].iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" ")
    }

    fn spawn(&self, place: Place) -> Result<Child, String> {
        let file = match place {
            Place::Local => self.local_file.as_ref(),
            Place::Remote => self.remote_file.as_ref(),
        }
        .ok_or("the script does not exist on this side")?;
        let program = self.form.program();
        let (interpreter, interpreter_args) = program.split_first().ok_or("no interpreter to run the script with")?;
        let direct = |dir: &Path| {
            let mut cmd = Command::new(interpreter);
            cmd.args(interpreter_args).arg(file).args(self.form.arguments()).envs(self.form.environment()).current_dir(dir);
            cmd
        };
        let mut cmd = match (place, crate::remote::Remote::parse(&self.remote).map_err(|e| e.to_string())?) {
            (Place::Local, _) => direct(Path::new(&self.local_root)),
            (Place::Remote, crate::remote::Remote::Local(dir)) => direct(&dir),
            (Place::Remote, crate::remote::Remote::Ssh { host, path }) => {
                let mut cmd = Command::new("ssh");
                cmd.args(["-tt", "-o", "BatchMode=yes"]);
                cmd.args(crate::remote::SHARED_CONNECTION);
                cmd.arg(host).arg(format!("cd {} && {}", quote(&path), self.command_line(file)));
                cmd
            }
        };
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.spawn().map_err(|e| format!("cannot start the script: {e}"))
    }

    pub(super) fn start(&mut self, place: Place, ctx: &egui::Context) {
        self.stop();
        self.error = None;
        match self.spawn(place) {
            Ok(mut child) => {
                let output = Arc::new(Mutex::new(Vec::new()));
                if let Some(out) = child.stdout.take() {
                    collect(out, output.clone(), false, ctx.clone());
                }
                if let Some(err) = child.stderr.take() {
                    collect(err, output.clone(), true, ctx.clone());
                }
                self.run = Some(Run { place, child, output, started: Instant::now(), finished: None });
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn stop(&mut self) {
        if let Some(run) = &mut self.run
            && run.finished.is_none()
        {
            let _ = run.child.kill();
            let _ = run.child.wait();
            run.finished = Some((None, run.started.elapsed()));
        }
    }

    pub(super) fn poll(&mut self) {
        if let Some(run) = &mut self.run
            && run.finished.is_none()
            && let Ok(Some(status)) = run.child.try_wait()
        {
            run.finished = Some((status.code(), run.started.elapsed()));
        }
    }

    pub(super) fn output_text(&self) -> String {
        self.run.as_ref().map(|r| r.output.lock().unwrap().iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
    }
}

impl<F: Form> Preview for ScriptRunner<F> {
    fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        runner_ui(ui, self);
    }

    fn run_request(&mut self) -> Option<RunRequest> {
        self.request.take()
    }
}

fn runner_ui<F: Form>(ui: &mut egui::Ui, runner: &mut ScriptRunner<F>) {
    let shown = runner.local_file.as_ref().or(runner.remote_file.as_ref()).map(|f| runner.command_line(f)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        let local = ui.add_enabled(runner.local_file.is_some(), egui::Button::new(RichText::new("▶ Run local").color(YOU)));
        if local.on_hover_text(format!("Run your version in a local shell tab, in {}", runner.local_root)).clicked() {
            runner.request = runner.request_for(false);
        }
        let remote = ui.add_enabled(runner.remote_file.is_some(), egui::Button::new(RichText::new("▶ Run on remote").color(CLAUDE)));
        if remote.on_hover_text(format!("Run Claude's version in a remote shell tab, in {}", runner.remote)).clicked() {
            runner.request = runner.request_for(true);
        }
        ui.label(RichText::new(format!("$ {shown}")).monospace().weak().small());
    });
    egui::CollapsingHeader::new("⚙ Arguments").id_salt(("script-form", &runner.key.0)).default_open(false).show(ui, |ui| runner.form.ui(ui));
}
