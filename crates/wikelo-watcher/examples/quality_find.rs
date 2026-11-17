//! Diagnostic: which items of the dataset carry quality values, and which mined or gathered
//! ones do not. Runs the normalizer only (no images), against one channel folder.
//!
//! `cargo run --manifest-path watcher/Cargo.toml -p wikelo-watcher --example quality_find -- <channel dir, e.g. …\StarCitizen\LIVE>`

use std::path::PathBuf;

use starbreaker_datacore::Database;
use wikelo_watcher::GameData;
use wikelo_watcher::loc::Localization;
use wikelo_watcher::normalize::Normalizer;
use wikelo_watcher::resolver::RefResolver;

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: quality_find <channel dir>"));
    let install = wikelo_install::install_from_channel_dir(&dir).expect("read the install");
    let data = GameData::open(&install.p4k_path, None).expect("open Data.p4k");
    let db = Database::from_bytes(&data.dcb).expect("parse Game2.dcb");
    let loc = Localization::parse(&data.ini);
    let mut res = RefResolver::new(&db);
    let ds = Normalizer::new(&mut res, &loc).run(install.build_info(), String::new()).expect("normalize");

    let with: Vec<_> = ds.items.iter().filter(|i| !i.qualities.is_empty()).collect();
    println!("# {} of {} items have quality values", with.len(), ds.items.len());
    for it in &with {
        println!("  {:<12} {:<28} {:?}", it.kind, it.name, it.qualities);
    }
    println!("\n# commodities and harvestables without them");
    for it in ds.items.iter().filter(|i| i.qualities.is_empty() && matches!(i.kind.as_str(), "commodity" | "harvestable")) {
        println!("  {:<12} {:<28} {}", it.kind, it.name, it.class_name);
    }
    println!("\n# recipe ingredients with a minimum quality");
    for r in &ds.crafting {
        for slot in &r.slots {
            for o in slot.options.iter().filter(|o| o.min_quality > 0) {
                let known = o.item_id.as_deref().and_then(|id| ds.item(id)).map(|i| i.qualities.clone()).unwrap_or_default();
                println!("  {:<40} {:<14} >= {:<4} item values {:?}", r.output_name, o.name, o.min_quality, known);
            }
        }
    }
}
