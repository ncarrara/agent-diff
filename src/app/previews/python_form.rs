use super::script_runner::{Form, Source, split_words};
use super::*;
use std::path::Path;

pub(super) fn is_python(e: &FileEntry) -> bool {
    let Body::Text(texts) = &e.body else { return false };
    let TextDiff { local, remote, .. } = texts.diff();
    let is_py = Path::new(&e.path).extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("py"));
    [local, remote].iter().any(|text| {
        let python_shebang = text.lines().next().is_some_and(|l| l.starts_with("#!") && l.contains("python"));
        (is_py || python_shebang) && runs_as_main(text)
    })
}

fn runs_as_main(text: &str) -> bool {
    text.lines().any(|line| line.trim_start().starts_with("if __name__") && line.contains("__main__"))
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct PythonForm {
    python: String,
    arguments: String,
    environment: String,
}

pub(super) fn parse_python(_: &Source) -> PythonForm {
    PythonForm { python: "python3".to_string(), arguments: String::new(), environment: String::new() }
}

fn environment_pairs(text: &str) -> Vec<(String, String)> {
    split_words(text).into_iter().filter_map(|word| word.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).filter(|(k, _)| !k.is_empty()).collect()
}

impl Form for PythonForm {
    fn program(&self) -> Vec<String> {
        split_words(&self.python)
    }

    fn arguments(&self) -> Vec<String> {
        split_words(&self.arguments)
    }

    fn environment(&self) -> Vec<(String, String)> {
        environment_pairs(&self.environment)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("python-form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("python");
            ui.add(egui::TextEdit::singleline(&mut self.python).desired_width(220.0).hint_text("python3")).on_hover_text("The interpreter, e.g. .venv/bin/python or uv run python");
            ui.end_row();
            ui.label("arguments");
            ui.add(egui::TextEdit::singleline(&mut self.arguments).desired_width(220.0).hint_text("--verbose \"a b\""));
            ui.end_row();
            ui.label("environment");
            ui.add(egui::TextEdit::singleline(&mut self.environment).desired_width(220.0).hint_text("DEBUG=1 NAME=\"a b\""));
            ui.end_row();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_is_read_from_assignments() {
        assert_eq!(environment_pairs("DEBUG=1 NAME=\"a b\" stray"), [("DEBUG".to_string(), "1".to_string()), ("NAME".to_string(), "a b".to_string())]);
    }
}
