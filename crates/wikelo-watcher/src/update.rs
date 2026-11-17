//! Updating Wikelink in place, only when the player asks for it: the settings window's
//! "Update" button and `wikelink update`. Nothing here runs by itself. Both ask first, naming
//! the installed version, the new one and where the file comes from (`--yes` on the command
//! line is that answer, given in advance by whoever typed it), and nothing is downloaded before.
//!
//! What [`install`] does, in order:
//! 1. takes the version number the site reports and refuses it unless it is a plain
//!    `digits.digits.digits` newer than this executable (never the same, never older);
//! 2. opens `<this exe>.new` next to this executable: a folder it cannot write to stops here;
//! 3. downloads `SHA256SUMS.txt` and `wikelink.exe` of that release from the address made of
//!    [`agent::RELEASES_REPO`] and the version (never an address the site sends), with an HTTP
//!    client of its own: no session token, no cookies, https only, redirects followed by hand
//!    and only to [`DOWNLOAD_HOSTS`];
//! 4. checks the download: its SHA-256 must be the one `SHA256SUMS.txt` lists for
//!    `wikelink.exe`, and it must look like a Windows program (`MZ`, at least 1 MiB). That
//!    proves the file is the one published in that release of the official repository,
//!    fetched over TLS; it is not a code signature;
//! 5. stops the background watcher and the overlay the way `wikelink stop` does;
//! 6. renames this executable to `<name>.old` and the download to `<name>` (Windows cannot
//!    overwrite a running program but can rename it); when the second rename fails, the first
//!    is undone;
//! 7. starts the new file: the background watcher again if it was running, and the settings
//!    window when the update began there. The caller then ends this process.
//!
//! Any failure before step 6 leaves the installed program as it was and deletes the download.
//! The next start of any Wikelink deletes a leftover `.old` / `.new` (see [`clean_leftovers`]).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use wikelo_core::PKG_VERSION;

use crate::agent;
use crate::{tr, trf};

/// The release file this updates to, and the checksums published next to it by the
/// repository's release workflow (`.github/workflows/release.yml`).
const EXE_ASSET: &str = "wikelink.exe";
const SUMS_ASSET: &str = "SHA256SUMS.txt";

/// The only hosts a download may be redirected to: `github.com` itself (where every download
/// starts) and the hosts GitHub serves release files from (it answers a release-asset address
/// with a redirect to one of them). Any other host ends the download.
const DOWNLOAD_HOSTS: [&str; 3] = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// Most redirects followed for one file.
const MAX_REDIRECTS: usize = 5;

/// Most bytes read of `SHA256SUMS.txt` (one line today).
const SUMS_LIMIT: u64 = 64 * 1024;
/// Most bytes read of `wikelink.exe`, counted as they arrive whatever the headers say.
const EXE_LIMIT: u64 = 200 * 1024 * 1024;
/// Least size of a real `wikelink.exe`: anything smaller is an error page, not the program.
const EXE_MIN: u64 = 1024 * 1024;

/// Longest wait for a connection, and for the head of each answer.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// Longest time for each whole file, redirects and body included.
const SUMS_WAIT: Duration = Duration::from_secs(120);
const EXE_WAIT: Duration = Duration::from_secs(20 * 60);
/// Bytes read at a time.
const CHUNK: usize = 64 * 1024;
const MIB: u64 = 1024 * 1024;

// ----- versions and addresses ------------------------------------------------------------

/// `digits.digits.digits` and nothing else (no `v`, no suffix, no spaces), each part 1 to 9
/// digits.
fn is_plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| (1..=9).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The version to update to: `offered` (from the site) when it is a plain version newer than
/// `running`; else why not.
pub fn target_version(offered: &str, running: &str) -> Result<String, String> {
    if !is_plain_version(offered) {
        let shown = shown(offered);
        return Err(trf!("\"{shown}\" is not a release version number", "\"{shown}\" não é um número de versão de release"));
    }
    if !wikelo_core::version::is_newer(offered, running) {
        return Err(trf!("{offered} is not newer than this Wikelink ({running})", "{offered} não é mais nova que este Wikelink ({running})"));
    }
    Ok(offered.to_string())
}

/// Where the release files of `version` (already checked by [`target_version`]) are.
fn asset_url(version: &str, asset: &str) -> String {
    let repo = agent::RELEASES_REPO;
    format!("https://github.com/{repo}/releases/download/v{version}/{asset}")
}

/// The release's page, for downloading by hand.
pub fn release_page(version: &str) -> String {
    let repo = agent::RELEASES_REPO;
    format!("https://github.com/{repo}/releases/tag/v{version}")
}

/// What the confirmation says: both versions and where the file comes from.
pub fn confirm_text(installed: &str, new: &str) -> String {
    let from = format!("github.com/{}", agent::RELEASES_REPO);
    trf!(
        "Wikelink {installed} → {new}, downloaded from {from} and checked before it replaces this file",
        "Wikelink {installed} → {new}, baixado de {from} e conferido antes de substituir este arquivo"
    )
}

/// `s` as it may be shown in a message: no control characters, at most 120 characters.
fn shown(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// A redirect's `Location` when a download may follow it: an absolute `https://` address on
/// one of [`DOWNLOAD_HOSTS`] (no user info, no port but 443, a path), printable ASCII only.
/// The answer is rebuilt from the checked parts. Relative addresses are refused: GitHub sends
/// absolute ones.
fn redirect_target(location: &str) -> Option<String> {
    let loc = location.trim();
    if loc.len() > 8192 || !loc.bytes().all(|b| b.is_ascii_graphic()) || loc.contains('\\') {
        return None;
    }
    let (scheme, rest) = loc.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let (authority, tail) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    if authority.contains('@') || !tail.starts_with('/') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, "443")) => host,
        Some(_) => return None,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    DOWNLOAD_HOSTS.contains(&host.as_str()).then(|| format!("https://{host}{tail}"))
}

