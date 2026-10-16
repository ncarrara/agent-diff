use super::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{Receiver, channel};

const BUCKETS: usize = 2048;
const BLOCK_FRAMES: usize = 256;
const WAV_PLAYERS: &[(&str, &[&str])] = &[("pw-play", &[]), ("paplay", &[]), ("aplay", &[])];
const MP3_PLAYERS: &[(&str, &[&str])] = &[("mpv", &["--no-video", "--really-quiet"]), ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]), ("mpg123", &["-q"]), ("pw-play", &[]), ("paplay", &[])];

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub(super) fn is_audio(path: &str) -> bool {
    matches!(extension(Path::new(path)).as_str(), "wav" | "mp3")
}

fn players_for(path: &Path) -> &'static [(&'static str, &'static [&'static str])] {
    if extension(path) == "mp3" { MP3_PLAYERS } else { WAV_PLAYERS }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct AudioInfo {
    pub(super) channels: u16,
    pub(super) sample_rate: u32,
    pub(super) bits: u16,
    pub(super) float: bool,
    pub(super) mp3: bool,
    pub(super) frames: u32,
}

impl AudioInfo {
    pub(super) fn seconds(&self) -> f64 {
        if self.sample_rate == 0 { 0.0 } else { self.frames as f64 / self.sample_rate as f64 }
    }
}

pub(super) struct Clip {
    pub(super) info: AudioInfo,
    pub(super) peaks: Vec<(f32, f32)>,
}

pub(super) fn load_clip(path: &Path) -> Result<Clip, String> {
    match extension(path).as_str() {
        "mp3" => load_mp3(path),
        _ => load_wav(path),
    }
}

fn load_wav(path: &Path) -> Result<Clip, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("cannot read the sound: {e}"))?;
    let spec = reader.spec();
    let info = AudioInfo {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        bits: spec.bits_per_sample,
        float: spec.sample_format == hound::SampleFormat::Float,
        mp3: false,
        frames: reader.duration(),
    };
    let scale = 1.0 / (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
    let mut peaks = Peaks::new(info.channels as usize);
    if info.float {
        reader.samples::<f32>().map_while(Result::ok).for_each(|s| peaks.add(s));
    } else {
        reader.samples::<i32>().map_while(Result::ok).for_each(|s| peaks.add(s as f32 * scale));
    }
    Ok(Clip { peaks: peaks.finish(), info })
}

fn load_mp3(path: &Path) -> Result<Clip, String> {
    use symphonia::core::{audio::SampleBuffer, codecs::DecoderOptions, errors::Error, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint};
    let cannot = |e: &dyn std::fmt::Display| format!("cannot read the sound: {e}");
    let file = std::fs::File::open(path).map_err(|e| cannot(&e))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| cannot(&e))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| cannot(&"no audio track"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| cannot(&e))?;
    let mut info = AudioInfo { channels: 0, sample_rate: 0, bits: 0, float: false, mp3: true, frames: 0 };
    let mut peaks: Option<Peaks> = None;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(cannot(&e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(cannot(&e)),
        };
        let spec = *decoded.spec();
        info.channels = spec.channels.count() as u16;
        info.sample_rate = spec.rate;
        info.frames += decoded.frames() as u32;
        let samples = buffer.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if samples.capacity() < decoded.capacity() * spec.channels.count() {
            *samples = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        samples.copy_interleaved_ref(decoded);
        let peaks = peaks.get_or_insert_with(|| Peaks::new(info.channels as usize));
        samples.samples().iter().for_each(|&s| peaks.add(s));
    }
    if info.frames == 0 {
        return Err(cannot(&"no sound in this file"));
    }
    Ok(Clip { peaks: peaks.map(Peaks::finish).unwrap_or_default(), info })
}

struct Peaks {
    channels: usize,
    seen: usize,
    blocks: Vec<(f32, f32)>,
}

impl Peaks {
    fn new(channels: usize) -> Self {
        Peaks { channels: channels.max(1), seen: 0, blocks: Vec::new() }
    }

