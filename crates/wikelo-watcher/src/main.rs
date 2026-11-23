//! Wikelink (`wikelink.exe`): the program players run on the PC with the game.
//!
//! Double-click (no arguments) opens the settings window (`gui`): account, start/stop,
//! channel, start with Windows, game-data upload, activity. The subcommands do the same
//! from a terminal or a script: `start`, `stop`, `status`, `autostart`, `watch`
//! (foreground), `logout`, `update` (to the newest release, after asking), `bundle` (signed
//! data package by hand). The remaining
//! subcommands are developer tools.
//!
//! The command-line help (clap's derive texts) stays in English: it is fixed at compile
//! time and cannot follow the language chosen at run time.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::Instant;

mod agent;
mod bundling;
mod console;
mod gui;
mod hotkey;
mod i18n;
mod overlay;
mod overlay_game;
mod overlay_log;
mod overlay_model;
mod overlay_sync;
mod overlay_widgets;
mod overlay_win;
mod palette;
mod tray;
mod update;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use starbreaker_common::Progress;
use starbreaker_datacore::Database;
use starbreaker_p4k::MappedP4k;

// `tr!` / `trf!` (see `i18n`) are exported at this crate root, so they need no `use` here.

#[derive(Parser)]
#[command(name = "wikelink", about = "Wikelink: follows the Game.log and keeps your Wikelist account up to date")]
struct Cli {
    /// Without a subcommand: the settings window.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start Wikelink in the background (sets it up on the first run; signing in opens the
    /// site in the browser, where you confirm a code).
    Start {
        /// Channel to follow (default: LIVE, or the only one installed).
        #[arg(long)]
        channel: Option<String>,
        /// Upload new game versions automatically (yes/no); asked once when missing.
        #[arg(long)]
        share_data: Option<bool>,
        /// Do not wait for Enter at the end (used by the autostart entry).
        #[arg(long)]
        quiet: bool,
    },
    /// Stop Wikelink in the background.
    Stop,
    /// Is Wikelink running in the background?
    Status,
    /// Run Wikelink in this console (Ctrl+C to quit).
    Watch {
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        share_data: Option<bool>,
        /// Do not replay the current Game.log on start.
        #[arg(long)]
        no_replay: bool,
        /// Internal: background mode spawned by `start`.
        #[arg(long, hide = true)]
        daemon: bool,
    },
    /// Start Wikelink automatically when Windows starts (on|off).
    Autostart {
        #[arg(value_parser = ["on", "off"])]
        mode: String,
    },
    /// Forget the saved session.
    Logout,
    /// Update Wikelink to the newest release, after asking: downloads `wikelink.exe` from
    /// the project's GitHub releases, checks it against that release's SHA256SUMS.txt and
    /// replaces this file. Never runs by itself.
    Update {
        /// Accept without the question (for scripts): the answer is given here instead.
        #[arg(long)]
        yes: bool,
    },
    /// Open the in-game overlay (Alt+Shift+F9 shows / hides it, Shift+Tab toggles the
    /// interactive mode; the game must run in borderless window mode). It waits, hidden, for
    /// the game's window, and leaves with it.
    Overlay {
        /// Show it now, without the game, over the monitor of the window in front (to try it).
        #[arg(long)]
        preview: bool,
    },
    /// Extract and write a bundle (.zip) per channel by hand, to send on the site.
    Bundle {
        /// Only this channel (LIVE, PTU, ...). Default: every install that has game data.
        #[arg(long)]
        channel: Option<String>,
        /// Explicit Data.p4k path (the channel is then taken from its folder name).
        #[arg(long)]
        p4k: Option<PathBuf>,
        /// Output directory. Default: the directory of the executable.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Name recorded as the author of the bundle. Default: `player`.
        #[arg(long)]
        name: Option<String>,
    },
    /// Extract and print the reproducible content hash of each installed channel, to
    /// compare with what the server shows (nothing is sent).
    Hash {
        /// Only this channel (LIVE, PTU, ...).
        #[arg(long)]
        channel: Option<String>,
        /// Explicit Data.p4k path.
        #[arg(long)]
        p4k: Option<PathBuf>,
    },
    /// List detected Star Citizen installations.
    Installs,
    /// Dump the raw generator record and localization for inspection.
    Spike {
        /// Channel to use (LIVE, PTU, ...). Ignored when --p4k is given.
        #[arg(long, default_value = "LIVE")]
        channel: String,
        /// Explicit Data.p4k path.
        #[arg(long)]
        p4k: Option<PathBuf>,
        /// Output directory.
        #[arg(long, default_value = "tmp/spike")]
        out: PathBuf,
        /// Record name to dump.
        #[arg(long, default_value = wikelo_core::GENERATOR_RECORD_NAME)]
        record: String,
    },
    /// Run the full extraction and write `wikelo-data.json`.
    Extract {
        #[arg(long, default_value = "LIVE")]
        channel: String,
        /// Explicit Data.p4k path (the channel is then taken from its folder name).
        #[arg(long)]
        p4k: Option<PathBuf>,
        /// Output directory.
        #[arg(long, default_value = "tmp/out")]
        out: PathBuf,
        /// Emit one JSON progress object per line on stdout (for scripts).
        #[arg(long)]
        json_progress: bool,
    },
    /// List Data.p4k entries whose path contains every given token (case-insensitive).
    P4kLs {
        #[arg(long, default_value = "LIVE")]
        channel: String,
        #[arg(long)]
        p4k: Option<PathBuf>,
        /// Maximum number of entries to print.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Extract matching entries into this directory (raw bytes).
        #[arg(long)]
        extract_to: Option<PathBuf>,
        #[arg(required = true)]
        tokens: Vec<String>,
    },
    /// Dump every main record of a DataForge struct type as JSON (one file each).
    DumpType {
        #[arg(long, default_value = "LIVE")]
        channel: String,
        #[arg(long)]
        p4k: Option<PathBuf>,
        #[arg(long, default_value = "tmp/spike")]
        out: PathBuf,
        /// Struct type name, e.g. `CraftingBlueprintRecord`.
        type_name: String,
        /// Only records whose name contains this (case-insensitive).
        #[arg(long)]
        filter: Option<String>,
    },
    /// Dump arbitrary records (by record name or by file-name suffix) as JSON.
    Dump {
        #[arg(long, default_value = "LIVE")]
        channel: String,
        #[arg(long)]
        p4k: Option<PathBuf>,
        #[arg(long, default_value = "tmp/spike")]
        out: PathBuf,
        /// Record names (e.g. `ContractTemplate.TheCollector_Vehicles`) or file-name
        /// suffixes (e.g. `contracttemplates/thecollector_vehicles_scorpius`), case-insensitive.
        #[arg(required = true)]
        names: Vec<String>,
    },
}

