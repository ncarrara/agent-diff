use super::*;

#[derive(Default)]
pub(super) struct FactoryReset {
    asking: bool,
}

pub(super) enum ResetChoice {
    Waiting,
    Cancelled,
    Confirmed,
}

impl FactoryReset {
    pub(super) fn ask(&mut self) {
        self.asking = true;
    }

    pub(super) fn dialog(&mut self, ctx: &egui::Context, running: &[(String, String)]) -> ResetChoice {
        if !self.asking {
            return ResetChoice::Waiting;
        }
        let mut choice = ResetChoice::Waiting;
        let modal = egui::Modal::new(egui::Id::new("factory-reset")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Reset agent-diff to factory settings?");
            ui.add_space(6.0);
            ui.label("This deletes, then quits the app:");
            ui.label(RichText::new(format!("• {}", crate::engine::app_dir().display())).monospace());
            ui.add_space(6.0);
            ui.label("Workspaces and options, mirrors, baselines, backups of overwritten files, pushes still waiting, and the saved output and history of shell tabs are lost. The shared ssh connections are closed.");
            ui.weak("Your local workspaces, the remote files and Claude's conversations on the remote are not touched.");
            if !running.is_empty() {
                ui.add_space(6.0);
                ui.colored_label(Color32::LIGHT_RED, "Wait for these to finish first:");
                for (workspace, step) in running {
                    ui.label(format!("• {workspace}: {step}"));
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let red = egui::Button::new(RichText::new("Delete everything and quit").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0xc0, 0x39, 0x2b));
                if ui.add_enabled(running.is_empty(), red).clicked() {
                    choice = ResetChoice::Confirmed;
                }
                if ui.button("Cancel").clicked() {
                    choice = ResetChoice::Cancelled;
                }
            });
        });
        if modal.should_close() && matches!(choice, ResetChoice::Waiting) {
            choice = ResetChoice::Cancelled;
        }
        if !matches!(choice, ResetChoice::Waiting) {
            self.asking = false;
        }
        choice
    }
}

pub(super) fn wipe_app_data(remotes: &[String]) -> Vec<String> {
    for remote in remotes {
        if let Ok(remote) = Remote::parse(remote) {
            remote.forget_connection();
        }
    }
    let dir = crate::engine::app_dir();
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => vec![format!("{}: {e}", dir.display())],
        _ => Vec::new(),
    }
}

pub(super) fn workspace_data_places(ws: &Workspace) -> Vec<PathBuf> {
    let mut places = Remote::parse(&ws.remote).map(|remote| crate::engine::workspace_state_dirs(&ws.local, &remote)).unwrap_or_default();
    for tab in &ws.sessions {
        places.push(saved_dir(&tab.id));
    }
    places
}

pub(super) fn wipe_workspace_data(places: &[PathBuf]) -> Vec<String> {
    places
        .iter()
        .filter(|p| p.exists())
        .filter_map(|dir| std::fs::remove_dir_all(dir).err().map(|e| format!("{}: {e}", dir.display())))
        .collect()
}

pub(super) fn workspace_reset_ui(ui: &mut egui::Ui, ws: &Workspace, running: Option<&str>) -> bool {
    ui.colored_label(Color32::LIGHT_RED, "Reset this workspace to factory settings?");
    ui.label("This deletes:");
    for place in workspace_data_places(ws).into_iter().filter(|p| p.exists()) {
        ui.label(RichText::new(format!("• {}", place.display())).monospace());
    }
    ui.label("Its mirror, baseline, backups of overwritten files, pushes still waiting, and the saved output of its shell tabs are lost; its view options, terminal tabs, fork graph and commit choices go back to the defaults. It then starts again from a fresh baseline (the current local files).");
    ui.weak("Its paths and poll interval stay. The local workspace, the remote files, Claude's conversations on the remote and the other workspaces are not touched.");
    if let Some(step) = running {
        ui.colored_label(Color32::LIGHT_RED, format!("Wait for this to finish first: {step}"));
    }
    let red = egui::Button::new(RichText::new("Reset this workspace").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0xc0, 0x39, 0x2b));
    ui.add_enabled(running.is_none(), red).clicked()
}