/// What to do with one answer of a download.
#[derive(Debug, PartialEq, Eq)]
enum Hop {
    /// Read its body: the file.
    Body,
    /// Ask this address next.
    Follow(String),
    /// Stop, and why.
    Refuse(String),
}

/// The rule for one answer, `hops` redirects into the download.
fn next_hop(status: u16, location: Option<&str>, hops: usize) -> Hop {
    match status {
        200 => Hop::Body,
        301 | 302 | 303 | 307 | 308 if hops >= MAX_REDIRECTS => Hop::Refuse(tr!("too many redirects", "redirecionamentos demais").into()),
        301 | 302 | 303 | 307 | 308 => match location.and_then(redirect_target) {
            Some(next) => Hop::Follow(next),
            None => {
                let to = shown(location.unwrap_or(""));
                Hop::Refuse(trf!("redirected outside GitHub's downloads ({to})", "redirecionado para fora dos downloads do GitHub ({to})"))
            }
        },
        404 => Hop::Refuse(tr!("not in this release (HTTP 404)", "não existe nesta release (HTTP 404)").into()),
        s => Hop::Refuse(format!("HTTP {s}")),
    }
}

// ----- checking ----------------------------------------------------------------------------

/// Why a download is not the release's file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckError {
    /// `SHA256SUMS.txt` is not text.
    SumsNotText,
    /// It has no line for the file.
    NoEntry,
    /// It has more than one.
    SeveralEntries,
    /// The line for the file has no 64-digit hex hash.
    BadEntry,
    /// The download is smaller than [`EXE_MIN`] (its size).
    TooSmall(u64),
    /// It does not start like a Windows program (`MZ`).
    NotAProgram,
    /// Its SHA-256 is not the listed one.
    Mismatch { expected: String, got: String },
}

impl CheckError {
    fn message(&self) -> String {
        match self {
            CheckError::SumsNotText => trf!("{SUMS_ASSET} of this release is not text", "o {SUMS_ASSET} desta release não é texto"),
            CheckError::NoEntry => trf!("{SUMS_ASSET} of this release does not list {EXE_ASSET}", "o {SUMS_ASSET} desta release não traz o {EXE_ASSET}"),
            CheckError::SeveralEntries => trf!("{SUMS_ASSET} of this release lists {EXE_ASSET} more than once", "o {SUMS_ASSET} desta release traz o {EXE_ASSET} mais de uma vez"),
            CheckError::BadEntry => trf!("the {EXE_ASSET} line of {SUMS_ASSET} has no valid SHA-256", "a linha do {EXE_ASSET} no {SUMS_ASSET} não tem um SHA-256 válido"),
            CheckError::TooSmall(n) => trf!("the download has only {n} bytes: it is not Wikelink", "o download tem só {n} bytes: não é o Wikelink"),
            CheckError::NotAProgram => tr!("the download is not a Windows program", "o download não é um programa do Windows").into(),
            CheckError::Mismatch { expected, got } => {
                let (expected, got) = (&expected[..expected.len().min(16)], &got[..got.len().min(16)]);
                trf!(
                    "the download does not match {SUMS_ASSET} (SHA-256 {got}…, listed {expected}…)",
                    "o download não bate com o {SUMS_ASSET} (SHA-256 {got}…, listado {expected}…)"
                )
            }
        }
    }
}

/// The SHA-256 (lowercase hex) that `sums`, a `SHA256SUMS.txt`, lists for `asset`. Read as
/// `sha256sum` writes it and as the release workflow does (`<hash>  wikelink.exe`, CRLF):
/// a byte-order mark, CR, blank lines and `#` comments are ignored, the hash may be upper or
/// lower case, the name may carry the `*` binary marker and a path (`dist/`, `dist\`). Lines
/// for other files are skipped; the file must have exactly one line for `asset`.
fn expected_sha256(sums: &[u8], asset: &str) -> Result<String, CheckError> {
    let text = std::str::from_utf8(sums).map_err(|_| CheckError::SumsNotText)?;
    let mut found: Vec<String> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((hash, rest)) = line.split_once(char::is_whitespace) else { continue };
        let name = rest.trim_start().trim_start_matches('*');
        let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if !file.eq_ignore_ascii_case(asset) {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CheckError::BadEntry);
        }
        found.push(hash.to_ascii_lowercase());
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(CheckError::NoEntry),
        _ => Err(CheckError::SeveralEntries),
    }
}

/// What has arrived of a download so far: its SHA-256 as it goes, its size and first bytes.
#[derive(Default)]
struct Received {
    hasher: Sha256,
    bytes: u64,
    head: [u8; 2],
    head_len: usize,
}

impl Received {
    fn feed(&mut self, chunk: &[u8]) {
        let take = (self.head.len() - self.head_len).min(chunk.len());
        self.head[self.head_len..self.head_len + take].copy_from_slice(&chunk[..take]);
        self.head_len += take;
        self.hasher.update(chunk);
        self.bytes += chunk.len() as u64;
    }

