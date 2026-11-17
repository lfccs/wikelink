//! The language of everything the program shows: English, or Portuguese when Windows itself
//! is in Portuguese. `watcher.json` may pin one (`"lang": "en"` / `"pt"`); anything else,
//! or nothing, follows the system.
//!
//! Every user-facing string is written in place in both languages:
//!
//! ```ignore
//! ui.label(tr!("Hide", "Ocultar"));
//! let line = trf!("{n} items", "{n} itens");
//! ```
//!
//! Game names (items, missions, places) are never translated.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Pt,
}

const UNSET: u8 = 0;
const EN: u8 = 1;
const PT: u8 = 2;

static LANG: AtomicU8 = AtomicU8::new(UNSET);

/// The language in use. Detected from the system on first use unless [`set`] ran before.
///
/// Unit tests always run in Portuguese, so their expectations do not depend on the machine.
pub fn lang() -> Lang {
    if cfg!(test) {
        return Lang::Pt;
    }
    match LANG.load(Ordering::Relaxed) {
        EN => Lang::En,
        PT => Lang::Pt,
        _ => {
            let l = system();
            LANG.store(if l == Lang::Pt { PT } else { EN }, Ordering::Relaxed);
            l
        }
    }
}

/// Apply the preference stored in `watcher.json`: `"en"`, `"pt"`, or the system's language
/// for anything else. Call once at the start of every process, before the first string.
pub fn set(pref: Option<&str>) {
    let l = parse(pref).unwrap_or_else(system);
    LANG.store(if l == Lang::Pt { PT } else { EN }, Ordering::Relaxed);
}

/// A stored preference, `None` when it means "follow the system".
pub fn parse(pref: Option<&str>) -> Option<Lang> {
    match pref.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        Some("en") => Some(Lang::En),
        Some("pt") | Some("pt-br") => Some(Lang::Pt),
        _ => None,
    }
}

/// Portuguese of any country when the Windows display language is Portuguese, else English.
fn system() -> Lang {
    #[cfg(windows)]
    {
        // SAFETY: no arguments, no pointers; returns the user's UI language id.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        from_langid(id)
    }
    #[cfg(not(windows))]
    {
        let env = std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).unwrap_or_default();
        if env.to_ascii_lowercase().starts_with("pt") { Lang::Pt } else { Lang::En }
    }
}

/// A Windows LANGID: its low 10 bits are the primary language, 0x16 being Portuguese.
#[cfg_attr(not(windows), allow(dead_code))]
fn from_langid(id: u16) -> Lang {
    if id & 0x3ff == 0x16 { Lang::Pt } else { Lang::En }
}

/// The string for the language in use.
pub fn pick<'a>(en: &'a str, pt: &'a str) -> &'a str {
    match lang() {
        Lang::En => en,
        Lang::Pt => pt,
    }
}

/// `tr!("English", "Português")`: the literal for the language in use, as `&'static str`.
#[macro_export]
macro_rules! tr {
    ($en:expr, $pt:expr $(,)?) => {
        $crate::i18n::pick($en, $pt)
    };
}

/// `trf!("{n} items", "{n} itens")`: `format!` in the language in use. Both literals must use
/// the same positional arguments (inline `{name}` captures are free).
#[macro_export]
macro_rules! trf {
    ($en:literal, $pt:literal $(, $($arg:tt)*)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::En => format!($en $(, $($arg)*)?),
            $crate::i18n::Lang::Pt => format!($pt $(, $($arg)*)?),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_preferences() {
        assert_eq!(parse(Some("en")), Some(Lang::En));
        assert_eq!(parse(Some(" PT ")), Some(Lang::Pt));
        assert_eq!(parse(Some("pt-BR")), Some(Lang::Pt));
        assert_eq!(parse(Some("system")), None);
        assert_eq!(parse(Some("")), None);
        assert_eq!(parse(None), None);
    }

    #[test]
    fn windows_language_ids() {
        assert_eq!(from_langid(0x0416), Lang::Pt, "pt-BR");
        assert_eq!(from_langid(0x0816), Lang::Pt, "pt-PT");
        assert_eq!(from_langid(0x0409), Lang::En, "en-US");
        assert_eq!(from_langid(0x0c0a), Lang::En, "es-ES");
    }

    #[test]
    fn macros_pick_one_literal() {
        let n = 3;
        assert_eq!(tr!("Hide", "Ocultar"), "Ocultar");
        assert_eq!(trf!("{n} items", "{n} itens"), "3 itens");
        assert_eq!(trf!("{} of {}", "{} de {}", 1, n), "1 de 3");
    }
}