    fn add(&mut self, sample: f32) {
        let block = self.seen / self.channels / BLOCK_FRAMES;
        if block == self.blocks.len() {
            self.blocks.push((0.0, 0.0));
        }
        let (low, high) = &mut self.blocks[block];
        *low = low.min(sample);
        *high = high.max(sample);
        self.seen += 1;
    }

    fn finish(self) -> Vec<(f32, f32)> {
        let buckets = BUCKETS.min(self.blocks.len()).max(1);
        let mut out = vec![(0.0f32, 0.0f32); buckets];
        for (i, (low, high)) in self.blocks.iter().enumerate() {
            let (l, h) = &mut out[i * buckets / self.blocks.len().max(1)];
            *l = l.min(*low);
            *h = h.max(*high);
        }
        out
    }
}

enum Loaded {
    Loading(Receiver<Result<Clip, String>>),
    Ready(Result<Clip, String>),
}

struct Track {
    path: PathBuf,
    loaded: Loaded,
}

impl Track {
    fn load(path: PathBuf, ctx: &egui::Context) -> Self {
        let (tx, rx) = channel();
        let (worker_path, ctx) = (path.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(load_clip(&worker_path));
            ctx.request_repaint();
        });
        Track { path, loaded: Loaded::Loading(rx) }
    }

    fn poll(&mut self) {
        if let Loaded::Loading(rx) = &self.loaded
            && let Ok(done) = rx.try_recv()
        {
            self.loaded = Loaded::Ready(done);
        }
    }

    fn info(&mut self) -> Option<AudioInfo> {
        self.poll();
        match &self.loaded {
            Loaded::Ready(Ok(clip)) => Some(clip.info),
            _ => None,
        }
    }
}

pub(super) struct AudioPair {
    key: (String, u64),
    local: Option<Track>,
    remote: Option<Track>,
    identical: bool,
    playing: Option<(PathBuf, Child)>,
    error: Option<String>,
}

impl Drop for AudioPair {
    fn drop(&mut self) {
        self.stop();
    }
}

impl AudioPair {
    pub(super) fn is_for(&self, e: &FileEntry) -> bool {
        self.key == (e.path.clone(), e.version)
    }

    pub(super) fn load(ctx: &egui::Context, e: &FileEntry, local_root: &str, mirror: &Path) -> Self {
        let (local_path, remote_path) = match &e.rename {
            Some(r) => (r.local_path.as_str(), r.remote_path.as_str()),
            None => (e.path.as_str(), e.path.as_str()),
        };
        let local = Path::new(local_root).join(local_path);
        let remote = mirror.join(remote_path);
        let identical = e.origin == Origin::Synced || (local.exists() && crate::engine::same_content(&local, &remote));
        AudioPair {
            key: (e.path.clone(), e.version),
            local: local.exists().then(|| Track::load(local, ctx)),
            remote: (!identical && remote.exists()).then(|| Track::load(remote, ctx)),
            identical,
            playing: None,
            error: None,
        }
    }

    fn stop(&mut self) {
        if let Some((_, mut child)) = self.playing.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn play(&mut self, path: &Path) {
        self.stop();
        self.error = None;
        let players = players_for(path);
        for (player, args) in players {
            match Command::new(player).args(*args).arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
                Ok(child) => {
                    self.playing = Some((path.to_path_buf(), child));
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    self.error = Some(format!("cannot play with {player}: {e}"));
                    return;
                }
            }
        }
        let names: Vec<&str> = players.iter().map(|(name, _)| *name).collect();
        self.error = Some(format!("no audio player found (install one of: {})", names.join(", ")));
    }

    fn playing_path(&mut self) -> Option<&Path> {
        if let Some((_, child)) = &mut self.playing
            && !matches!(child.try_wait(), Ok(None))
        {
            self.playing = None;
        }
        self.playing.as_ref().map(|(path, _)| path.as_path())
    }
}

