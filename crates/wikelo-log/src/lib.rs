//! Game.log parsing for Wikelo contracts: line parser, per-session mission
//! tracker, history importer over `logbackups\`, a reader of the blueprints in one old log
//! and a polling tailer.
//!
//! Only `Game.log` lines are read; nothing is sent anywhere.

pub mod backlog;
pub mod import;
pub mod parser;
pub mod tailer;
pub mod tracker;

pub use backlog::{BACKLOG_MAX_BLUEPRINTS, BACKLOG_MAX_BYTES, BACKLOG_MAX_LINE, BlueprintScan, BlueprintSeen, blueprints_in, blueprints_in_limited};
pub use import::{ImportReport, import_history, list_logbackups};
pub use parser::{CompletionType, LineParser, LogEvent, timestamp_of};
pub use tailer::{Tailer, TailerConfig};
pub use tracker::{MissionState, MissionTracker, TrackerOutput};
