use super::*;

#[derive(Clone, PartialEq, Debug)]
pub(super) struct Options {
    pub(super) theme: usize,
    pub(super) syntax_colors: bool,
    pub(super) full_view: bool,
    pub(super) layout: Layout,
    pub(super) git_poll: GitPoll,
}

impl Options {
    pub(super) fn load(saved: &[(String, String)]) -> Self {
        let get = |key: &str| saved.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let on = |key: &str| get(key) == Some("true");
        let seconds = |key: &str, default: u64| get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
        let default_poll = GitPoll::default();
        Options {
            theme: THEMES.iter().position(|t| get("theme") == Some(t.name)).unwrap_or(0),
            syntax_colors: get("syntax_colors") != Some("false"),
            full_view: on("full_view"),
            layout: Layout::parse(get("layout").unwrap_or(""), get("show_terminal") != Some("false")),
            git_poll: GitPoll { fetch_every: seconds("git_fetch_every", default_poll.fetch_every), log_every: seconds("git_log_every", default_poll.log_every) },
        }
    }

    pub(super) fn settings_text(&self) -> String {
        format!(
            "theme={}\nsyntax_colors={}\nfull_view={}\nlayout={}\ngit_fetch_every={}\ngit_log_every={}\n",
            THEMES[self.theme].name,
            self.syntax_colors,
            self.full_view,
            self.layout.to_text(),
            self.git_poll.fetch_every,
            self.git_poll.log_every,
        )
    }
}

#[derive(Default)]
pub(super) struct OptionsChange {
    pub(super) theme: Option<usize>,
    pub(super) syntax_colors: bool,
    pub(super) factory_reset: bool,
}

pub(super) fn options_menu(ui: &mut egui::Ui, options: &mut Options) -> OptionsChange {
    let mut change = OptionsChange::default();
    ui.menu_button("⚙ Options", |ui| {
        ui.label(RichText::new("🎨 Theme").strong());
        let mut theme = options.theme;
        for (i, t) in THEMES.iter().enumerate() {
            ui.radio_value(&mut theme, i, t.name);
        }
        if theme != options.theme {
            change.theme = Some(theme);
        }
        ui.separator();
        change.syntax_colors = ui.checkbox(&mut options.syntax_colors, "Syntax colors").changed();
        ui.separator();
        if ui.button("🗑 Factory reset…").on_hover_text("Delete all the app's settings and data (mirrors, backups, terminal history, ssh connections) and quit").clicked() {
            change.factory_reset = true;
            ui.close();
        }
    })
    .response
    .on_hover_text("Theme and display options, saved for every workspace");
    change
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<(String, String)> {
        text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn saved_and_loaded_back() {
        let options = Options {
            theme: 2,
            syntax_colors: false,
            full_view: true,
            layout: Layout { terminal_shown: false, detached: vec![Pane::Terminal], ..Layout::default() },
            git_poll: GitPoll { fetch_every: 0, log_every: 30 },
        };
        assert_eq!(Options::load(&parse(&options.settings_text())), options);
    }

    #[test]
    fn defaults_and_older_files() {
        let options = Options::load(&parse("theme=Unknown\nworkspace=a\tb\n"));
        assert_eq!(options.theme, 0);
        assert!(options.syntax_colors && !options.full_view);
        let named = Options::load(&parse(&format!("theme={}\n", THEMES[3].name)));
        assert_eq!(named.theme, 3);
        assert_eq!(options.git_poll, GitPoll::default());
    }
}
