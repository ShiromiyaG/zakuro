//! what a title asks of the system: syscalls and service commands, counted
//! when ZAKURO_USAGE names a file, written there by `dump`.

use std::collections::HashMap;
use std::sync::Mutex;

static COUNTS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ZAKURO_USAGE").is_some())
}

pub fn record(key: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    let mut counts = COUNTS.lock().unwrap();
    *counts.get_or_insert_with(HashMap::new).entry(key()).or_default() += 1;
}

/// the counts, most asked first, a line each
pub fn dump() {
    let Some(path) = std::env::var_os("ZAKURO_USAGE") else {
        return;
    };
    let counts = COUNTS.lock().unwrap();
    let mut lines: Vec<_> = counts.iter().flatten().collect();
    lines.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let text: String = lines.iter().map(|(key, n)| format!("{n} {key}\n")).collect();
    let _ = std::fs::write(path, text);
}