    /// The whole download against `expected` (from [`expected_sha256`]).
    fn check(self, expected: &str) -> Result<(), CheckError> {
        if self.bytes < EXE_MIN {
            return Err(CheckError::TooSmall(self.bytes));
        }
        if self.head_len < 2 || self.head != *b"MZ" {
            return Err(CheckError::NotAProgram);
        }
        let got: String = self.hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != expected {
            return Err(CheckError::Mismatch { expected: expected.to_string(), got });
        }
        Ok(())
    }
}

// ----- the file swap ------------------------------------------------------------------------

/// The three names of an update, next to the running executable `exe`: the download goes to
/// `<name>.new`, the running file moves to `<name>.old`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Swap {
    exe: PathBuf,
    new: PathBuf,
    old: PathBuf,
}

/// How [`Swap::apply`] failed.
#[derive(Debug)]
enum SwapError {
    /// The running file could not be moved aside: nothing changed.
    NotMoved(std::io::Error),
    /// The new file could not take its place; the running file is back where it was.
    RolledBack(std::io::Error),
    /// Neither the new file nor the old one is in place (both renames failed).
    Stuck { install: std::io::Error, restore: std::io::Error },
}

impl SwapError {
    fn message(&self, swap: &Swap) -> String {
        let exe = swap.exe.display();
        match self {
            SwapError::NotMoved(e) => trf!("could not move {exe} aside ({e}); nothing was changed", "não deu para tirar {exe} do lugar ({e}); nada foi alterado"),
            SwapError::RolledBack(e) => trf!(
                "could not put the new version in place ({e}); the installed one was put back",
                "não deu para colocar a versão nova no lugar ({e}); a instalada foi recolocada"
            ),
            SwapError::Stuck { install, restore } => {
                let (old, new) = (swap.old.display(), swap.new.display());
                trf!(
                    "could not put the new version in place ({install}) nor the installed one back ({restore}). Rename {old} (the installed one) or {new} (the new one) to {exe} by hand.",
                    "não deu para colocar a versão nova no lugar ({install}) nem recolocar a instalada ({restore}). Renomeie {old} (a instalada) ou {new} (a nova) para {exe} à mão."
                )
            }
        }
    }
}

impl Swap {
    fn for_exe(exe: &Path) -> Option<Self> {
        let name = exe.file_name()?;
        let with = |suffix: &str| {
            let mut n = name.to_os_string();
            n.push(suffix);
            exe.with_file_name(n)
        };
        Some(Self { exe: exe.to_path_buf(), new: with(".new"), old: with(".old") })
    }

    /// `exe` → `old`, then `new` → `exe`; when the second fails, `old` → `exe` again. A `.old`
    /// left by an earlier update is deleted first (when its program still runs, that fails and
    /// so does the first rename: nothing changes). `rename` is `std::fs::rename` outside tests.
    fn apply(&self, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Result<(), SwapError> {
        let _ = std::fs::remove_file(&self.old);
        rename(&self.exe, &self.old).map_err(SwapError::NotMoved)?;
        if let Err(install) = rename(&self.new, &self.exe) {
            return Err(match rename(&self.old, &self.exe) {
                Ok(()) => SwapError::RolledBack(install),
                Err(restore) => SwapError::Stuck { install, restore },
            });
        }
        Ok(())
    }

    /// Delete what an earlier update left: `.new` (a download that did not finish or was
    /// refused) and `.old` (the program it replaced). Silent; `true` when `.old` is still there
    /// (its program has not ended yet).
    fn remove_leftovers(&self) -> bool {
        let _ = std::fs::remove_file(&self.new);
        let _ = std::fs::remove_file(&self.old);
        self.old.exists()
    }
}

/// The rename [`Swap::apply`] does outside tests.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// `\\?\C:\…` (what `canonicalize` gives on Windows) as `C:\…`, the form the "start with
/// Windows" entry and the processes use; anything else as it is.
fn plain_path(p: PathBuf) -> PathBuf {
    let stripped = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")).filter(|rest| rest.as_bytes().get(1) == Some(&b':')).map(PathBuf::from);
    stripped.unwrap_or(p)
}

/// This executable's file, links resolved.
fn running_exe() -> std::io::Result<PathBuf> {
    Ok(plain_path(std::env::current_exe()?.canonicalize()?))
}

/// At the start of every Wikelink process: delete a `.old` / `.new` an earlier update left next
/// to this executable. Best effort and silent. The program an update replaced may still be
/// ending when the new one starts, so `.old` gets a few more tries on a thread of its own.
pub fn clean_leftovers() {
    let Some(swap) = running_exe().ok().as_deref().and_then(Swap::for_exe) else { return };
    if swap.remove_leftovers() {
        let _ = std::thread::Builder::new().name("update-cleanup".into()).spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(500));
                if std::fs::remove_file(&swap.old).is_ok() || !swap.old.exists() {
                    break;
                }
            }
        });
    }
}

// ----- progress --------------------------------------------------------------------------------

/// Where an update is, for the window and the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Bytes of `wikelink.exe` so far, of its announced size.
    Downloading { done: u64, total: Option<u64> },
    Checking,
    Stopping,
    Replacing,
    Restarting,
}

