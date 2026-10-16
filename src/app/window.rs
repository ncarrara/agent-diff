use super::*;
use egui::{CursorIcon, Pos2, Rect, ResizeDirection, ViewportCommand};

const EDGE: f32 = 6.0;
pub(super) const BUTTONS_WIDTH: f32 = 96.0;

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

pub(super) fn title_bar(ctx: &egui::Context, bar: &egui::Response) {
    if bar.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(ctx)));
    } else if bar.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

pub(super) fn window_buttons(ui: &mut egui::Ui) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let button = |text: &str| egui::Button::new(RichText::new(text).size(15.0)).frame(false).min_size(egui::vec2(28.0, 24.0));
        let close = ui.add(button("🗙")).on_hover_text("Close");
        if close.hovered() {
            ui.painter().rect_filled(close.rect, 3.0, Color32::from_rgb(0xc0, 0x39, 0x2b).gamma_multiply(0.6));
        }
        if close.clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
        let is_max = maximized(ui.ctx());
        let (icon, help) = if is_max { ("🗗", "Restore") } else { ("🗖", "Maximize") };
        if ui.add(button(icon)).on_hover_text(help).clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!is_max));
        }
        if ui.add(button("🗕")).on_hover_text("Minimize").clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    });
}

pub(super) fn resize_edges(ctx: &egui::Context) {
    if maximized(ctx) {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
    let Some(direction) = edge_at(ctx.viewport_rect(), pos) else { return };
    ctx.set_cursor_icon(cursor_for(direction));
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

fn edge_at(window: Rect, pos: Pos2) -> Option<ResizeDirection> {
    if !window.contains(pos) {
        return None;
    }
    let west = pos.x - window.left() < EDGE;
    let east = window.right() - pos.x < EDGE;
    let north = pos.y - window.top() < EDGE;
    let south = window.bottom() - pos.y < EDGE;
    Some(match (north, south, west, east) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (true, _, _, true) => ResizeDirection::NorthEast,
        (_, true, true, _) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, _, _, _) => ResizeDirection::North,
        (_, true, _, _) => ResizeDirection::South,
        (_, _, true, _) => ResizeDirection::West,
        (_, _, _, true) => ResizeDirection::East,
        _ => return None,
    })
}

