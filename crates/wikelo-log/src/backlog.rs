//! Blueprints already received, read from one old log (a file of `logbackups\`).
//!
//! The file is read as a stream, line by line, and only `Received Blueprint:` notification
//! lines are kept (through [`LineParser`], the same parser the live tailing uses: the line
//! must start with its timestamp and the notification, and the name be plain text of at most
//! 200 characters). It reads only: the file is opened for reading and nothing is written
//! anywhere.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::parser::{LineParser, LogEvent};

/// Most bytes read of one file: 256 MiB. A session's log is a few MB to a few dozen; past
/// this the rest of the file is not read (see [`BlueprintScan::truncated`]).
pub const BACKLOG_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Most bytes kept of one line: 64 KiB. A blueprint line is a few hundred bytes; the rest of
/// a longer line is read and dropped, so one huge line never takes the memory.
pub const BACKLOG_MAX_LINE: usize = 64 * 1024;
/// Most blueprints kept of one file: 10 000. A session receives a handful; past this the
/// reading stops (see [`BlueprintScan::capped`]).
pub const BACKLOG_MAX_BLUEPRINTS: usize = 10_000;

/// One `Received Blueprint: <name>:` notification: the line's timestamp and the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintSeen {
    pub at: String,
    pub name: String,
}

/// What [`blueprints_in`] found in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlueprintScan {
    /// In the order of the file.
    pub blueprints: Vec<BlueprintSeen>,
    /// Bytes read.
    pub bytes: u64,
    /// The file was bigger than the limit and its end was not read.
    pub truncated: bool,
    /// The file had more than [`BACKLOG_MAX_BLUEPRINTS`] blueprints: the reading stopped at
    /// the first one past that number, and the rest of the file was not read.
    pub capped: bool,
}

/// The blueprints received in the log at `path`, reading at most [`BACKLOG_MAX_BYTES`] of it,
/// keeping at most [`BACKLOG_MAX_LINE`] bytes of each line and at most
/// [`BACKLOG_MAX_BLUEPRINTS`] blueprints. Invalid UTF-8 is replaced, never an error; only
/// opening or reading the file fails.
pub fn blueprints_in(path: &Path, parser: &LineParser) -> std::io::Result<BlueprintScan> {
    blueprints_in_limited(path, parser, BACKLOG_MAX_BYTES, BACKLOG_MAX_LINE)
}

/// [`blueprints_in`] with other limits on bytes (tests use small ones).
pub fn blueprints_in_limited(path: &Path, parser: &LineParser, max_bytes: u64, max_line: usize) -> std::io::Result<BlueprintScan> {
    scan_limited(path, parser, max_bytes, max_line, BACKLOG_MAX_BLUEPRINTS)
}

/// [`blueprints_in_limited`] with another limit on the blueprints kept (tests use a small one).
fn scan_limited(path: &Path, parser: &LineParser, max_bytes: u64, max_line: usize, max_blueprints: usize) -> std::io::Result<BlueprintScan> {
    // `File::open` asks Windows to share reading, writing and deleting, so the game can still
    // move or delete the file while it is read here.
    let file = File::open(path)?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(1 << 16, file.take(max_bytes));
    let mut scan = BlueprintScan { truncated: size > max_bytes, ..Default::default() };
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        let n = read_line_capped(&mut reader, &mut buf, max_line)?;
        if n == 0 {
            break;
        }
        scan.bytes += n;
        let line = String::from_utf8_lossy(&buf);
        // Cheap check first: almost no line is a blueprint notification.
        if !line.contains("Received Blueprint:") {
            continue;
        }
        // The parser keeps only well-formed lines with a plain name of at most 200 characters.
        if let Some(LogEvent::BlueprintReceived { at, name }) = parser.parse(&line) {
            if scan.blueprints.len() >= max_blueprints {
                scan.capped = true;
                break;
            }
            scan.blueprints.push(BlueprintSeen { at, name });
        }
    }
    Ok(scan)
}