impl Stage {
    pub fn text(&self) -> String {
        let mib = |n: u64| n as f64 / MIB as f64;
        match *self {
            Stage::Downloading { done: 0, .. } => tr!("downloading…", "baixando…").into(),
            Stage::Downloading { done, total: Some(total) } if total > 0 => {
                let pct = done.saturating_mul(100) / total;
                let (d, t) = (mib(done), mib(total));
                trf!("downloading… {pct}% ({d:.1} of {t:.1} MiB)", "baixando… {pct}% ({d:.1} de {t:.1} MiB)")
            }
            Stage::Downloading { done, .. } => {
                let d = mib(done);
                trf!("downloading… {d:.1} MiB", "baixando… {d:.1} MiB")
            }
            Stage::Checking => tr!("checking the download…", "conferindo o download…").into(),
            Stage::Stopping => tr!("stopping Wikelink in the background and the overlay…", "parando o Wikelink em segundo plano e o overlay…").into(),
            Stage::Replacing => tr!("replacing the file…", "substituindo o arquivo…").into(),
            Stage::Restarting => tr!("starting the new version…", "abrindo a versão nova…").into(),
        }
    }
}

/// When a new progress line is due: each percent, or each MiB without an announced size.
fn progress_step(done: u64, total: Option<u64>) -> u64 {
    match total {
        Some(t) if t > 0 => done.saturating_mul(100) / t,
        _ => done / MIB,
    }
}

// ----- network ---------------------------------------------------------------------------------

/// The HTTP agent of the downloads, separate from the one that talks to the site: it never
/// carries the session token (no request here sets a header), keeps no cookies, refuses plain
/// http, and follows no redirect itself (see [`next_hop`]). ureq 3 has no timeout per socket
/// read: the connection and each answer's head have their own, and each request (body
/// included) must end within `overall`.
fn download_agent(overall: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(overall))
        .timeout_connect(Some(CONNECT_WAIT))
        .timeout_recv_response(Some(ANSWER_WAIT))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into()
}

fn too_big(limit: u64) -> String {
    let mib = limit / MIB;
    let kib = limit / 1024;
    if limit >= MIB { trf!("bigger than {mib} MiB", "maior que {mib} MiB") } else { trf!("bigger than {kib} KiB", "maior que {kib} KiB") }
}

fn timed_out() -> String {
    tr!("took too long", "demorou demais").into()
}

/// Download `url` into `sink`, following redirects by [`next_hop`]'s rule, at most `limit`
/// bytes (refused on the announced size, and in any case as soon as more arrive), all within
/// `overall`. `on_bytes` hears the bytes so far and the announced size.
fn fetch(url: &str, limit: u64, overall: Duration, sink: &mut dyn FnMut(&[u8]) -> Result<(), String>, on_bytes: &mut dyn FnMut(u64, Option<u64>)) -> Result<u64, String> {
    let http = download_agent(overall);
    let deadline = Instant::now() + overall;
    let mut url = url.to_string();
    let mut hops = 0;
    loop {
        let mut res = http.get(&url).call().map_err(|e| e.to_string())?;
        let location = res.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        match next_hop(res.status().as_u16(), location.as_deref(), hops) {
            Hop::Body => {
                let total = res.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
                if total.is_some_and(|n| n > limit) {
                    return Err(too_big(limit));
                }
                let mut reader = res.body_mut().as_reader();
                let mut buf = vec![0u8; CHUNK];
                let mut done: u64 = 0;
                loop {
                    let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        return Ok(done);
                    }
                    done += n as u64;
                    if done > limit {
                        return Err(too_big(limit));
                    }
                    sink(&buf[..n])?;
                    on_bytes(done, total);
                    if Instant::now() > deadline {
                        return Err(timed_out());
                    }
                }
            }
            Hop::Follow(next) => {
                url = next;
                hops += 1;
            }
            Hop::Refuse(why) => return Err(why),
        }
        if Instant::now() > deadline {
            return Err(timed_out());
        }
    }
}

/// Open `path` for the download. Others may read it while it is written and checked, never
/// delete or change it (a Wikelink starting meanwhile deletes leftover downloads).
fn open_download(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        o.share_mode(FILE_SHARE_READ);
    }
    o.open(path)
}

fn cannot_write(dir: &Path, e: &std::io::Error) -> String {
    let dir = dir.display();
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        trf!(
            "Wikelink cannot write to {dir}. Run it once as administrator to update it, or download the new version by hand.",
            "O Wikelink não pode gravar em {dir}. Abra-o uma vez como administrador para atualizar, ou baixe a versão nova à mão."
        )
    } else {
        trf!("could not create the download in {dir}: {e}", "não deu para criar o download em {dir}: {e}")
    }
}

/// Steps 3 and 4: the release's `SHA256SUMS.txt`, then its `wikelink.exe` into `file`,
/// checked against it. `file` is flushed to disk before the check.
fn download_and_check(version: &str, file: &mut std::fs::File, progress: &dyn Fn(Stage)) -> Result<(), String> {
    progress(Stage::Downloading { done: 0, total: None });
    let mut sums: Vec<u8> = Vec::new();
    fetch(
        &asset_url(version, SUMS_ASSET),
        SUMS_LIMIT,
        SUMS_WAIT,
        &mut |chunk: &[u8]| {
            sums.extend_from_slice(chunk);
            Ok(())
        },
        &mut |_, _| {},
    )
    .map_err(|e| trf!("could not download {SUMS_ASSET}: {e}", "não deu para baixar o {SUMS_ASSET}: {e}"))?;
    let expected = expected_sha256(&sums, EXE_ASSET).map_err(|e| e.message())?;

    let mut received = Received::default();
    let mut writer = std::io::BufWriter::with_capacity(4 * CHUNK, &mut *file);
    let mut last_step: Option<u64> = None;
    fetch(
        &asset_url(version, EXE_ASSET),
        EXE_LIMIT,
        EXE_WAIT,
        &mut |chunk: &[u8]| {
            received.feed(chunk);
            writer.write_all(chunk).map_err(|e| e.to_string())
        },
        &mut |done, total| {
            let step = progress_step(done, total);
            if last_step != Some(step) {
                last_step = Some(step);
                progress(Stage::Downloading { done, total });
            }
        },
    )
    .map_err(|e| trf!("could not download {EXE_ASSET}: {e}", "não deu para baixar o {EXE_ASSET}: {e}"))?;
    writer.flush().map_err(|e| e.to_string())?;
    drop(writer);
    file.sync_all().map_err(|e| e.to_string())?;
    progress(Stage::Checking);
    received.check(&expected).map_err(|e| e.message())
}

