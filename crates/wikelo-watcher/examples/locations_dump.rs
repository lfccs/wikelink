//! Diagnostic: the dataset's locations as JSON (normalizer only, no images), to check which
//! places are offered, which are hidden, the residences and the system / body tree.
//!
//! `cargo run --manifest-path watcher/Cargo.toml -p wikelo-watcher --example locations_dump -- <channel dir> <out.json>`

use std::path::PathBuf;

use starbreaker_datacore::Database;
use wikelo_watcher::GameData;
use wikelo_watcher::loc::Localization;
use wikelo_watcher::normalize::Normalizer;
use wikelo_watcher::resolver::RefResolver;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: locations_dump <channel dir> <out.json>"));
    let out = PathBuf::from(args.next().expect("usage: locations_dump <channel dir> <out.json>"));
    let install = wikelo_install::install_from_channel_dir(&dir).expect("read the install");
    let data = GameData::open(&install.p4k_path, None).expect("open Data.p4k");
    let db = Database::from_bytes(&data.dcb).expect("parse Game2.dcb");
    let loc = Localization::parse(&data.ini);
    let mut res = RefResolver::new(&db);
    let ds = Normalizer::new(&mut res, &loc).run(install.build_info(), String::new()).expect("normalize");
    let offered = ds.locations.iter().filter(|l| !l.hidden).count();
    println!("{} locations, {} offered, {} residences", ds.locations.len(), offered, ds.locations.iter().filter(|l| l.home).count());
    for l in ds.locations.iter().filter(|l| l.home) {
        println!("  home: {} / {} / {} ({})", l.system.as_deref().unwrap_or("?"), l.body.as_deref().unwrap_or("-"), l.name, l.id);
    }
    std::fs::write(&out, serde_json::to_string(&ds.locations).expect("json")).expect("write");
}