fn cursor_for(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

#[derive(Default)]
pub(super) struct QuitGuard {
    asking: bool,
    when_done: bool,
    allowed: bool,
}

pub(super) fn guard_quit(ctx: &egui::Context, guard: &mut QuitGuard, running: &[(String, String)]) {
    if running.is_empty() && guard.when_done {
        guard.allowed = true;
        guard.asking = false;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    if ctx.input(|i| i.viewport().close_requested()) && !guard.allowed && !running.is_empty() {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        guard.asking = true;
    }
    if running.is_empty() && !guard.when_done {
        guard.asking = false;
    }
    if guard.asking {
        quit_dialog(ctx, guard, running);
    }
}

fn quit_dialog(ctx: &egui::Context, guard: &mut QuitGuard, running: &[(String, String)]) {
    let modal = egui::Modal::new(egui::Id::new("confirm-quit")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("Quit while syncing?");
        ui.add_space(6.0);
        ui.label("Still running:");
        for (workspace, step) in running {
            ui.horizontal(|ui| {
                ui.label(RichText::new(workspace).strong());
                ui.weak(step);
            });
        }
        ui.add_space(4.0);
        ui.label("Quitting now stops it midway: some files may be applied and others not, or not pushed yet.");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let red = egui::Button::new(RichText::new("Quit now").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0xc0, 0x39, 0x2b));
            if ui.add(red).clicked() {
                guard.allowed = true;
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
            if ui.button("Quit when finished").clicked() {
                guard.when_done = true;
            }
            if ui.button("Keep working").clicked() {
                guard.asking = false;
                guard.when_done = false;
            }
        });
        if guard.when_done {
            ui.add_space(4.0);
            ui.weak("The app closes by itself once this is done.");
        }
    });
    if modal.should_close() && !guard.allowed {
        guard.asking = false;
        guard.when_done = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands_after(clicks: &[(egui::Pos2, bool)]) -> Vec<ViewportCommand> {
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let mut frames: Vec<Vec<egui::Event>> = vec![vec![]];
        for &(pos, pressed) in clicks {
            frames.push(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
            ]);
        }
        for events in frames {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::top("tabs").show(ui, |ui| {
                    let bar = ui
                        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let _ = ui.button("tab");
                            });
                        })
                        .response;
                    title_bar(ui.ctx(), &bar);
                });
            });
            out.textures_delta.clear();
            commands.extend(out.viewport_output.remove(&egui::ViewportId::ROOT).map(|v| v.commands).unwrap_or_default());
        }
        commands
    }

    #[test]
    fn bar_double_click_maximizes_and_drag_moves() {
        let empty = egui::pos2(500.0, 8.0);
        let clicks = [(empty, true), (empty, false), (empty, true), (empty, false)];
        assert!(commands_after(&clicks).contains(&ViewportCommand::Maximized(true)));
        let dragged = [(empty, true), (egui::pos2(560.0, 30.0), true)];
        assert!(commands_after(&dragged).contains(&ViewportCommand::StartDrag));
        let tab = egui::pos2(20.0, 10.0);
        let on_tab = [(tab, true), (tab, false), (tab, true), (tab, false)];
        let commands = commands_after(&on_tab);
        assert!(!commands.contains(&ViewportCommand::Maximized(true)) && !commands.contains(&ViewportCommand::StartDrag), "{commands:?}");
    }

    #[test]
    fn bar_keeps_its_height_with_window_buttons() {
        let ctx = egui::Context::default();
        let mut height = 0.0;
        for _ in 0..10 {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                height = egui::Panel::top("tabs")
                    .show(ui, |ui| {
                        ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add_space(4.0);
                            ui.horizontal_top(|ui| {
                                let wrap = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                                ui.allocate_ui_with_layout(egui::vec2(ui.available_width() - BUTTONS_WIDTH, 0.0), wrap, |ui| {
                                    let _ = ui.button("tab");
                                    egui::ComboBox::from_id_salt("theme").selected_text("theme").show_ui(ui, |_| {});
                                });
                                window_buttons(ui);
                            });
                            ui.add_space(4.0);
                        });
                    })
                    .response
                    .rect
                    .height();
            });
            out.textures_delta.clear();
        }
        assert!(height < 60.0, "{height}");
    }

    fn quit_frame(ctx: &egui::Context, guard: &mut QuitGuard, running: &[(String, String)], close: bool) -> Vec<ViewportCommand> {
        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() };
        if close {
            input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        }
        let mut out = ctx.run_ui(input, |ui| guard_quit(ui.ctx(), guard, running));
        out.textures_delta.clear();
        out.viewport_output.remove(&egui::ViewportId::ROOT).map(|v| v.commands).unwrap_or_default()
    }

    #[test]
    fn quitting_while_syncing_asks_first() {
        let ctx = egui::Context::default();
        let running = vec![("repo ⇄ devbox".to_string(), "pushing 3 file(s) · 70%".to_string())];

        let mut idle = QuitGuard::default();
        assert!(!quit_frame(&ctx, &mut idle, &[], true).contains(&ViewportCommand::CancelClose));

        let mut guard = QuitGuard::default();
        assert!(quit_frame(&ctx, &mut guard, &running, true).contains(&ViewportCommand::CancelClose));
        assert!(guard.asking && !guard.allowed);
        quit_frame(&ctx, &mut guard, &running, false);
        assert!(guard.asking);

        guard.when_done = true;
        assert!(!quit_frame(&ctx, &mut guard, &running, false).contains(&ViewportCommand::Close));
        let commands = quit_frame(&ctx, &mut guard, &[], false);
        assert!(commands.contains(&ViewportCommand::Close) && guard.allowed, "{commands:?}");
        assert!(!quit_frame(&ctx, &mut guard, &[], true).contains(&ViewportCommand::CancelClose));
    }

    #[test]
    fn the_question_goes_away_when_the_work_ends() {
        let ctx = egui::Context::default();
        let running = vec![("w".to_string(), "applying 1/2 · 30%".to_string())];
        let mut guard = QuitGuard::default();
        quit_frame(&ctx, &mut guard, &running, true);
        assert!(guard.asking);
        quit_frame(&ctx, &mut guard, &[], false);
        assert!(!guard.asking && !guard.allowed);
    }

    #[test]
    fn edges_and_corners() {
        let window = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let at = |x, y| edge_at(window, egui::pos2(x, y));
        assert_eq!(at(400.0, 300.0), None);
        assert_eq!(at(2.0, 300.0), Some(ResizeDirection::West));
        assert_eq!(at(798.0, 300.0), Some(ResizeDirection::East));
        assert_eq!(at(400.0, 1.0), Some(ResizeDirection::North));
        assert_eq!(at(400.0, 599.0), Some(ResizeDirection::South));
        assert_eq!(at(1.0, 1.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(799.0, 599.0), Some(ResizeDirection::SouthEast));
        assert_eq!(at(900.0, 300.0), None);
    }
}

