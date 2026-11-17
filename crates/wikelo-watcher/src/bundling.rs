//! Build a bundle for one install: used by `wikelink bundle`, `wikelink hash`
//! and by the background watcher when it sends a new game version to the server.

use wikelo_install::InstallInfo;
use wikelo_watcher::bundle::{Author, DATASET_FILE, MANIFEST_FORMAT, Manifest, build_bundle, file_name, safe_entry};
use wikelo_watcher::{ExtractOutput, ExtractProgress, extract};

pub struct Built {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub label: String,
    pub contracts: usize,
    pub items: usize,
    pub images: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
    /// Reproducible identity of the dataset (`wikelo_core::canonical_hash`).
    pub content_hash: String,
    pub extractor: Option<wikelo_core::ExtractorInfo>,
}

/// Name recorded as the author of bundles made by hand: the one given (`--name`), else
/// `player`. Never the PC's user name: the bundle is a file that gets shared.
pub fn author_name(name: Option<String>) -> String {
    name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| DEFAULT_AUTHOR.into())
}

/// The author of a bundle made by hand without `--name` (the same word in every language:
/// it travels inside the file).
const DEFAULT_AUTHOR: &str = "player";

/// Extract the install's contracts and images and zip them behind a manifest, without a
/// signature. `name`/`handle` are recorded as the author (the signed-in account, if any).
pub fn make_bundle(install: &InstallInfo, name: &str, handle: &str, on_progress: &dyn Fn(ExtractProgress)) -> anyhow::Result<Built> {
    let ExtractOutput { dataset, images } = extract(install, on_progress)?;
    let label = wikelo_core::version_label(&dataset.build.branch, &dataset.build.version);
    let content_hash = wikelo_core::canonical_hash(&dataset);
    let mut entries: Vec<(String, Vec<u8>)> = vec![(DATASET_FILE.to_string(), serde_json::to_vec(&dataset)?)];
    let mut skipped = 0;
    for img in images {
        let path = img.file.replace('\\', "/");
        if safe_entry(&path).is_some() {
            entries.push((path, img.bytes));
        } else {
            skipped += 1;
        }
    }
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        channel: dataset.build.channel.to_ascii_uppercase(),
        version: dataset.build.version.clone(),
        label: label.clone(),
        created_at: time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        author: Author { name: name.to_string(), handle: handle.to_string(), public_key: None, mode: "watcher".into() },
        files: Vec::new(),
        content_hash: Some(content_hash.clone()),
        extractor: dataset.extractor.clone(),
    };
    let bytes = build_bundle(&entries, &mut manifest, None)?;
    Ok(Built {
        bytes,
        file_name: file_name(&dataset.build.channel, &label),
        label,
        contracts: dataset.contracts.len(),
        items: dataset.items.len(),
        images: entries.len() - 1,
        skipped,
        warnings: dataset.warnings,
        content_hash,
        extractor: dataset.extractor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_by_hand_is_signed_player_unless_named() {
        assert_eq!(author_name(None), "player");
        assert_eq!(author_name(Some("   ".into())), "player", "a blank name is no name");
        assert_eq!(author_name(Some("Test Pilot".into())), "Test Pilot", "an explicit name wins");
    }
}