pub(super) fn audio_view(ui: &mut egui::Ui, pair: &mut AudioPair) {
    if pair.playing_path().is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
    if let Some(e) = &pair.error {
        ui.colored_label(Color32::LIGHT_RED, e);
    }
    let mut action = None;
    let playing = pair.playing_path().map(Path::to_path_buf);
    if pair.identical {
        action = track_ui(ui, "Identical on both sides", Color32::GRAY, pair.local.as_mut(), None, playing.as_deref());
    } else {
        let local_info = pair.local.as_mut().and_then(Track::info);
        let remote_info = pair.remote.as_mut().and_then(Track::info);
        let (local, remote) = (pair.local.as_mut(), pair.remote.as_mut());
        ui.columns(2, |columns| {
            let left = track_ui(&mut columns[0], "Local (yours)", YOU, local, remote_info, playing.as_deref());
            let right = track_ui(&mut columns[1], "Remote (Claude)", CLAUDE, remote, local_info, playing.as_deref());
            action = left.or(right);
        });
    }
    match action {
        Some(Playback::Play(path)) => pair.play(&path),
        Some(Playback::Stop) => pair.stop(),
        None => {}
    }
}

enum Playback {
    Play(PathBuf),
    Stop,
}

fn track_ui(ui: &mut egui::Ui, title: &str, color: Color32, track: Option<&mut Track>, other: Option<AudioInfo>, playing: Option<&Path>) -> Option<Playback> {
    let mut action = None;
    let Some(track) = track else {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(color).strong());
            ui.weak("not present");
        });
        return None;
    };
    track.poll();
    let clip = ui
        .horizontal(|ui| {
            ui.label(RichText::new(title).color(color).strong());
            match &track.loaded {
                Loaded::Loading(_) => {
                    ui.spinner();
                    None
                }
                Loaded::Ready(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                    None
                }
                Loaded::Ready(Ok(clip)) => {
                    let is_playing = playing == Some(track.path.as_path());
                    let (label, help) = if is_playing { ("■ Stop", "Stop playing") } else { ("▶ Play", "Play this version with the system's audio player") };
                    if ui.small_button(label).on_hover_text(help).clicked() {
                        action = Some(if is_playing { Playback::Stop } else { Playback::Play(track.path.clone()) });
                    }
                    ui.weak(audio_text(&clip.info, other));
                    Some(clip)
                }
            }
        })
        .inner;
    if let Some(clip) = clip {
        waveform(ui, clip, color);
    }
    action
}

fn audio_text(info: &AudioInfo, other: Option<AudioInfo>) -> String {
    let difference = match other {
        Some(o) if (o.seconds() - info.seconds()).abs() >= 0.005 => format!(" ({:+.2} s)", info.seconds() - o.seconds()),
        _ => String::new(),
    };
    let depth = match (info.mp3, info.float) {
        (true, _) => "MP3".to_string(),
        (false, true) => format!("{}-bit float", info.bits),
        (false, false) => format!("{}-bit", info.bits),
    };
    format!("{:.2} s{difference} · {} Hz · {} ch · {depth}", info.seconds(), info.sample_rate, info.channels)
}