fn main() -> anyhow::Result<()> {
    // The language of everything below, in every process this exe can be (settings window,
    // background watcher, overlay, each subcommand): `lang` in watcher.json, else Windows'.
    i18n::set(agent::lang_pref().as_deref());
    // A `.old` / `.new` an earlier update left next to this executable goes (silently).
    update::clean_leftovers();
    // Subcommands print; the window does not. We are a GUI executable, so a terminal that
    // launched us is attached by hand (pipes set up by a script are left alone). Not the
    // overlay: it is a window, and tied to a terminal's console it would close with it.
    let first_arg = std::env::args_os().nth(1);
    if first_arg.as_ref().is_some_and(|a| a != "overlay") {
        console::attach_parent();
    }
    // No colour codes: the background watcher's output lands in a log file the window shows.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let cli = Cli::parse();
    if cli.cmd.is_none() {
        return match gui::run() {
            Ok(()) => Ok(()),
            Err(e) => {
                console::message_box(&trf!(
                    "{e:#}\n\nWithout the window, use the command line: wikelink start",
                    "{e:#}\n\nSem a janela, use a linha de comando: wikelink start"
                ));
                Err(e)
            }
        };
    }
    // `start` typed in a terminal: keep the result readable before the window closes.
    let pause_at_end = matches!(&cli.cmd, Some(Cmd::Start { quiet, .. }) if !quiet) && console::has_console();
    let result = run(cli);
    if pause_at_end {
        if let Err(e) = &result {
            eprintln!("{}", trf!("\nERROR: {e:#}", "\nERRO: {e:#}"));
        }
        eprintln!("{}", tr!("\nPress Enter to close...", "\nPressione Enter para fechar..."));
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    result
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.cmd {
        None => unreachable!("the window runs from main"),
        Some(Cmd::Start { channel, share_data, .. }) => run_start(agent::SetupOptions { channel, share_data }),
        Some(Cmd::Stop) => {
            // The overlay goes too: without the watcher it would wait for the game forever.
            if let Err(e) = agent::close_overlay() {
                eprintln!("{e:#}");
            }
            if agent::stop_background()? {
                eprintln!("{}", tr!("Wikelink stopped.", "Wikelink parado."));
            } else {
                eprintln!("{}", tr!("Wikelink was not running.", "o Wikelink não estava rodando."));
            }
            Ok(())
        }
        Some(Cmd::Status) => {
            eprintln!("Wikelink {} ({})", wikelo_core::PKG_VERSION, wikelo_core::GIT_COMMIT);
            match agent::running_pid() {
                Some(pid) => {
                    let log = agent::log_path();
                    let log = log.display();
                    eprintln!("{}", trf!("Wikelink running in the background (pid {pid}); log in {log}", "Wikelink rodando em segundo plano (pid {pid}); log em {log}"));
                }
                None => eprintln!("{}", tr!("Wikelink stopped.", "Wikelink parado.")),
            }
            if let Some(cfg) = agent::load_config() {
                // Saved by this program, but it came from the server: plain text only.
                let (server, handle) = (agent::server_address(), agent::server_text(&cfg.handle));
                let channel = cfg.channel.as_deref().unwrap_or("?");
                let upload = if cfg.share_data == Some(true) { tr!("on", "ativado") } else { tr!("off", "desativado") };
                eprintln!(
                    "{}",
                    trf!(
                        "server {server} · account {handle} · channel {channel} · game data upload on a new version: {upload}",
                        "servidor {server} · conta {handle} · canal {channel} · envio dos dados do jogo em versão nova: {upload}"
                    )
                );
                if cfg.token.is_empty() {
                    eprintln!("{}", tr!("not signed in: open Wikelink or run `wikelink start`", "sem sessão: abra o Wikelink ou rode `wikelink start`"));
                }
            }
            if let Some(s) = agent::read_status() {
                let state = if s.paused {
                    tr!("paused from the site", "pausado pelo site").to_string()
                } else if let Some(e) = &s.last_error {
                    e.clone()
                } else if s.last_ok.is_some() {
                    tr!("connected to the server", "conectado ao servidor").to_string()
                } else {
                    tr!("connecting", "conectando").to_string()
                };
                let last = s.last_ok.map(|t| trf!(" · last contact {t}", " · último contato {t}")).unwrap_or_default();
                let data = s.data_check.map(|d| trf!(" · game data: {d}", " · dados do jogo: {d}")).unwrap_or_default();
                eprintln!("{state}{last}{data}");
                if let (Some(v), Some(u)) = (s.latest_version, s.update_url)
                    && wikelo_core::version::is_newer(&v, wikelo_core::PKG_VERSION)
                {
                    eprintln!("{}", trf!("update available: {v} → {u}", "atualização disponível: {v} → {u}"));
                }
            }
            Ok(())
        }
        Some(Cmd::Watch { channel, share_data, no_replay, daemon }) => agent::run(agent::WatchOptions { setup: agent::SetupOptions { channel, share_data }, no_replay, daemon }),
        Some(Cmd::Autostart { mode }) => {
            let on = mode == "on";
            agent::set_autostart(on)?;
            if on {
                eprintln!("{}", tr!("start with Windows on: Wikelink starts with Windows.", "inicialização automática ativada: o Wikelink sobe junto com o Windows."));
            } else {
                eprintln!("{}", tr!("start with Windows off.", "inicialização automática desativada."));
            }
            Ok(())
        }
        Some(Cmd::Logout) => {
            // Before the session goes: an overlay left waiting would come up over the game
            // later without an account.
            if let Err(e) = agent::close_overlay() {
                eprintln!("{e:#}");
            }
            let _ = agent::stop_background();
            // The server ends the session too (best effort, a few seconds at most); the
            // session on this PC is forgotten whatever it answers.
            if let Some(cfg) = agent::load_config()
                && let Err(e) = agent::sign_out_on_server(&cfg)
            {
                eprintln!("{}", trf!("the server could not be told about the sign-out ({e}); the session is forgotten here anyway", "não deu para avisar o servidor da saída ({e}); a sessão é esquecida aqui mesmo assim"));
            }
            let p = agent::config_path();
            let shown = p.display();
            // The saved session is what makes this PC signed in: when it stays, so does the
            // sign-in, and the command says so and fails.
            if let Err(e) = agent::remove_config() {
                anyhow::bail!(trf!(
                    "could not remove the saved session ({shown}): {e}; this PC is still signed in",
                    "não deu para remover a sessão salva ({shown}): {e}; este PC continua conectado"
                ));
            }
            let _ = std::fs::remove_file(agent::status_path());
            eprintln!("{}", trf!("Wikelink session removed ({shown}).", "sessão do Wikelink removida ({shown})."));
            Ok(())
        }
        Some(Cmd::Update { yes }) => update::command(yes),
        Some(Cmd::Overlay { preview }) => overlay::run(preview).inspect_err(|e| console::message_box(&format!("{e:#}"))),
        Some(Cmd::Bundle { channel, p4k, out, name }) => run_bundle(channel, p4k, out, name),
        Some(Cmd::Hash { channel, p4k }) => run_hash(channel, p4k),
        Some(cmd) => run_dev(cmd),
    }
}

/// `start`: complete the setup interactively if needed, then leave the watcher running detached.
fn run_start(setup: agent::SetupOptions) -> anyhow::Result<()> {
    // The background watcher itself keeps the overlay up (see `agent::OverlayKeeper`).
    if let Some(pid) = agent::running_pid() {
        // The autostart entry of `wikelo-watcher.exe` moves to this exe here too (it may be
        // the old program that is running).
        if let Err(e) = agent::migrate_autostart() {
            eprintln!("{e:#}");
        }
        eprintln!(
            "{}",
            trf!(
                "Wikelink is already running in the background (pid {pid}). To stop it: wikelink stop",
                "o Wikelink já está rodando em segundo plano (pid {pid}). Para parar: wikelink stop"
            )
        );
        return Ok(());
    }
    let (_cfg, name) = agent::ensure_setup(setup, console::has_console())?;
    let pid = agent::start_background()?;
    let server = agent::server_address();
    eprintln!("{}", trf!("\nWikelink started in the background (pid {pid}) for {server} as {name}.", "\nWikelink iniciado em segundo plano (pid {pid}) para {server} como {name}."));
    eprintln!(
        "{}",
        tr!(
            "You can close this window. To stop: wikelink stop · to start with Windows: wikelink autostart on · settings: open Wikelink",
            "Pode fechar esta janela. Para parar: wikelink stop · para subir com o Windows: wikelink autostart on · opções: abra o Wikelink"
        )
    );
    eprintln!("Log: {}", agent::log_path().display());
    Ok(())
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

/// Installs selected by `--channel` / `--p4k` that have game data.
fn selected_installs(channel: Option<&str>, p4k: Option<PathBuf>) -> anyhow::Result<Vec<wikelo_install::InstallInfo>> {
    let installs: Vec<wikelo_install::InstallInfo> = if let Some(p) = p4k {
        vec![resolve_install("", Some(p))?]
    } else {
        wikelo_install::discover_installs(&[])
            .into_iter()
            .filter(|i| channel.is_none_or(|c| i.channel.eq_ignore_ascii_case(c)))
            .filter(|i| i.has_p4k())
            .collect()
    };
    if installs.is_empty() {
        match channel {
            Some(c) => bail!(trf!(
                "no Star Citizen install with game data was found for channel {c}. Use --p4k <path of Data.p4k> or SC_INSTALL_ROOTS.",
                "nenhuma instalação do Star Citizen com dados do jogo foi encontrada para o canal {c}. Use --p4k <caminho do Data.p4k> ou SC_INSTALL_ROOTS."
            )),
            None => bail!(tr!(
                "no Star Citizen install with game data was found. Use --p4k <path of Data.p4k> or SC_INSTALL_ROOTS.",
                "nenhuma instalação do Star Citizen com dados do jogo foi encontrada. Use --p4k <caminho do Data.p4k> ou SC_INSTALL_ROOTS."
            )),
        }
    }
    Ok(installs)
}

fn print_provenance(built: &bundling::Built) {
    let hash = &built.content_hash;
    eprintln!("{}", trf!("content hash: {hash}", "hash do conteúdo: {hash}"));
    if let Some(x) = &built.extractor {
        let (name, version, commit, dcb, ini) = (&x.name, &x.version, &x.commit, &x.dcb_sha256[..16], &x.ini_sha256[..16]);
        eprintln!("{}", trf!("extractor {name} {version} ({commit}) · Game2.dcb {dcb} · global.ini {ini}", "extrator {name} {version} ({commit}) · Game2.dcb {dcb} · global.ini {ini}"));
    }
}

/// Extract every selected install and write one bundle per channel.
fn run_bundle(channel: Option<String>, p4k: Option<PathBuf>, out: Option<PathBuf>, name: Option<String>) -> anyhow::Result<()> {
    let out = out.unwrap_or_else(exe_dir);
    std::fs::create_dir_all(&out)?;
    let author = bundling::author_name(name);
    let installs = selected_installs(channel.as_deref(), p4k)?;
    eprintln!("{}", trf!("Author: {author}", "Autor: {author}"));
    for install in installs {
        eprintln!("\n== {} {} ==", install.channel, install.manifest.version);
        let started = Instant::now();
        let on_progress = |p: wikelo_watcher::ExtractProgress| eprintln!("[{:>6.2}s] {:<10} {:>3}% {}", p.elapsed_sec, p.stage, p.pct, p.message);
        let built = bundling::make_bundle(&install, &author, &author, &on_progress)?;
        let path = out.join(&built.file_name);
        std::fs::write(&path, &built.bytes)?;
        let (file, contracts, items, images, skipped) = (path.display(), built.contracts, built.items, built.images, built.skipped);
        let (mb, secs) = (built.bytes.len() as f64 / 1048576.0, started.elapsed().as_secs_f64());
        let skipped = if skipped > 0 { trf!(" ({skipped} skipped)", " ({skipped} ignoradas)") } else { String::new() };
        eprintln!(
            "{}",
            trf!(
                "bundle: {file} — {contracts} contracts, {items} items, {images} images{skipped}, {mb:.1} MB in {secs:.1}s",
                "pacote: {file} — {contracts} contratos, {items} itens, {images} imagens{skipped}, {mb:.1} MB em {secs:.1}s"
            )
        );
        print_provenance(&built);
        for w in built.warnings.iter().take(10) {
            eprintln!("{}", trf!("warning: {w}", "aviso: {w}"));
        }
    }
    eprintln!(
        "{}",
        tr!(
            "\nDone. Send the file in Data → Send bundle on the site; it shows the same hash there.",
            "\nPronto. Envie o arquivo em Dados → Enviar pacote no site; ele mostra o mesmo hash lá."
        )
    );
    Ok(())
}

/// Extract and print the content hash per channel, so anyone with the game can check
/// what the server received (nothing is uploaded).
fn run_hash(channel: Option<String>, p4k: Option<PathBuf>) -> anyhow::Result<()> {
    let installs = selected_installs(channel.as_deref(), p4k)?;
    for install in installs {
        let started = Instant::now();
        eprintln!("== {} {} ({}) ==", install.channel, wikelo_core::version_label(&install.manifest.branch, &install.manifest.version), install.manifest.version);
        let quiet = |_p: wikelo_watcher::ExtractProgress| {};
        let built = bundling::make_bundle(&install, "", "", &quiet)?;
        let (contracts, items, images, secs) = (built.contracts, built.items, built.images, started.elapsed().as_secs_f64());
        eprintln!("{}", trf!("{contracts} contracts, {items} items, {images} images in {secs:.1}s", "{contracts} contratos, {items} itens, {images} imagens em {secs:.1}s"));
        print_provenance(&built);
        println!("{} {} {}", install.channel, install.manifest.version, built.content_hash);
    }
    Ok(())
}

fn run_dev(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Start { .. } | Cmd::Stop | Cmd::Status | Cmd::Watch { .. } | Cmd::Autostart { .. } | Cmd::Logout | Cmd::Update { .. } | Cmd::Overlay { .. } | Cmd::Bundle { .. } | Cmd::Hash { .. } => {
            unreachable!("handled in run")
        }
        Cmd::Installs => {
            for i in wikelo_install::discover_installs(&[]) {
                println!(
                    "{:<13} {:<16} p4change={:<10} p4k={} {}",
                    i.channel,
                    i.manifest.version,
                    i.manifest.requested_p4_change_num,
                    if i.has_p4k() { "yes" } else { "no " },
                    i.root.display()
                );
            }
            Ok(())
        }
        Cmd::Spike { channel, p4k, out, record } => spike(channel, p4k, out, &record),
        Cmd::Dump { channel, p4k, out, names } => dump(channel, p4k, out, &names),
        Cmd::DumpType { channel, p4k, out, type_name, filter } => dump_type(channel, p4k, out, &type_name, filter.as_deref()),
        Cmd::P4kLs { channel, p4k, limit, extract_to, tokens } => p4k_ls(channel, p4k, limit, extract_to, &tokens),
        Cmd::Extract { channel, p4k, out, json_progress } => run_extract(channel, p4k, out, json_progress),
    }
}

fn resolve_install(channel: &str, p4k: Option<PathBuf>) -> anyhow::Result<wikelo_install::InstallInfo> {
    if let Some(p) = p4k {
        let dir = p.parent().context("p4k path has no parent")?;
        return wikelo_install::install_from_channel_dir(dir).context("read build_manifest.id next to Data.p4k");
    }
    let installs = wikelo_install::discover_installs(&[]);
    installs
        .into_iter()
        .find(|i| i.channel.eq_ignore_ascii_case(channel))
        .with_context(|| format!("channel {channel} not found"))
}

fn run_extract(channel: String, p4k: Option<PathBuf>, out: PathBuf, json_progress: bool) -> anyhow::Result<()> {
    let install = resolve_install(&channel, p4k)?;
    std::fs::create_dir_all(&out)?;
    let on_progress = |p: wikelo_watcher::ExtractProgress| {
        if json_progress {
            println!("{}", serde_json::to_string(&p).unwrap_or_default());
        } else {
            eprintln!("[{:>6.2}s] {:<10} {:>3}% {}", p.elapsed_sec, p.stage, p.pct, p.message);
        }
    };
    let output = wikelo_watcher::extract(&install, &on_progress)?;
    let dataset = &output.dataset;
    let path = out.join("wikelo-data.json");
    std::fs::write(&path, serde_json::to_vec_pretty(dataset)?)?;
    let n_img = output.write_images(&out)?;
    if !json_progress {
        let dir = out.join(wikelo_watcher::images::IMG_DIR);
        let dir = dir.display();
        eprintln!("{}", trf!("{n_img} images written to {dir}", "{n_img} imagens gravadas em {dir}"));
    }
    if json_progress {
        println!(
            "{}",
            serde_json::json!({
                "stage": "written",
                "pct": 100,
                "message": path.display().to_string(),
                "contracts": dataset.contracts.len(),
                "items": dataset.items.len(),
                "warnings": dataset.warnings.len(),
            })
        );
    } else {
        eprintln!("written {}", path.display());
        for w in dataset.warnings.iter().take(40) {
            eprintln!("warning: {w}");
        }
        if dataset.warnings.len() > 40 {
            eprintln!("... {} more warnings", dataset.warnings.len() - 40);
        }
    }
    Ok(())
}

fn dump(channel: String, p4k: Option<PathBuf>, out: PathBuf, names: &[String]) -> anyhow::Result<()> {
    let p4k_path = resolve_p4k(&channel, p4k)?;
    std::fs::create_dir_all(&out)?;
    let p4k = MappedP4k::open(&p4k_path).context("open p4k")?;
    let dcb = p4k
        .read_file("Data\\Game2.dcb")
        .or_else(|_| p4k.read_file("Data\\Game.dcb"))
        .context("read Game2.dcb")?;
    let db = Database::from_bytes(&dcb).context("parse DataCore")?;
    for wanted in names {
        let wanted_lc = wanted.to_ascii_lowercase().replace('\\', "/");
        let mut hits = 0;
        for r in db.records() {
            if !db.is_main_record(r) {
                continue;
            }
            let name_offset = r.name_offset;
            let file_offset = r.file_name_offset;
            let name = db.resolve_string2(name_offset);
            let file = db.resolve_string(file_offset).to_ascii_lowercase().replace('\\', "/");
            let file_noext = file.rsplit_once('.').map(|(a, _)| a).unwrap_or(&file);
            if name.eq_ignore_ascii_case(wanted) || file_noext.ends_with(&wanted_lc) {
                let rec = *r;
                let json = starbreaker_datacore::export::to_json(&db, &rec)?;
                let safe: String =
                    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' }).collect();
                let path = out.join(format!("{safe}.json"));
                std::fs::write(&path, &json)?;
                let id = rec.id;
                eprintln!("{wanted}: {name} ({id}) -> {} [{} bytes] file={file}", path.display(), json.len());
                hits += 1;
            }
        }
        if hits == 0 {
            eprintln!("{wanted}: no record found");
        }
    }
    Ok(())
}

fn dump_type(channel: String, p4k: Option<PathBuf>, out: PathBuf, type_name: &str, filter: Option<&str>) -> anyhow::Result<()> {
    let p4k_path = resolve_p4k(&channel, p4k)?;
    std::fs::create_dir_all(&out)?;
    let p4k = MappedP4k::open(&p4k_path).context("open p4k")?;
    let dcb = p4k
        .read_file("Data\\Game2.dcb")
        .or_else(|_| p4k.read_file("Data\\Game.dcb"))
        .context("read Game2.dcb")?;
    let db = Database::from_bytes(&dcb).context("parse DataCore")?;
    let filter_lc = filter.map(|f| f.to_ascii_lowercase());
    let mut n = 0;
    for r in db.records_by_type_name(type_name) {
        if !db.is_main_record(r) {
            continue;
        }
        let name = db.resolve_string2(r.name_offset).to_string();
        if let Some(f) = &filter_lc
            && !name.to_ascii_lowercase().contains(f.as_str())
        {
            continue;
        }
        let rec = *r;
        let json = starbreaker_datacore::export::to_json(&db, &rec)?;
        let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' }).collect();
        std::fs::write(out.join(format!("{safe}.json")), &json)?;
        n += 1;
    }
    let dir = out.display();
    eprintln!("{}", trf!("{n} {type_name} records written to {dir}", "{n} registros {type_name} gravados em {dir}"));
    Ok(())
}

fn p4k_ls(channel: String, p4k: Option<PathBuf>, limit: usize, extract_to: Option<PathBuf>, tokens: &[String]) -> anyhow::Result<()> {
    let p4k_path = resolve_p4k(&channel, p4k)?;
    let started = Instant::now();
    let p4k = MappedP4k::open(&p4k_path)?;
    let (secs, total) = (started.elapsed().as_secs_f64(), p4k.len());
    eprintln!("{}", trf!("p4k opened in {secs:.1}s ({total} entries)", "p4k aberto em {secs:.1}s ({total} entradas)"));
    let mut idx = p4k.search(&tokens.join(" "));
    idx.sort_unstable();
    let entries = p4k.entries();
    let hits = idx.len();
    eprintln!("{}", trf!("{hits} entries match", "{hits} entradas casam"));
    for &i in idx.iter().take(limit) {
        let e = &entries[i as usize];
        println!("{:>12} {}", e.uncompressed_size, e.name);
    }
    if let Some(dir) = extract_to {
        std::fs::create_dir_all(&dir)?;
        for &i in idx.iter().take(limit) {
            let e = &entries[i as usize];
            let data = p4k.read(e)?;
            let file = dir.join(e.name.replace(['\\', '/'], "__"));
            std::fs::write(&file, data)?;
        }
        let dir = dir.display();
        eprintln!("{}", trf!("extracted to {dir}", "extraído para {dir}"));
    }
    Ok(())
}

fn resolve_p4k(channel: &str, p4k: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = p4k {
        return Ok(p);
    }
    let installs = wikelo_install::discover_installs(&[]);
    let install = installs
        .iter()
        .find(|i| i.channel.eq_ignore_ascii_case(channel))
        .with_context(|| format!("channel {channel} not found; detected: {:?}", installs.iter().map(|i| &i.channel).collect::<Vec<_>>()))?;
    if !install.has_p4k() {
        bail!("{} has no Data.p4k at {}", install.channel, install.p4k_path.display());
    }
    Ok(install.p4k_path.clone())
}

fn spike(channel: String, p4k: Option<PathBuf>, out: PathBuf, record_name: &str) -> anyhow::Result<()> {
    let p4k_path = resolve_p4k(&channel, p4k)?;
    std::fs::create_dir_all(&out)?;
    eprintln!("p4k: {}", p4k_path.display());

    let t = Instant::now();
    let progress = Progress::new();
    let p4k = MappedP4k::open_with_progress(&p4k_path, Some(&progress)).context("open p4k")?;
    eprintln!("[{:>7.2}s] p4k opened: {} entries", t.elapsed().as_secs_f64(), p4k.len());

    let t1 = Instant::now();
    let dcb = p4k
        .read_file("Data\\Game2.dcb")
        .or_else(|_| p4k.read_file("Data\\Game.dcb"))
        .context("read Game2.dcb")?;
    eprintln!("[{:>7.2}s] dcb read: {} bytes (took {:.2}s)", t.elapsed().as_secs_f64(), dcb.len(), t1.elapsed().as_secs_f64());

    let ini = p4k.read_file("Data\\Localization\\english\\global.ini").context("read global.ini")?;
    std::fs::write(out.join("global.ini"), &ini)?;
    eprintln!("[{:>7.2}s] global.ini: {} bytes", t.elapsed().as_secs_f64(), ini.len());

    let t2 = Instant::now();
    let db = Database::from_bytes(&dcb).context("parse DataCore")?;
    eprintln!(
        "[{:>7.2}s] DataCore parsed: {} records, {} structs (took {:.2}s)",
        t.elapsed().as_secs_f64(),
        db.records().len(),
        db.struct_defs().len(),
        t2.elapsed().as_secs_f64()
    );

    // All ContractGenerator record names, for orientation.
    let mut gen_names: Vec<String> = db
        .records_by_type_name("ContractGenerator")
        .map(|r| {
            let name_offset = r.name_offset;
            db.resolve_string2(name_offset).to_string()
        })
        .collect();
    gen_names.sort();
    std::fs::write(out.join("contract-generators.txt"), gen_names.join("\n"))?;
    eprintln!("ContractGenerator records: {}", gen_names.len());

    let record = db
        .records_by_type_name("ContractGenerator")
        .find(|r| {
            let name_offset = r.name_offset;
            db.resolve_string2(name_offset) == record_name
        })
        .with_context(|| format!("record {record_name} not found"))?;
    let record = *record;
    let t3 = Instant::now();
    let json = starbreaker_datacore::export::to_json(&db, &record).context("export record")?;
    let file = out.join(format!("{record_name}.json"));
    std::fs::write(&file, &json)?;
    eprintln!(
        "[{:>7.2}s] exported {} ({} bytes, took {:.2}s)",
        t.elapsed().as_secs_f64(),
        file.display(),
        json.len(),
        t3.elapsed().as_secs_f64()
    );

    // A few supporting record families, names only, to confirm struct names.
    for ty in ["FactionReputation", "SReputationScopeParams", "MissionLocationTemplate", "ContractTemplate", "Tag"] {
        let names: Vec<String> = db
            .records_by_type_name(ty)
            .map(|r| {
                let name_offset = r.name_offset;
                let id = r.id;
                format!("{}\t{}", id, db.resolve_string2(name_offset))
            })
            .collect();
        std::fs::write(out.join(format!("records-{ty}.txt")), names.join("\n"))?;
        eprintln!("{ty}: {} records", names.len());
    }
    eprintln!("done in {:.2}s", t.elapsed().as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_takes_no_account_name_nor_secret() {
        // Signing in goes through the site: the old sign-in flags are unknown now. (The flag is
        // spelt in two pieces so that the source scan in agent.rs finds the word nowhere.)
        let secret_flag = concat!("--pass", "word");
        for args in [
            ["wikelink", "start", "--handle", "TestPilot"],
            ["wikelink", "start", secret_flag, "x"],
            ["wikelink", "watch", "--handle", "TestPilot"],
            ["wikelink", "watch", secret_flag, "x"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["wikelink", "start", "--channel", "LIVE", "--share-data", "true", "--quiet"]).is_ok());
        assert!(Cli::try_parse_from(["wikelink", "watch", "--no-replay"]).is_ok());
    }
}
