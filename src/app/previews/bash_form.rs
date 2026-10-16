use super::script_runner::{Form, split_words};
use super::*;
use std::path::Path;

pub(super) fn is_bash(e: &FileEntry) -> bool {
    let extension = Path::new(&e.path).extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase);
    let first_line = match &e.body {
        Body::Text(texts) => texts.diff().local.lines().next().or_else(|| texts.diff().remote.lines().next()).unwrap_or("").to_string(),
        _ => String::new(),
    };
    matches!(extension.as_deref(), Some("sh" | "bash")) || (first_line.starts_with("#!") && (first_line.contains("bash") || first_line.ends_with("/sh")))
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Field {
    pub(super) name: String,
    pub(super) value: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Flag {
    pub(super) letter: char,
    pub(super) takes_value: bool,
    pub(super) on: bool,
    pub(super) value: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct ScriptForm {
    pub(super) usage: Option<String>,
    pub(super) flags: Vec<Flag>,
    pub(super) positionals: Vec<Field>,
    pub(super) env: Vec<Field>,
    pub(super) extra: String,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn usage_of(script: &str) -> Option<String> {
    script.lines().filter_map(|l| l.trim_start().strip_prefix('#')).find_map(|comment| {
        let lower = comment.to_ascii_lowercase();
        let at = lower.find("usage:")?;
        Some(comment[at + "usage:".len()..].trim().to_string()).filter(|u| !u.is_empty())
    })
}

fn getopts_flags(script: &str) -> Vec<Flag> {
    let Some(at) = script.find("getopts") else { return Vec::new() };
    let spec: String = script[at + "getopts".len()..].trim_start().trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':').collect();
    let spec = spec.trim_start_matches(':');
    let chars: Vec<char> = spec.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_ascii_alphanumeric())
        .map(|(i, &letter)| Flag { letter, takes_value: chars.get(i + 1) == Some(&':'), ..Flag::default() })
        .collect()
}

fn expansions(script: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let name: String = rest.chars().take_while(|&c| is_name_char(c)).collect();
        let after = &rest[name.len()..];
        let default = [":-", ":=", "-", "="].iter().find_map(|op| after.strip_prefix(op)).map(|d| d.chars().take_while(|&c| c != '}').collect::<String>());
        if !name.is_empty() {
            found.push((name, default.map(|d| d.trim_matches(['"', '\'']).to_string())));
        }
    }
    let mut plain = script;
    while let Some(at) = plain.find('$') {
        plain = &plain[at + 1..];
        if let Some(digit) = plain.chars().next().filter(|c| c.is_ascii_digit() && *c != '0') {
            found.push((digit.to_string(), None));
        }
    }
    found
}

fn positional_names(script: &str) -> Vec<(usize, String)> {
    let mut names = Vec::new();
    for line in script.lines() {
        let line = line.trim().trim_start_matches("local ").trim_start_matches("readonly ").trim_start_matches("declare ");
        let Some((name, value)) = line.split_once('=') else { continue };
        if name.is_empty() || !name.chars().all(is_name_char) {
            continue;
        }
        let value = value.trim_matches(['"', '\'']);
        let digit = value.strip_prefix("${").or_else(|| value.strip_prefix('$')).and_then(|v| v.chars().next()).filter(|c| c.is_ascii_digit() && *c != '0');
        if let Some(digit) = digit {
            names.push((digit.to_digit(10).unwrap() as usize, name.to_string()));
        }
    }
    names
}

pub(super) fn parse_bash(script: &str) -> ScriptForm {
    let expansions = expansions(script);
    let names = positional_names(script);
    let count = expansions.iter().filter_map(|(n, _)| n.parse::<usize>().ok()).max().unwrap_or(0);
    let positionals = (1..=count)
        .map(|n| {
            let name = names.iter().find(|(i, _)| *i == n).map_or(format!("arg {n}"), |(_, name)| name.clone());
            let value = expansions.iter().find(|(name, d)| name == &n.to_string() && d.is_some()).and_then(|(_, d)| d.clone()).unwrap_or_default();
            Field { name, value }
        })
        .collect();
    let mut env: Vec<Field> = Vec::new();
    for (name, default) in &expansions {
        let is_setting = name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if let Some(default) = default.as_ref().filter(|_| is_setting)
            && !env.iter().any(|f| &f.name == name)
        {
            env.push(Field { name: name.clone(), value: default.clone() });
        }
    }
    ScriptForm { usage: usage_of(script), flags: getopts_flags(script), positionals, env, extra: String::new() }
}

impl Form for ScriptForm {
    fn program(&self) -> Vec<String> {
        vec!["bash".to_string()]
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = Vec::new();
        for flag in self.flags.iter().filter(|f| f.on) {
            args.push(format!("-{}", flag.letter));
            if flag.takes_value {
                args.push(flag.value.clone());
            }
        }
        let filled = self.positionals.iter().rposition(|f| !f.value.is_empty()).map_or(0, |i| i + 1);
        args.extend(self.positionals[..filled].iter().map(|f| f.value.clone()));
        args.extend(split_words(&self.extra));
        args
    }

