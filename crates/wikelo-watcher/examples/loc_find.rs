//! Diagnostic: which DataCore records are named after a Game.log location class
//! (`Location[Stanton2_Orison]` in `<RequestLocationInventory>` lines), and which
//! localization keys / string fields they carry.
//!
//! Matches the last `.`-segment of every record name, case-insensitively, prints the struct
//! type, then every non-empty string/bool of the record (60 at most) with `@key` strings
//! resolved through `global.ini`. `LOC_BRIEF=1` prints only names and types.
//!
//! `cargo run --manifest-path watcher/Cargo.toml -p wikelo-watcher --example loc_find -- <Data.p4k> <class> [class…]`

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use starbreaker_datacore::Database;
use wikelo_watcher::GameData;
use wikelo_watcher::loc::Localization;
use wikelo_watcher::resolver::{RefResolver, record_value};

fn strings(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                strings(x, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                strings(x, &format!("{path}[{i}]"), out);
            }
        }
        Value::String(s) if !s.is_empty() => out.push(format!("{path} = {:?}", s.chars().take(120).collect::<String>())),
        Value::Bool(b) => out.push(format!("{path} = {b}")),
        _ => {}
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let p4k = PathBuf::from(args.next().expect("usage: loc_find <Data.p4k> <class> [class…]"));
    let classes: Vec<String> = args.collect();
    let data = GameData::open(&p4k, None).expect("open Data.p4k");
    let db = Database::from_bytes(&data.dcb).expect("parse Game2.dcb");
    let loc = Localization::parse(&data.ini);
    let mut res = RefResolver::new(&db);
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for class in &classes {
        let lc = class.to_ascii_lowercase();
        println!("\n######## {class}");
        let mut hits = Vec::new();
        for r in db.records() {
            let name = db.resolve_string2(r.name_offset);
            let last = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
            if last == lc {
                hits.push((name.to_string(), db.struct_name(r.struct_id()).to_string(), *r, db.is_main_record(r)));
            }
        }
        for (name, sname, r, main) in hits {
            *by_type.entry(sname.clone()).or_default() += 1;
            println!("\n## {name} ({sname}, main={main})");
            if std::env::var_os("LOC_BRIEF").is_some() {
                continue;
            }
            let Ok(v) = res.materialize(&r) else {
                println!("  (cannot materialize)");
                continue;
            };
            let root = record_value(&v);
            let mut out = Vec::new();
            strings(&root, "", &mut out);
            for line in out.iter().take(400) {
                let key = line.split(" = \"").nth(1).map(|s| s.trim_end_matches('"'));
                let text = key.filter(|k| k.starts_with('@')).and_then(|k| loc.text_opt(k));
                match text {
                    Some(t) => println!("  {line}  -> {t:?}"),
                    None => println!("  {line}"),
                }
            }
            if out.len() > 60 {
                println!("  … {} more", out.len().saturating_sub(400));
            }
        }
    }
    println!("\n# record types matched: {by_type:?}");
}
