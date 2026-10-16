mod app;
mod diff;
mod engine;
mod highlight;
mod icon;
mod remote;

use clap::Parser;
use eframe::egui;
use std::path::PathBuf;

/// Review and selectively sync changes between a local workspace and a remote
/// workspace where an agent (e.g. Claude) is working.
#[derive(Parser)]
struct Args {
    /// Local workspace directory.
    local: Option<PathBuf>,
    /// Remote workspace: `user@host:/path` (rsync over ssh) or a local directory.
    remote: Option<String>,
    /// Seconds between remote polls (default: last used, or 2).
    #[arg(long)]
    interval: Option<f64>,
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let _ = std::fs::create_dir_all(engine::ssh_socket_dir());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_title("agent-diff")
            .with_app_id("agent-diff")
            .with_icon(egui::IconData { rgba: icon::icon_rgba(256), width: 256, height: 256 })
            .with_decorations(false),
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        ..Default::default()
    };
    eframe::run_native(
        "agent-diff",
        options,
        Box::new(move |cc| Ok(Box::new(app::AgentDiffApp::new(&cc.egui_ctx, args.local, args.remote, args.interval)))),
    )
}