#[cfg(test)]
mod tab_bar_tests {
    use super::*;

    #[test]
    fn every_visible_part_of_a_tab_is_clickable() {
        let ctx = egui::Context::default();
        let screen = Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 600.0)));
        let tab_rect = std::cell::Cell::new(Rect::NOTHING);
        let frame = |events: Vec<egui::Event>| {
            let mut clicked = false;
            let input = egui::RawInput { screen_rect: screen, events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::top("tabs").show(ui, |ui| {
                    let bar = ui
                        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click_and_drag()), |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add_space(6.0);
                            ui.horizontal_top(|ui| {
                                let width = (ui.available_width() - BUTTONS_WIDTH).max(0.0);
                                let wrap = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                                ui.allocate_ui_with_layout(egui::vec2(width, 0.0), wrap, |ui| {
                                    let tab = closable_tab(ui, false, |ui| tab_label(ui, RichText::new("repo ⇄ devbox (3)").strong()), Some("Close"));
                                    tab_rect.set(tab.response.rect);
                                    clicked |= tab.response.clicked();
                                    let _ = ui.selectable_label(false, "➕ Workspace");
                                    ui.separator();
                                    let _ = ui.menu_button("⚙ Options", |_| {});
                                });
                                window_buttons(ui);
                            });
                            ui.add_space(4.0);
                        })
                        .response;
                    title_bar(ui.ctx(), &bar);
                });
                resize_edges(ui.ctx());
            });
            out.textures_delta.clear();
            let commands = out.viewport_output.remove(&egui::ViewportId::ROOT).map(|v| v.commands).unwrap_or_default();
            (clicked, commands)
        };
        frame(vec![]);
        frame(vec![]);
        let r = tab_rect.get();
        assert!(r.top() > EDGE, "{r:?}");
        for (name, pos) in [
            ("top", egui::pos2(r.center().x, r.top() + 0.5)),
            ("center", r.center()),
            ("bottom", egui::pos2(r.center().x, r.bottom() - 0.5)),
            ("left", egui::pos2(r.left() + 0.5, r.center().y)),
        ] {
            let press = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            frame(vec![egui::Event::PointerMoved(pos)]);
            let (a, ca) = frame(vec![press(true)]);
            let (b, cb) = frame(vec![press(false)]);
            let commands = [ca, cb].concat();
            assert!((a || b) && commands.is_empty(), "{name} {pos:?} in {r:?}: clicked={} {commands:?}", a || b);
            frame(vec![egui::Event::PointerMoved(egui::pos2(450.0, 300.0))]);
            for _ in 0..30 {
                frame(vec![]);
            }
        }
    }
}
