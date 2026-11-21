//! The overlay's own diagnostic log: `overlay.log` in the data folder, next to `watcher.log`.
//!
//! The overlay is a process of its own with no console, and what goes wrong with it happens
//! between it and the window manager (the hotkey was seen but the game was not in front,
//! Windows refused to hand over the keyboard…). Each of those moments writes one line here,
//! with what the window manager answered, so a player can send the file along with a report.
//!
//! Only this computer's window-manager facts go in: this process's own window, and for the
//! window in front its handle, owner ids, class and the first characters of its title. The
//! file stays on this computer; nothing here is sent anywhere.

use std::io::Write;
use std::path::PathBuf;

/// `overlay.log` is renamed to `overlay.log.1` past this size (the previous `.1` is replaced).
const MAX: u64 = 256 * 1024;

pub fn path() -> PathBuf {
    wikelo_watcher::data_dir().join("overlay.log")
}

/// One timestamped line. A failure to write is nobody's problem: the overlay goes on.
pub fn line(args: std::fmt::Arguments<'_>) {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let stamp = format!("{} {:02}:{:02}:{:02}.{:03}", now.date(), now.hour(), now.minute(), now.second(), now.millisecond());
    let _ = append(&path(), &format!("[{stamp}] {args}\n"));
}

fn append(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX {
        let old = path.with_extension("log.1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_add_up_and_a_big_file_makes_room() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("overlay.log");
        append(&log, "one\n").unwrap();
        append(&log, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        std::fs::write(&log, vec![b'x'; MAX as usize + 1]).unwrap();
        append(&log, "three\n").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "three\n", "a fresh file");
        assert_eq!(std::fs::metadata(dir.path().join("overlay.log.1")).unwrap().len(), MAX + 1, "the old one is kept once");
    }
}