// ----- the whole update -----------------------------------------------------------------------

/// What [`install`] did once the new file is in place.
pub struct Finished {
    pub version: String,
    /// The background watcher was running and the new version's was started.
    pub daemon_restarted: bool,
    /// A settings window of the new version was started: this process can end.
    pub window_opened: bool,
    /// What could not be started again (the update itself is done).
    pub problem: Option<String>,
}

/// Update to `offered`, already confirmed by the player (see the module's documentation).
/// `reopen_window`: start the new version's settings window. On `Err` the installed program is
/// unchanged, unless the text says otherwise (both renames failed).
pub fn install(offered: &str, reopen_window: bool, progress: &dyn Fn(Stage)) -> Result<Finished, String> {
    let version = target_version(offered, PKG_VERSION)?;
    let exe = running_exe().map_err(|e| trf!("could not find this program's file: {e}", "não deu para achar o arquivo deste programa: {e}"))?;
    let swap = Swap::for_exe(&exe).ok_or_else(|| tr!("could not find this program's file", "não deu para achar o arquivo deste programa").to_string())?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let mut file = open_download(&swap.new).map_err(|e| cannot_write(dir, &e))?;
    let discard = |file: std::fs::File| {
        drop(file);
        let _ = std::fs::remove_file(&swap.new);
    };
    if let Err(e) = download_and_check(&version, &mut file, progress) {
        discard(file);
        return Err(e);
    }

    progress(Stage::Stopping);
    let overlay_was_open = agent::overlay_running();
    let daemon_was_running = agent::running_pid().is_some();
    let stopped = agent::close_overlay().and_then(|_| agent::stop_background());
    if let Err(e) = stopped {
        discard(file);
        // Back as it was: a watcher that did not stop in time keeps running, and the overlay
        // comes back if it was open.
        let _ = std::fs::remove_file(agent::stop_path());
        if overlay_was_open {
            let _ = agent::launch_overlay();
        }
        return Err(trf!("{e:#}; nothing was changed", "{e:#}; nada foi alterado"));
    }

    progress(Stage::Replacing);
    drop(file);
    if let Err(e) = swap.apply(&mut rename) {
        let msg = e.message(&swap);
        if !matches!(e, SwapError::Stuck { .. }) {
            // The installed program is in its place: back as it was.
            let _ = std::fs::remove_file(&swap.new);
            if daemon_was_running {
                let _ = agent::start_background();
            } else if overlay_was_open {
                let _ = agent::launch_overlay();
            }
        }
        return Err(msg);
    }

    progress(Stage::Restarting);
    let mut problems: Vec<String> = Vec::new();
    let mut daemon_restarted = false;
    if daemon_was_running {
        // The new background watcher brings the overlay up itself.
        match agent::spawn_detached(&swap.exe, ["start", "--quiet"], None) {
            Ok(()) => daemon_restarted = true,
            Err(e) => problems.push(trf!("could not start Wikelink in the background again: {e:#}", "não deu para iniciar o Wikelink em segundo plano de novo: {e:#}")),
        }
    } else if overlay_was_open {
        // An overlay opened without the background watcher: the new version's, as
        // `agent::launch_overlay` would start it (a pending close request cleared first).
        let _ = std::fs::remove_file(agent::overlay_stop_path());
        let _ = std::fs::remove_file(agent::overlay_closed_path());
        if let Err(e) = agent::spawn_detached(&swap.exe, ["overlay"], None) {
            problems.push(trf!("could not open the overlay again: {e:#}", "não deu para abrir o overlay de novo: {e:#}"));
        }
    }
    let mut window_opened = false;
    if reopen_window {
        match agent::spawn_detached(&swap.exe, std::iter::empty::<&str>(), None) {
            Ok(()) => window_opened = true,
            Err(e) => problems.push(trf!("could not open the new version: {e:#}", "não deu para abrir a versão nova: {e:#}")),
        }
    }
    Ok(Finished { version, daemon_restarted, window_opened, problem: (!problems.is_empty()).then(|| problems.join(" · ")) })
}

