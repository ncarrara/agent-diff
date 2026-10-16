use super::*;
use std::path::Path;

#[cfg(feature = "sounds")]
mod audio_view;
mod hex_view;
#[cfg(feature = "html")]
mod css;
#[cfg(feature = "html")]
mod html_view;
#[cfg(feature = "images")]
mod image_view;
#[cfg(feature = "markdown")]
mod markdown_view;
#[cfg(feature = "models")]
mod model_view;
#[cfg(any(feature = "html", feature = "markdown"))]
mod rich_text;
#[cfg(feature = "bash")]
mod bash_form;
#[cfg(any(feature = "bash", feature = "python"))]
mod script_runner;
#[cfg(feature = "svg")]
mod svg_view;
#[cfg(feature = "python")]
mod python_form;
#[cfg(feature = "python")]
mod typer_form;

pub(super) trait Preview {
    fn is_for(&self, e: &FileEntry) -> bool;
    fn ui(&mut self, ui: &mut egui::Ui);
    fn run_request(&mut self) -> Option<RunRequest> {
        None
    }
}

#[cfg_attr(not(any(feature = "bash", feature = "python")), allow(dead_code))]
pub(super) struct RunRequest {
    pub(super) remote: bool,
    pub(super) command: String,
}

#[cfg_attr(not(any(feature = "bash", feature = "python")), allow(dead_code))]
pub(super) struct FilePlace<'a> {
    pub(super) local_root: &'a str,
    pub(super) mirror: &'a Path,
    pub(super) remote: &'a str,
}

type Loader = fn(&egui::Context, &FileEntry, &FilePlace) -> Box<dyn Preview>;

struct Plugin {
    source_first: bool,
    handles: fn(&FileEntry) -> bool,
    load: Loader,
}

fn script_runners() -> Vec<Plugin> {
    #[allow(unused_mut)]
    let mut runners: Vec<Plugin> = Vec::new();
    #[cfg(feature = "bash")]
    runners.push(Plugin { source_first: false, handles: bash_form::is_bash, load: |_, e, at| Box::new(script_runner::ScriptRunner::load(e, at, |source| bash_form::parse_bash(source.text))) });
    #[cfg(feature = "python")]
    runners.push(Plugin { source_first: false, handles: typer_form::is_typer, load: |_, e, at| Box::new(script_runner::ScriptRunner::load(e, at, typer_form::parse_typer)) });
    #[cfg(feature = "python")]
    runners.push(Plugin { source_first: false, handles: python_form::is_python, load: |_, e, at| Box::new(script_runner::ScriptRunner::load(e, at, python_form::parse_python)) });
    runners
}

pub(super) fn runner_for<'a>(current: &'a mut Option<Box<dyn Preview>>, ctx: &egui::Context, e: &FileEntry, at: &FilePlace) -> Option<&'a mut (dyn Preview + 'static)> {
    find_or_load(current, script_runners, ctx, e, at)
}

fn plugins() -> Vec<Plugin> {
    #[allow(unused_mut)]
    let mut plugins: Vec<Plugin> = Vec::new();
    #[cfg(feature = "sounds")]
    plugins.push(Plugin { source_first: false, handles: |e| audio_view::is_audio(&e.path), load: |ctx, e, at| Box::new(audio_view::AudioPair::load(ctx, e, at.local_root, at.mirror)) });
    #[cfg(feature = "models")]
    plugins.push(Plugin { source_first: false, handles: |e| model_view::is_model(&e.path), load: |ctx, e, at| Box::new(model_view::ModelPair::load(ctx, e, at.local_root, at.mirror)) });
    #[cfg(feature = "html")]
    plugins.push(Plugin { source_first: true, handles: |e| html_view::is_html(&e.path), load: |_, e, at| Box::new(rich_text::DocumentPair::load(e, at, html_view::parse_html, true)) });
    #[cfg(feature = "markdown")]
    plugins.push(Plugin { source_first: true, handles: |e| markdown_view::is_markdown(&e.path), load: |_, e, at| Box::new(rich_text::DocumentPair::load(e, at, markdown_view::parse_markdown, false)) });
    #[cfg(feature = "svg")]
    plugins.push(Plugin { source_first: false, handles: |e| svg_view::is_svg(&e.path), load: |_, e, at| Box::new(svg_view::SvgPair::load(e, at)) });
    #[cfg(feature = "images")]
    plugins.push(Plugin { source_first: false,
        handles: |e| image_view::is_image(&e.path) && matches!(e.body, Body::Binary),
        load: |ctx, e, at| Box::new(image_view::ImagePair::load(ctx, e, at.local_root, at.mirror)),
    });
    plugins
}

fn find_or_load<'a>(current: &'a mut Option<Box<dyn Preview>>, registry: fn() -> Vec<Plugin>, ctx: &egui::Context, e: &FileEntry, at: &FilePlace) -> Option<&'a mut (dyn Preview + 'static)> {
    if !current.as_ref().is_some_and(|p| p.is_for(e)) {
        *current = registry().into_iter().find(|plugin| (plugin.handles)(e)).map(|plugin| (plugin.load)(ctx, e, at));
    }
    current.as_deref_mut()
}

fn hex_viewer() -> Vec<Plugin> {
    vec![Plugin { source_first: false, handles: |e| matches!(e.body, Body::Binary), load: |_, e, at| Box::new(hex_view::HexPair::load(e, at)) }]
}

pub(super) fn hex_for<'a>(current: &'a mut Option<Box<dyn Preview>>, ctx: &egui::Context, e: &FileEntry, at: &FilePlace) -> Option<&'a mut (dyn Preview + 'static)> {
    find_or_load(current, hex_viewer, ctx, e, at)
}

pub(super) fn starts_on_source(e: &FileEntry) -> bool {
    plugins().into_iter().find(|plugin| (plugin.handles)(e)).is_some_and(|plugin| plugin.source_first)
}

pub(super) fn preview_for<'a>(current: &'a mut Option<Box<dyn Preview>>, ctx: &egui::Context, e: &FileEntry, at: &FilePlace) -> Option<&'a mut (dyn Preview + 'static)> {
    find_or_load(current, plugins, ctx, e, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, body: Body) -> FileEntry {
        FileEntry { path: path.into(), rename: None, origin: Origin::Remote, local_change: None, remote_change: None, local_exists: true, remote_exists: true, body, version: 1 }
    }

    #[test]
    fn documents_start_on_their_source() {
        let text = || Body::Text(Texts::new("x", "y"));
        assert_eq!(starts_on_source(&file("site/index.html", text())), cfg!(feature = "html"));
        assert!(!starts_on_source(&file("logo.png", Body::Binary)));
        assert_eq!(starts_on_source(&file("README.md", text())), cfg!(feature = "markdown"));
        assert!(!starts_on_source(&file("notes.txt", text())));
    }
}
