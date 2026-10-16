use super::*;

pub(super) const REPO: Color32 = Color32::from_rgb(0x4f, 0xa8, 0xd8);

pub(super) const THEMES: &[ThemeDef] = &[
    ThemeDef { name: "Ocean Dark", syntax: "base16-ocean.dark", dark: true },
    ThemeDef { name: "Eighties Dark", syntax: "base16-eighties.dark", dark: true },
    ThemeDef { name: "Mocha Dark", syntax: "base16-mocha.dark", dark: true },
    ThemeDef { name: "Solarized Dark", syntax: "Solarized (dark)", dark: true },
    ThemeDef { name: "Ocean Light", syntax: "base16-ocean.light", dark: false },
    ThemeDef { name: "GitHub Light", syntax: "InspiredGitHub", dark: false },
    ThemeDef { name: "Solarized Light", syntax: "Solarized (light)", dark: false },
];

/// Diff colors for the current theme, stored in egui's memory so every
/// drawing function can read them.
#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) red: Color32,
    pub(super) green: Color32,
    /// Text drawn on top of `red`/`green`.
    pub(super) on_diff: Color32,
    pub(super) gutter: Color32,
}

pub(super) fn palette(ctx: &egui::Context) -> Palette {
    ctx.data(|d| d.get_temp(egui::Id::NULL)).unwrap_or_else(|| palette_for(true))
}

pub(super) fn palette_for(dark: bool) -> Palette {
    if dark {
        Palette {
            red: Color32::from_rgb(0x5a, 0x1e, 0x1e),
            green: Color32::from_rgb(0x1b, 0x4a, 0x26),
            on_diff: Color32::WHITE,
            gutter: Color32::from_gray(110),
        }
    } else {
        Palette {
            red: Color32::from_rgb(0xff, 0xd7, 0xd5),
            green: Color32::from_rgb(0xcc, 0xf2, 0xd0),
            on_diff: Color32::BLACK,
            gutter: Color32::from_gray(140),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn themes_exist() {
        let set = syntect::highlighting::ThemeSet::load_defaults();
        for t in super::THEMES {
            assert!(set.themes.contains_key(t.syntax), "{}", t.syntax);
        }
    }
}