    fn environment(&self) -> Vec<(String, String)> {
        self.env.iter().filter(|f| !f.value.is_empty()).map(|f| (f.name.clone(), f.value.clone())).collect()
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(usage) = &self.usage {
            ui.weak(format!("Usage: {usage}"));
        }
        form_ui(ui, self);
    }
}

fn form_ui(ui: &mut egui::Ui, form: &mut ScriptForm) {
    egui::Grid::new("script-form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        for flag in &mut form.flags {
            ui.checkbox(&mut flag.on, RichText::new(format!("-{}", flag.letter)).monospace());
            if flag.takes_value {
                ui.add_enabled(flag.on, egui::TextEdit::singleline(&mut flag.value).desired_width(220.0).hint_text("value"));
            } else {
                ui.label("");
            }
            ui.end_row();
        }
        for field in &mut form.positionals {
            ui.label(&field.name);
            ui.add(egui::TextEdit::singleline(&mut field.value).desired_width(220.0));
            ui.end_row();
        }
        for field in &mut form.env {
            ui.label(RichText::new(&field.name).monospace()).on_hover_text("environment variable");
            ui.add(egui::TextEdit::singleline(&mut field.value).desired_width(220.0));
            ui.end_row();
        }
        ui.label("extra arguments");
        ui.add(egui::TextEdit::singleline(&mut form.extra).desired_width(220.0).hint_text("--verbose \"a b\""));
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::super::script_runner::{Place, ScriptRunner, strip_ansi};
    use super::*;

    const SCRIPT: &str = r#"#!/usr/bin/env bash
# Deploy the site.
# Usage: deploy.sh [-v] [-e env] target [version]
set -euo pipefail
while getopts "ve:" opt; do
  case $opt in v) VERBOSE=1;; e) ENV="$OPTARG";; esac
done
shift $((OPTIND - 1))
target="$1"
version="${2:-latest}"
REGION="${REGION:-eu-west-1}"
echo "deploying $target $version to $REGION"
"#;

    #[test]
    fn form_from_a_bash_script() {
        let form = parse_bash(SCRIPT);
        assert_eq!(form.usage.as_deref(), Some("deploy.sh [-v] [-e env] target [version]"));
        assert_eq!(form.flags.iter().map(|f| (f.letter, f.takes_value)).collect::<Vec<_>>(), [('v', false), ('e', true)]);
        assert_eq!(form.positionals, [Field { name: "target".into(), value: String::new() }, Field { name: "version".into(), value: "latest".into() }]);
        assert_eq!(form.env, [Field { name: "REGION".into(), value: "eu-west-1".into() }]);
    }

    #[test]
    fn arguments_in_order() {
        let mut form = parse_bash(SCRIPT);
        form.flags[1].on = true;
        form.flags[1].value = "prod".into();
        form.positionals[0].value = "site".into();
        form.extra = "--dry-run 'two words'".into();
        assert_eq!(form.arguments(), ["-e", "prod", "site", "latest", "--dry-run", "two words"]);
        form.positionals[1].value.clear();
        assert_eq!(form.arguments(), ["-e", "prod", "site", "--dry-run", "two words"]);
        assert_eq!(split_words(r#"a "b c" '' d"#), ["a", "b c", "", "d"]);
    }

    #[test]
    fn colors_are_removed_from_the_output() {
        assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m: x\r"), "error: x");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}done"), "done");
    }

    #[test]
    fn runs_the_local_script_with_its_form() {
        let root = std::env::temp_dir().join(format!("agent-diff-runner-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(root.join("tools/deploy.sh"), SCRIPT).unwrap();
        let entry = FileEntry {
            path: "tools/deploy.sh".into(),
            rename: None,
            origin: Origin::Synced,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Text(Texts::new(SCRIPT, SCRIPT)),
            version: 1,
        };
        assert!(is_bash(&entry));
        let local_root = root.to_str().unwrap().to_string();
        let mirror = root.join("mirror");
        let place = FilePlace { local_root: &local_root, mirror: &mirror, remote: &local_root };
        let mut runner = ScriptRunner::load(&entry, &place, |source| parse_bash(source.text));
        runner.form.positionals[0].value = "site".into();
        runner.form.env[0].value = "us-east-2".into();
        runner.start(Place::Local, &egui::Context::default());
        for _ in 0..300 {
            runner.poll();
            if runner.run.as_ref().is_some_and(|r| r.finished.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(runner.run.as_ref().unwrap().finished.unwrap().0, Some(0));
        assert_eq!(runner.output_text(), "deploying site latest to us-east-2");
        runner.form.positionals[0].value.clear();
        runner.form.positionals[1].value.clear();
        assert!(runner.form.arguments().is_empty());
        runner.start(Place::Local, &egui::Context::default());
        for _ in 0..300 {
            runner.poll();
            if runner.run.as_ref().is_some_and(|r| r.finished.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_ne!(runner.run.as_ref().unwrap().finished.unwrap().0, Some(0));
        assert!(runner.output_text().contains("unbound variable"), "{}", runner.output_text());
        let _ = std::fs::remove_dir_all(root);
    }
}