/// The next line of `r` in `buf` (its newline included when it fits), keeping at most `max`
/// bytes of it: the rest of a longer line is read and dropped. Returns the bytes taken from
/// `r`, 0 at the end. A last line without a newline is a line too.
fn read_line_capped(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<u64> {
    buf.clear();
    let mut taken = 0u64;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(taken);
        }
        let (used, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let room = max.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..used.min(room)]);
        r.consume(used);
        taken += used as u64;
        if done {
            return Ok(taken);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn blueprint_line(at: &str, name: &str) -> String {
        format!(
            "<{at}> [Notice] <SHUDEvent_OnNotification> Added notification \"Received Blueprint: {name}: \" [3] to queue. New queue size: 1, MissionId: [00000000-0000-0000-0000-000000000000], ObjectiveId: [] [Team_CoreGameplayFeatures][Missions][Comms]\n"
        )
    }

    #[test]
    fn finds_blueprints_with_their_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Game Build(12881860) 12 Oct 26 (13 27 10).log");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"<2026-10-12T13:27:10.000Z> BackupNameAttachment=\" Build(12881860) 12 Oct 26 (13 27 10)\"  -- used by backup system\n").unwrap();
        f.write_all(blueprint_line("2026-10-12T14:00:00.000Z", "Monde Arms Hemlock Camo").as_bytes()).unwrap();
        f.write_all(b"<2026-10-12T14:00:01.000Z> [Notice] <StatObjLoad 0x800 Format> whatever\n").unwrap();
        // Invalid UTF-8 on a line of its own and inside a blueprint name.
        f.write_all(b"<2026-10-12T14:00:02.000Z> \xff\xfe broken\n").unwrap();
        let bad: Vec<u8> = blueprint_line("2026-10-12T15:00:00.000Z", "Karna @").bytes().map(|b| if b == b'@' { 0xff } else { b }).collect();
        f.write_all(&bad).unwrap();
        // The last line has no newline.
        f.write_all(blueprint_line("2026-10-12T16:00:00.000Z", "Last One").trim_end().as_bytes()).unwrap();
        drop(f);
        let scan = blueprints_in(&p, &LineParser::new()).unwrap();
        assert_eq!(
            scan.blueprints,
            vec![
                BlueprintSeen { at: "2026-10-12T14:00:00.000Z".into(), name: "Monde Arms Hemlock Camo".into() },
                BlueprintSeen { at: "2026-10-12T15:00:00.000Z".into(), name: "Karna \u{FFFD}".into() },
                BlueprintSeen { at: "2026-10-12T16:00:00.000Z".into(), name: "Last One".into() },
            ]
        );
        assert!(!scan.truncated);
        assert_eq!(scan.bytes, std::fs::metadata(&p).unwrap().len());
    }

    #[test]
    fn a_very_long_line_is_cut_and_the_next_lines_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("long.log");
        let mut f = File::create(&p).unwrap();
        f.write_all("x".repeat(300_000).as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        f.write_all(blueprint_line("2026-10-12T14:00:00.000Z", "After").as_bytes()).unwrap();
        drop(f);
        let scan = blueprints_in_limited(&p, &LineParser::new(), BACKLOG_MAX_BYTES, 1024).unwrap();
        assert_eq!(scan.blueprints.len(), 1);
        assert_eq!(scan.blueprints[0].name, "After");
    }

    #[test]
    fn stops_at_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.log");
        let first = blueprint_line("2026-10-12T14:00:00.000Z", "First");
        let mut f = File::create(&p).unwrap();
        f.write_all(first.as_bytes()).unwrap();
        f.write_all(blueprint_line("2026-10-12T15:00:00.000Z", "Past The Limit").as_bytes()).unwrap();
        drop(f);
        let limit = first.len() as u64;
        let scan = blueprints_in_limited(&p, &LineParser::new(), limit, BACKLOG_MAX_LINE).unwrap();
        assert_eq!(scan.blueprints.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), vec!["First"]);
        assert!(scan.truncated);
        assert_eq!(scan.bytes, limit);
    }

    #[test]
    fn stops_collecting_past_the_blueprint_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.log");
        let mut f = File::create(&p).unwrap();
        for i in 0..5 {
            f.write_all(blueprint_line("2026-10-12T14:00:00.000Z", &format!("Blueprint {i}")).as_bytes()).unwrap();
        }
        drop(f);
        let scan = scan_limited(&p, &LineParser::new(), BACKLOG_MAX_BYTES, BACKLOG_MAX_LINE, 3).unwrap();
        assert_eq!(scan.blueprints.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), vec!["Blueprint 0", "Blueprint 1", "Blueprint 2"]);
        assert!(scan.capped);
        // Exactly at the limit is not past it.
        let exact = scan_limited(&p, &LineParser::new(), BACKLOG_MAX_BYTES, BACKLOG_MAX_LINE, 5).unwrap();
        assert_eq!(exact.blueprints.len(), 5);
        assert!(!exact.capped);
    }

    #[test]
    fn only_anchored_lines_with_plain_names_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("odd.log");
        let mut f = File::create(&p).unwrap();
        // The notification's words inside another message, and without a leading timestamp.
        let inner = blueprint_line("2026-10-12T14:00:00.000Z", "Inner");
        f.write_all(format!("<2026-10-12T14:00:00.000Z> [Notice] <Chat> quoted: {}", inner.split_once("> ").unwrap().1).as_bytes()).unwrap();
        f.write_all(inner.split_once("> ").unwrap().1.as_bytes()).unwrap();
        f.write_all(blueprint_line("2026-10-12T14:00:01.000Z", &"x".repeat(201)).as_bytes()).unwrap();
        f.write_all(blueprint_line("2026-10-12T14:00:02.000Z", "Spoof\u{202E}txt").as_bytes()).unwrap();
        f.write_all(blueprint_line("2026-10-12T14:00:03.000Z", "Kept").as_bytes()).unwrap();
        drop(f);
        let scan = blueprints_in(&p, &LineParser::new()).unwrap();
        assert_eq!(scan.blueprints, vec![BlueprintSeen { at: "2026-10-12T14:00:03.000Z".into(), name: "Kept".into() }]);
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(blueprints_in(&dir.path().join("none.log"), &LineParser::new()).is_err());
    }
}
