use super::*;

/// `key=value` lines in `~/.agent-diff/settings`. `workspace` lines
/// repeat, one per tab: `local<TAB>remote<TAB>interval<TAB>claude-ids`, the
/// last being the comma-separated Claude conversation IDs.
pub(super) fn settings_file() -> Option<PathBuf> {
    Some(crate::engine::app_dir().join("settings"))
}

pub(super) fn load_settings() -> Vec<(String, String)> {
    let text = settings_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.to_string())).collect()
}
