//! Diagnostic: every `StarMapObject` record as one JSON array (scalars and references only,
//! with `@keys` resolved), to study which places are real, hidden or placeholders and how they
//! hang from planets and systems.
//!
//! `cargo run --manifest-path watcher/Cargo.toml -p wikelo-watcher --example starmap_dump -- <Data.p4k> <out.json>`

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use starbreaker_datacore::Database;
use wikelo_watcher::GameData;
use wikelo_watcher::loc::Localization;
use wikelo_watcher::resolver::{RefResolver, record_value};

fn main() {
    let mut args = std::env::args().skip(1);
    let p4k = PathBuf::from(args.next().expect("usage: starmap_dump <Data.p4k> <out.json>"));
    let out = PathBuf::from(args.next().expect("usage: starmap_dump <Data.p4k> <out.json>"));
    let data = GameData::open(&p4k, None).expect("open Data.p4k");
    let db = Database::from_bytes(&data.dcb).expect("parse Game2.dcb");
    let loc = Localization::parse(&data.ini);
    let mut res = RefResolver::new(&db);
    let mut all = Vec::new();
    for (name, rec) in res.records_of_type("StarMapObject") {
        let Ok(v) = res.materialize(&rec) else { continue };
        let root = record_value(&v);
        let mut o = Map::new();
        o.insert("record".into(), json!(name));
        if let Some(map) = root.as_object() {
            for (k, x) in map {
                match x {
                    Value::String(s) if !s.is_empty() => {
                        o.insert(k.clone(), json!(s));
                        if s.starts_with('@') {
                            o.insert(format!("{k}_text"), json!(loc.text_opt(s)));
                        }
                    }
                    Value::Bool(_) | Value::Number(_) => {
                        o.insert(k.clone(), x.clone());
                    }
                    Value::Object(m) => {
                        if let Some(n) = m.get("$name").or_else(|| m.get("_RecordName_")) {
                            o.insert(k.clone(), n.clone());
                        } else if k == "locationParams" || k == "respawnLocationParams" || k.to_ascii_lowercase().contains("spawn") {
                            o.insert(k.clone(), x.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        all.push(Value::Object(o));
    }
    std::fs::write(&out, serde_json::to_string(&all).expect("json")).expect("write");
    println!("{} records -> {}", all.len(), out.display());
}
