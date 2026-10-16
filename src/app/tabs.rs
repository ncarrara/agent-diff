use super::*;

pub(super) struct TabOutput {
    pub(super) response: egui::Response,
    pub(super) close: bool,
}

pub(super) fn closable_tab(ui: &mut egui::Ui, selected: bool, contents: impl FnOnce(&mut egui::Ui), close_help: Option<&str>) -> TabOutput {
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
        egui::Frame::NONE
            .inner_margin(egui::Margin { left: 6, right: 3, top: 3, bottom: 3 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    contents(ui);
                    close_help.is_some_and(|help| ui.add(egui::Button::new("×").frame(false).small()).on_hover_text(help).clicked())
                })
                .inner
            })
            .inner
    });
    let response = inner.response;
    let rect = response.rect;
    let visuals = ui.visuals();
    let fill = if selected {
        Some(visuals.selection.bg_fill)
    } else if ui.rect_contains_pointer(rect) {
        Some(visuals.widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(background, egui::Shape::rect_filled(rect, 4.0, fill));
    }
    TabOutput { response, close: inner.inner }
}

#[derive(Clone, Copy)]
struct TabDrag {
    bar: egui::Id,
    from: usize,
}

pub(super) fn dragged_onto(ui: &egui::Ui, bar: egui::Id, index: usize, response: &egui::Response) -> Option<(usize, usize)> {
    response.dnd_set_drag_payload(TabDrag { bar, from: index });
    let ours = |drag: &TabDrag| drag.bar == bar && drag.from != index;
    if let Some(drag) = response.dnd_hover_payload::<TabDrag>().filter(|d| ours(d)) {
        let rect = response.rect;
        let x = if drag.from < index { rect.right() } else { rect.left() };
        ui.painter().vline(x, rect.y_range(), egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
    }
    let dropped = response.dnd_release_payload::<TabDrag>().filter(|d| ours(d))?;
    Some((dropped.from, index))
}

pub(super) fn move_tab<T>(items: &mut Vec<T>, (from, to): (usize, usize)) {
    if from < items.len() && to < items.len() {
        let item = items.remove(from);
        items.insert(to, item);
    }
}

pub(super) fn index_after_move(index: usize, (from, to): (usize, usize)) -> usize {
    match index {
        i if i == from => to,
        i if from < i && i <= to => i - 1,
        i if to <= i && i < from => i + 1,
        i => i,
    }
}

pub(super) fn tab_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_at(pos: egui::Pos2) -> (bool, bool) {
        let ctx = egui::Context::default();
        let (mut clicked, mut closed) = (false, false);
        let press = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        for events in [vec![], vec![egui::Event::PointerMoved(pos)], vec![press(true)], vec![press(false)]] {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 100.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                ui.horizontal(|ui| {
                    let tab = closable_tab(ui, true, |ui| tab_label(ui, "workspace one"), Some("Close"));
                    clicked |= tab.response.clicked();
                    closed |= tab.close;
                });
            });
            out.textures_delta.clear();
        }
        (clicked, closed)
    }

    #[test]
    fn moving_a_tab_keeps_the_others_in_order() {
        let mut tabs = vec!['a', 'b', 'c', 'd'];
        move_tab(&mut tabs, (0, 2));
        assert_eq!(tabs, ['b', 'c', 'a', 'd']);
        assert_eq!([0, 1, 2, 3].map(|i| index_after_move(i, (0, 2))), [2, 0, 1, 3]);
        move_tab(&mut tabs, (3, 0));
        assert_eq!(tabs, ['d', 'b', 'c', 'a']);
        assert_eq!([0, 1, 2, 3].map(|i| index_after_move(i, (3, 0))), [1, 2, 3, 0]);
    }

    #[test]
    fn label_selects_and_cross_closes() {
        assert_eq!(click_at(egui::pos2(20.0, 8.0)), (true, false));
        let ctx = egui::Context::default();
        let mut cross = egui::Rect::NOTHING;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.horizontal(|ui| {
                closable_tab(ui, true, |ui| tab_label(ui, "workspace one"), Some("Close"));
                cross = ui.min_rect();
            });
        });
        out.textures_delta.clear();
        assert_eq!(click_at(egui::pos2(cross.right() - 10.0, cross.center().y)), (false, true));
    }

    fn tab_rect_and_clicks(points: &[egui::Pos2]) -> (egui::Rect, Vec<bool>) {
        let ctx = egui::Context::default();
        let mut rect = egui::Rect::NOTHING;
        let mut results = Vec::new();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 100.0)));
        let mut frame = |events: Vec<egui::Event>| {
            let mut clicked = false;
            let input = egui::RawInput { screen_rect: screen, events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::top("tabs").show(ui, |ui| {
                    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let tab = closable_tab(ui, false, |ui| tab_label(ui, "my workspace"), Some("Close"));
                            rect = tab.response.rect;
                            clicked |= tab.response.clicked();
                        });
                    });
                });
            });
            out.textures_delta.clear();
            clicked
        };
        frame(vec![]);
        frame(vec![]);
        for &pos in points {
            let press = |pressed, at| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            let moved = pos + egui::vec2(3.0, 2.0);
            frame(vec![egui::Event::PointerMoved(pos)]);
            let a = frame(vec![press(true, pos)]);
            let a2 = frame(vec![egui::Event::PointerMoved(moved)]);
            let b = frame(vec![press(false, moved)]) || a2;
            let c = frame(vec![]);
            results.push(a || b || c);
            frame(vec![egui::Event::PointerMoved(egui::pos2(590.0, 90.0))]);
            for _ in 0..30 {
                frame(vec![]);
            }
        }
        (rect, results)
    }

    #[test]
    fn the_whole_tab_is_clickable() {
        let (rect, _) = tab_rect_and_clicks(&[]);
        let points = [
            egui::pos2(rect.left() + 1.0, rect.center().y),
            egui::pos2(rect.center().x, rect.top() + 1.0),
            egui::pos2(rect.center().x, rect.bottom() - 1.0),
            egui::pos2(rect.left() + 20.0, rect.center().y),
        ];
        let (_, clicks) = tab_rect_and_clicks(&points);
        assert_eq!(clicks, [true; 4], "{rect:?}");
    }
}
