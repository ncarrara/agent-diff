use super::*;

const FLASH: Duration = Duration::from_millis(700);
const ZOOMED_HELP: &str = "enlarged · Shift+Esc to restore";

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Pane {
    Files,
    Changes,
    Main,
    Terminal,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Shortcut {
    GoTo(Pane),
    SearchInFiles,
    Zoom,
    Rename,
    NewFile,
    NewTerminal,
    GoToDefinition,
    Back,
    QuickOpen,
    FindInFile,
}

pub(super) fn is_modifier(key: egui::Key) -> bool {
    use egui::Key;
    matches!(key, Key::ControlLeft | Key::ControlRight | Key::ShiftLeft | Key::ShiftRight | Key::AltLeft | Key::AltRight | Key::SuperLeft | Key::SuperRight)
}

pub(super) fn pane_name(pane: Pane) -> &'static str {
    match pane {
        Pane::Files => "Files",
        Pane::Changes => "Changes",
        Pane::Main => "File view",
        Pane::Terminal => "Terminals",
    }
}

#[derive(Default)]
pub(super) struct Panes {
    pub(super) current: Option<Pane>,
    pub(super) rects: Vec<(Pane, egui::Rect)>,
    pub(super) zoomed: Option<Pane>,
    flash: Option<(Pane, Instant)>,
}

impl Panes {
    pub(super) fn arrived(&mut self, pane: Pane) {
        self.current = Some(pane);
        self.zoomed = None;
        self.flash = Some((pane, Instant::now()));
    }

    pub(super) fn forget(&mut self, pane: Pane) {
        self.rects.retain(|(p, _)| *p != pane);
    }

    pub(super) fn record(&mut self, pane: Pane, rect: egui::Rect) {
        self.rects.retain(|(p, _)| *p != pane);
        self.rects.push((pane, rect));
    }

    pub(super) fn toggle_zoom(&mut self, pane: Pane) {
        self.zoomed = if self.zoomed == Some(pane) { None } else { Some(pane) };
        self.current = Some(pane);
        self.flash = Some((pane, Instant::now()));
    }

    pub(super) fn clicked_pane(&self, ctx: &egui::Context) -> Option<Pane> {
        let pos = ctx.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten())?;
        self.rects.iter().find(|(_, rect)| rect.contains(pos)).map(|(pane, _)| *pane)
    }

    fn paint_zoomed(&self, painter: &egui::Painter) {
        let Some((_, rect)) = self.zoomed.and_then(|pane| self.rects.iter().find(|(p, _)| *p == pane)) else { return };
        let galley = painter.layout_no_wrap(ZOOMED_HELP.to_string(), egui::FontId::proportional(12.0), Color32::WHITE);
        let at = egui::pos2(rect.right() - galley.size().x - 10.0, rect.top() + 6.0);
        painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(3.0), 3.0, CLAUDE.gamma_multiply(0.75));
        painter.galley(at, galley, Color32::WHITE);
    }

    pub(super) fn paint(&mut self, ctx: &egui::Context) {
        if let Some(pane) = self.clicked_pane(ctx) {
            self.current = Some(pane);
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pane-overlay")));
        self.paint_zoomed(&painter);
        if let Some((pane, at)) = self.flash {
            let left = FLASH.saturating_sub(at.elapsed());
            match self.rects.iter().find(|(p, _)| *p == pane) {
                Some((_, rect)) if !left.is_zero() => {
                    let strength = left.as_secs_f32() / FLASH.as_secs_f32();
                    painter.rect_stroke(rect.shrink(1.0), 4.0, egui::Stroke::new(2.0, CLAUDE.gamma_multiply(strength)), egui::StrokeKind::Inside);
                    ctx.request_repaint();
                }
                _ => self.flash = None,
            }
        }
    }
}

const SHORTCUTS: [(egui::Modifiers, egui::Key, Shortcut, bool); 14] = [
    (egui::Modifiers::CTRL, egui::Key::Num1, Shortcut::GoTo(Pane::Files), true),
    (egui::Modifiers::ALT.plus(egui::Modifiers::SHIFT), egui::Key::O, Shortcut::GoTo(Pane::Files), true),
    (egui::Modifiers::CTRL, egui::Key::Num2, Shortcut::GoTo(Pane::Changes), true),
    (egui::Modifiers::CTRL, egui::Key::Num0, Shortcut::GoTo(Pane::Main), true),
    (egui::Modifiers::CTRL, egui::Key::Num4, Shortcut::GoTo(Pane::Terminal), true),
    (egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT), egui::Key::F, Shortcut::SearchInFiles, true),
    (egui::Modifiers::SHIFT, egui::Key::Escape, Shortcut::Zoom, true),
    (egui::Modifiers::CTRL, egui::Key::R, Shortcut::Rename, false),
    (egui::Modifiers::CTRL, egui::Key::N, Shortcut::NewFile, false),
    (egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT), egui::Key::T, Shortcut::NewTerminal, true),
    (egui::Modifiers::CTRL, egui::Key::B, Shortcut::GoToDefinition, false),
    (egui::Modifiers::ALT, egui::Key::ArrowLeft, Shortcut::Back, false),
    (egui::Modifiers::CTRL, egui::Key::O, Shortcut::QuickOpen, false),
    (egui::Modifiers::CTRL, egui::Key::F, Shortcut::FindInFile, false),
];

pub(super) fn shortcut_pressed(ctx: &egui::Context, in_terminal: bool) -> Option<Shortcut> {
    ctx.input_mut(|i| {
        SHORTCUTS
            .iter()
            .filter(|(_, _, _, in_terminal_too)| *in_terminal_too || !in_terminal)
            .find(|(modifiers, key, _, _)| i.consume_key(*modifiers, *key))
            .map(|(_, _, shortcut, _)| *shortcut)
    })
}

pub(super) fn new_terminal_pressed(ctx: &egui::Context) -> bool {
    let (modifiers, key, _, _) = SHORTCUTS.iter().find(|(_, _, shortcut, _)| *shortcut == Shortcut::NewTerminal).unwrap();
    ctx.input_mut(|i| i.consume_key(*modifiers, *key))
}

pub(super) fn list_step(ctx: &egui::Context) -> Option<isize> {
    ctx.input_mut(|i| {
        let down = i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as isize;
        let up = i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as isize;
        (down != up).then_some(down - up)
    })
}

pub(super) fn step_in(order: &[String], selected: Option<&String>, step: isize) -> Option<String> {
    if order.is_empty() {
        return selected.cloned();
    }
    let at = match selected.and_then(|s| order.iter().position(|p| p == s)) {
        Some(at) => (at as isize + step).clamp(0, order.len() as isize - 1),
        None if step > 0 => 0,
        None => order.len() as isize - 1,
    };
    Some(order[at as usize].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_through_the_list() {
        let order: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        assert_eq!(step_in(&order, Some(&"b".to_string()), 1).as_deref(), Some("c"));
        assert_eq!(step_in(&order, Some(&"c".to_string()), 1).as_deref(), Some("c"));
        assert_eq!(step_in(&order, None, -1).as_deref(), Some("c"));
        assert_eq!(step_in(&order, Some(&"a".to_string()), -1).as_deref(), Some("a"));
    }
}