fn waveform(ui: &mut egui::Ui, clip: &Clip, color: Color32) {
    let height = ui.available_height().clamp(60.0, 220.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    painter.hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.4)));
    let columns = rect.width().max(1.0) as usize;
    let half = rect.height() * 0.5 - 2.0;
    for column in 0..columns {
        let from = column * clip.peaks.len() / columns;
        let to = ((column + 1) * clip.peaks.len() / columns).max(from + 1).min(clip.peaks.len());
        let (low, high) = clip.peaks[from..to].iter().fold((0.0f32, 0.0f32), |(l, h), (a, b)| (l.min(*a), h.max(*b)));
        let x = rect.left() + column as f32 + 0.5;
        let top = rect.center().y - high.clamp(-1.0, 1.0) * half;
        let bottom = rect.center().y - low.clamp(-1.0, 1.0) * half;
        painter.vline(x, top..=bottom.max(top + 1.0), egui::Stroke::new(1.0, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sine(path: &Path, seconds: f32, amplitude: f32) {
        let spec = hound::WavSpec { channels: 2, sample_rate: 8000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(8000.0 * seconds) as usize {
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / 8000.0).sin() * amplitude * i16::MAX as f32;
            writer.write_sample(v as i16).unwrap();
            writer.write_sample(v as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn audio_files_by_extension() {
        assert!(is_audio("sounds/click.wav") && is_audio("A.WAV") && is_audio("music/theme.MP3"));
        assert!(!is_audio("a.ogg") && !is_audio("wav"));
    }

    #[test]
    fn reads_the_format_and_the_peaks() {
        let dir = std::env::temp_dir().join(format!("agent-diff-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_sine(&dir.join("tone.wav"), 0.5, 0.5);
        let clip = load_clip(&dir.join("tone.wav")).unwrap();
        assert_eq!(clip.info, AudioInfo { channels: 2, sample_rate: 8000, bits: 16, float: false, mp3: false, frames: 4000 });
        assert!((clip.info.seconds() - 0.5).abs() < 1e-9);
        let loudest = clip.peaks.iter().map(|(_, h)| *h).fold(0.0, f32::max);
        assert!((loudest - 0.5).abs() < 0.02, "{loudest}");
        std::fs::write(dir.join("broken.wav"), b"RIFF nonsense").unwrap();
        assert!(load_clip(&dir.join("broken.wav")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn silent_mp3(frames: usize) -> Vec<u8> {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC0]);
        frame.repeat(frames)
    }

    #[test]
    fn reads_an_mp3() {
        let dir = std::env::temp_dir().join(format!("agent-diff-mp3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("silence.mp3"), silent_mp3(40)).unwrap();
        let clip = load_clip(&dir.join("silence.mp3")).unwrap();
        assert!(clip.info.mp3 && clip.info.channels == 1 && clip.info.sample_rate == 44100, "{:?}", clip.info);
        let expected = 40.0 * 1152.0 / 44100.0;
        assert!((clip.info.seconds() - expected).abs() < 2.0 * 1152.0 / 44100.0, "{:?}", clip.info);
        assert!(clip.peaks.iter().all(|(l, h)| l.abs() < 1e-3 && h.abs() < 1e-3));
        assert!(audio_text(&clip.info, None).ends_with("44100 Hz · 1 ch · MP3"), "{}", audio_text(&clip.info, None));
        std::fs::write(dir.join("broken.mp3"), b"not an mp3 at all").unwrap();
        assert!(load_clip(&dir.join("broken.mp3")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn side_by_side_with_the_length_difference() {
        let root = std::env::temp_dir().join(format!("agent-diff-audio-pair-{}", std::process::id()));
        std::fs::create_dir_all(root.join("local/sfx")).unwrap();
        std::fs::create_dir_all(root.join("mirror/sfx")).unwrap();
        write_sine(&root.join("local/sfx/hit.wav"), 0.5, 0.5);
        write_sine(&root.join("mirror/sfx/hit.wav"), 1.0, 0.8);
        let entry = FileEntry {
            path: "sfx/hit.wav".into(),
            rename: None,
            origin: Origin::Remote,
            local_change: None,
            remote_change: None,
            local_exists: true,
            remote_exists: true,
            body: Body::Binary,
            version: 1,
        };
        let ctx = egui::Context::default();
        let mut pair = AudioPair::load(&ctx, &entry, root.join("local").to_str().unwrap(), &root.join("mirror"));
        let mut texts = Vec::new();
        for _ in 0..200 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 400.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| audio_view(ui, &mut pair));
            out.textures_delta.clear();
            texts = out.shapes.iter().filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some(t.galley.job.text.clone()) } else { None }).collect();
            if texts.iter().any(|t| t.contains("(-0.50 s)")) && texts.iter().any(|t| t.contains("(+0.50 s)")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(texts.iter().any(|t| t.starts_with("0.50 s (-0.50 s) · 8000 Hz · 2 ch · 16-bit")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("1.00 s (+0.50 s)")), "{texts:?}");
        assert_eq!(texts.iter().filter(|t| t.as_str() == "▶ Play").count(), 2, "{texts:?}");
        drop(pair);
        let _ = std::fs::remove_dir_all(root);
    }
}

impl Preview for AudioPair {
    fn is_for(&self, e: &FileEntry) -> bool {
        AudioPair::is_for(self, e)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        audio_view(ui, self);
    }
}