/// `wikelink update`: ask the site for the newest version and, when it is newer, say what
/// would happen and ask (`yes`: the player answered in advance). Nothing newer: says so, `Ok`.
pub fn command(yes: bool) -> anyhow::Result<()> {
    let mine = PKG_VERSION;
    let server = agent::server_address();
    let latest = agent::latest_version().map_err(|e| anyhow::anyhow!(trf!("could not ask {server} for the newest version: {e}", "não deu para perguntar a versão mais nova a {server}: {e}")))?;
    let Some(offered) = latest.filter(|v| wikelo_core::version::is_newer(v, mine)) else {
        eprintln!("{}", trf!("Wikelink {mine} is up to date.", "O Wikelink {mine} está em dia."));
        return Ok(());
    };
    let version = target_version(&offered, mine).map_err(anyhow::Error::msg)?;
    let exe = running_exe()?;
    eprintln!("{}", confirm_text(mine, &version));
    let (path, page) = (exe.display(), release_page(&version));
    eprintln!("{}", trf!("File: {path}", "Arquivo: {path}"));
    eprintln!("{}", tr!("Wikelink in the background and the overlay stop for a moment and start again.", "O Wikelink em segundo plano e o overlay param por um momento e voltam em seguida."));
    let accepted = yes || {
        let answer = agent::prompt(&format!("{} [{}] ", tr!("Update now?", "Atualizar agora?"), tr!("y/N", "s/N")))?.to_ascii_lowercase();
        matches!(answer.as_str(), "y" | "yes" | "s" | "sim")
    };
    if !accepted {
        eprintln!("{}", tr!("Nothing was changed.", "Nada foi alterado."));
        return Ok(());
    }
    let inline = std::cell::Cell::new(false);
    let show = |s: Stage| {
        if matches!(s, Stage::Downloading { .. }) {
            eprint!("\r{}    ", s.text());
            inline.set(true);
        } else {
            if inline.replace(false) {
                eprintln!();
            }
            eprintln!("{}", s.text());
        }
    };
    let done = install(&version, false, &show).map_err(|e| anyhow::anyhow!(trf!("{e}\nTo update by hand: {page}", "{e}\nPara atualizar à mão: {page}")))?;
    let v = &done.version;
    eprintln!("{}", trf!("Wikelink updated to {v}.", "Wikelink atualizado para a {v}."));
    if done.daemon_restarted {
        eprintln!("{}", tr!("Wikelink is running in the background again.", "O Wikelink voltou a rodar em segundo plano."));
    }
    if let Some(p) = done.problem {
        eprintln!("{p}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_newer_versions_are_taken() {
        for ok in ["1.2.3", "0.0.1", "10.20.30", "123456789.0.0"] {
            assert!(is_plain_version(ok), "{ok}");
        }
        for bad in ["v1.2.3", "1.2", "1.2.3.4", "1.2.3-rc1", " 1.2.3", "1.2.3 ", "1..3", "", "1.2.x", "+1.2.3", "１.2.3", "1234567890.0.0", "1.2.3/../x"] {
            assert!(!is_plain_version(bad), "{bad}");
        }
        assert_eq!(target_version("1.1.0", "1.0.0"), Ok("1.1.0".to_string()));
        assert_eq!(target_version("1.0.10", "1.0.9"), Ok("1.0.10".to_string()), "numeric, not lexical");
        assert!(target_version("1.0.0", "1.0.0").is_err(), "never the same version");
        assert!(target_version("0.9.9", "1.0.0").is_err(), "never a downgrade");
        assert!(target_version("v1.1.0", "1.0.0").is_err());
        assert!(target_version("1.1.0-rc1", "1.0.0").is_err());
    }

    #[test]
    fn downloads_come_from_the_release_of_the_repository() {
        assert_eq!(asset_url("1.2.3", EXE_ASSET), "https://github.com/lfccs/wikelink/releases/download/v1.2.3/wikelink.exe");
        assert_eq!(asset_url("1.2.3", SUMS_ASSET), "https://github.com/lfccs/wikelink/releases/download/v1.2.3/SHA256SUMS.txt");
        assert_eq!(release_page("1.2.3"), "https://github.com/lfccs/wikelink/releases/tag/v1.2.3");
        assert_eq!(redirect_target(&asset_url("1.2.3", EXE_ASSET)).as_deref(), Some("https://github.com/lfccs/wikelink/releases/download/v1.2.3/wikelink.exe"), "the first address keeps the rule too");
        assert_eq!(
            confirm_text("1.0.0", "1.1.0"),
            "Wikelink 1.0.0 → 1.1.0, baixado de github.com/lfccs/wikelink e conferido antes de substituir este arquivo"
        );
    }

    #[test]
    fn redirects_go_to_githubs_download_hosts_only() {
        let signed = "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sp=r&sv=2018-11-09&sig=abc%2Bdef%3D&response-content-disposition=attachment%3B%20filename%3Dwikelink.exe";
        assert_eq!(redirect_target(signed).as_deref(), Some(signed));
        assert_eq!(redirect_target("https://objects.githubusercontent.com/x/y?z=1").as_deref(), Some("https://objects.githubusercontent.com/x/y?z=1"));
        assert_eq!(redirect_target("HTTPS://GitHub.com/a").as_deref(), Some("https://github.com/a"), "scheme and host in any case");
        assert_eq!(redirect_target("https://github.com:443/a").as_deref(), Some("https://github.com/a"));
        for bad in [
            "http://github.com/a",
            "https://evil.example/a",
            "https://github.com.evil.example/a",
            "https://evilgithub.com/a",
            "https://evil.example/github.com/a",
            "https://github.com@evil.example/a",
            "https://user:pw@github.com/a",
            "https://github.com:8443/a",
            "https://[::1]/a",
            "https://github.com",
            "https://github.com?x=1",
            "/relative/path",
            "//github.com/a",
            "https://github.com/a b",
            "https://github.com/a\\b",
            "https://github.com/\u{7f}",
            "ftp://github.com/a",
            "",
        ] {
            assert_eq!(redirect_target(bad), None, "{bad}");
        }
    }

    #[test]
    fn each_answer_is_read_followed_or_refused() {
        let gh = "https://objects.githubusercontent.com/f";
        assert_eq!(next_hop(200, None, 0), Hop::Body);
        for status in [301, 302, 303, 307, 308] {
            assert_eq!(next_hop(status, Some(gh), 0), Hop::Follow(gh.to_string()), "{status}");
        }
        assert_eq!(next_hop(302, Some(gh), MAX_REDIRECTS - 1), Hop::Follow(gh.to_string()));
        assert!(matches!(next_hop(302, Some(gh), MAX_REDIRECTS), Hop::Refuse(_)), "at most five redirects");
        assert!(matches!(next_hop(302, None, 0), Hop::Refuse(_)), "a redirect without a place");
        assert!(matches!(next_hop(302, Some("https://evil.example/f"), 0), Hop::Refuse(_)));
        assert!(matches!(next_hop(302, Some("http://github.com/f"), 0), Hop::Refuse(_)));
        for status in [204, 206, 304, 404, 403, 500] {
            assert!(matches!(next_hop(status, Some(gh), 0), Hop::Refuse(_)), "{status}");
        }
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A stand-in for `wikelink.exe`: `MZ`, then filler up to `size` bytes.
    fn fake_exe(size: usize) -> Vec<u8> {
        let mut exe = b"MZ".to_vec();
        exe.resize(size, 0x5a);
        exe
    }

    /// The download check, on bytes fed in uneven chunks.
    fn check_bytes(exe: &[u8], sums: &[u8]) -> Result<(), CheckError> {
        let expected = expected_sha256(sums, EXE_ASSET)?;
        let mut r = Received::default();
        let (first, rest) = exe.split_at(exe.len().min(1));
        r.feed(first);
        for chunk in rest.chunks(7919) {
            r.feed(chunk);
        }
        r.check(&expected)
    }

    #[test]
    fn the_sums_file_is_read_as_the_release_writes_it() {
        let h = "ab".repeat(32);
        let upper = h.to_ascii_uppercase();
        // What the workflow writes: `Out-File -Encoding ascii` with CRLF.
        assert_eq!(expected_sha256(format!("{h}  wikelink.exe\r\n").as_bytes(), EXE_ASSET), Ok(h.clone()));
        for text in [
            format!("{upper}  wikelink.exe\n"),
            format!("{h} *wikelink.exe"),
            format!("{h}  dist/wikelink.exe\r\n"),
            format!("{h}  dist\\wikelink.exe\r\n"),
            format!("\u{feff}{h}  wikelink.exe\r\n"),
            format!("# made by the release\r\n\r\n{}  other.zip\r\n{h}  wikelink.exe\r\n", "cd".repeat(32)),
            format!("{h}\tWIKELINK.EXE"),
        ] {
            assert_eq!(expected_sha256(text.as_bytes(), EXE_ASSET), Ok(h.clone()), "{text:?}");
        }
        assert_eq!(expected_sha256(b"", EXE_ASSET), Err(CheckError::NoEntry));
        assert_eq!(expected_sha256(format!("{h}  other.exe").as_bytes(), EXE_ASSET), Err(CheckError::NoEntry));
        assert_eq!(expected_sha256(format!("{h}  notwikelink.exe").as_bytes(), EXE_ASSET), Err(CheckError::NoEntry));
        assert_eq!(expected_sha256(format!("{h}  wikelink.exe\n{h}  wikelink.exe\n").as_bytes(), EXE_ASSET), Err(CheckError::SeveralEntries));
        assert_eq!(expected_sha256(format!("{}  wikelink.exe", &h[1..]).as_bytes(), EXE_ASSET), Err(CheckError::BadEntry), "63 digits");
        assert_eq!(expected_sha256(format!("{}g  wikelink.exe", &h[1..]).as_bytes(), EXE_ASSET), Err(CheckError::BadEntry), "not hex");
        assert_eq!(expected_sha256(&[0xff, 0xfe, 0x00], EXE_ASSET), Err(CheckError::SumsNotText));
    }

    #[test]
    fn a_download_must_be_the_listed_program() {
        let exe = fake_exe(EXE_MIN as usize + 10);
        let sums = format!("{}  wikelink.exe\r\n", hex(&exe));
        assert_eq!(check_bytes(&exe, sums.as_bytes()), Ok(()));
        assert_eq!(check_bytes(&exe, sums.to_ascii_uppercase().as_bytes()), Ok(()), "upper-case hex in the sums file");

        let mut changed = exe.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(matches!(check_bytes(&changed, sums.as_bytes()), Err(CheckError::Mismatch { .. })), "one byte off");

        let small = fake_exe(EXE_MIN as usize - 1);
        let small_sums = format!("{}  wikelink.exe", hex(&small));
        assert_eq!(check_bytes(&small, small_sums.as_bytes()), Err(CheckError::TooSmall(EXE_MIN - 1)), "even with a matching hash");
        assert_eq!(check_bytes(&[], small_sums.as_bytes()), Err(CheckError::TooSmall(0)));

        let mut zip = exe.clone();
        zip[..2].copy_from_slice(b"PK");
        let zip_sums = format!("{}  wikelink.exe", hex(&zip));
        assert_eq!(check_bytes(&zip, zip_sums.as_bytes()), Err(CheckError::NotAProgram), "even with a matching hash");

        assert_eq!(check_bytes(&exe, b"no sums here"), Err(CheckError::NoEntry));
        assert!(!CheckError::Mismatch { expected: "ab".repeat(32), got: "cd".repeat(32) }.message().is_empty());
    }

    /// `wikelink.exe` (the "running" one) and `wikelink.exe.new` (the download) in a fresh folder.
    fn folder() -> (tempfile::TempDir, Swap) {
        let dir = tempfile::tempdir().unwrap();
        let swap = Swap::for_exe(&dir.path().join("wikelink.exe")).unwrap();
        std::fs::write(&swap.exe, "installed").unwrap();
        std::fs::write(&swap.new, "downloaded").unwrap();
        (dir, swap)
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn the_swap_names_sit_next_to_the_executable() {
        let s = Swap::for_exe(Path::new("/games/Wikelink/wikelink.exe")).unwrap();
        assert_eq!(s.new, Path::new("/games/Wikelink/wikelink.exe.new"));
        assert_eq!(s.old, Path::new("/games/Wikelink/wikelink.exe.old"));
        let renamed = Swap::for_exe(Path::new("/x/Wikelink (1).exe")).unwrap();
        assert_eq!(renamed.old, Path::new("/x/Wikelink (1).exe.old"), "a renamed executable keeps its own name");
    }

    #[test]
    fn the_new_file_takes_the_place_of_the_running_one() {
        let (_dir, swap) = folder();
        std::fs::write(&swap.old, "left by an earlier update").unwrap();
        swap.apply(&mut rename).unwrap();
        assert_eq!(read(&swap.exe), "downloaded");
        assert_eq!(read(&swap.old), "installed");
        assert!(!swap.new.exists());
    }

    #[test]
    fn a_failed_second_rename_puts_the_installed_file_back() {
        let (_dir, swap) = folder();
        let mut calls = 0;
        let mut second_fails = |from: &Path, to: &Path| -> std::io::Result<()> {
            calls += 1;
            if calls == 2 { Err(std::io::Error::other("refused")) } else { std::fs::rename(from, to) }
        };
        assert!(matches!(swap.apply(&mut second_fails), Err(SwapError::RolledBack(_))));
        assert_eq!(read(&swap.exe), "installed");
        assert_eq!(read(&swap.new), "downloaded", "the download stays for the caller to delete");
        assert!(!swap.old.exists());

        // The same with a real failure: the download vanished.
        let (_dir, swap) = folder();
        std::fs::remove_file(&swap.new).unwrap();
        assert!(matches!(swap.apply(&mut rename), Err(SwapError::RolledBack(_))));
        assert_eq!(read(&swap.exe), "installed");
        assert!(!swap.old.exists());
    }

    #[test]
    fn nothing_changes_when_the_running_file_cannot_move() {
        let (_dir, swap) = folder();
        let mut refuse = |_: &Path, _: &Path| -> std::io::Result<()> { Err(std::io::Error::other("in use")) };
        assert!(matches!(swap.apply(&mut refuse), Err(SwapError::NotMoved(_))));
        assert_eq!((read(&swap.exe), read(&swap.new)), ("installed".to_string(), "downloaded".to_string()));
    }

    #[test]
    fn when_both_renames_fail_the_message_says_where_the_files_are() {
        let (_dir, swap) = folder();
        let mut calls = 0;
        let mut only_first = |from: &Path, to: &Path| -> std::io::Result<()> {
            calls += 1;
            if calls == 1 { std::fs::rename(from, to) } else { Err(std::io::Error::other("refused")) }
        };
        let err = swap.apply(&mut only_first).unwrap_err();
        assert!(matches!(err, SwapError::Stuck { .. }));
        assert_eq!(read(&swap.old), "installed");
        assert_eq!(read(&swap.new), "downloaded");
        let msg = err.message(&swap);
        for p in [&swap.old, &swap.new, &swap.exe] {
            assert!(msg.contains(&p.display().to_string()), "{msg}");
        }
    }

    #[test]
    fn leftovers_of_an_earlier_update_are_deleted() {
        let (_dir, swap) = folder();
        std::fs::write(&swap.old, "replaced program").unwrap();
        assert!(!swap.remove_leftovers());
        assert!(!swap.new.exists() && !swap.old.exists());
        assert_eq!(read(&swap.exe), "installed", "the executable itself is never touched");
        assert!(!swap.remove_leftovers(), "nothing left: nothing to do");
    }

    #[test]
    fn verbatim_drive_paths_lose_their_prefix() {
        assert_eq!(plain_path(PathBuf::from(r"\\?\C:\Games\wikelink.exe")), PathBuf::from(r"C:\Games\wikelink.exe"));
        assert_eq!(plain_path(PathBuf::from(r"\\?\UNC\server\share\wikelink.exe")), PathBuf::from(r"\\?\UNC\server\share\wikelink.exe"));
        assert_eq!(plain_path(PathBuf::from(r"C:\Games\wikelink.exe")), PathBuf::from(r"C:\Games\wikelink.exe"));
        assert_eq!(plain_path(PathBuf::from("/opt/wikelink")), PathBuf::from("/opt/wikelink"));
    }

    #[test]
    fn progress_reads_well() {
        assert_eq!(Stage::Downloading { done: 0, total: Some(20 * MIB) }.text(), "baixando…");
        assert_eq!(Stage::Downloading { done: 5 * MIB, total: Some(20 * MIB) }.text(), "baixando… 25% (5.0 de 20.0 MiB)");
        assert_eq!(Stage::Downloading { done: 3 * MIB / 2, total: None }.text(), "baixando… 1.5 MiB");
        assert_eq!(Stage::Checking.text(), "conferindo o download…");
        assert_eq!(progress_step(5 * MIB, Some(20 * MIB)), 25);
        assert_eq!(progress_step(3 * MIB, None), 3);
        assert_eq!(progress_step(3 * MIB, Some(0)), 3);
    }
}
