mod api;
mod marks;
mod thumbs;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use api::{AppState, Snapshot, TrashScan};
use clap::{Args, Parser, Subcommand};
use museum_core::note::{self, LinkIndex, Resolution};
use museum_core::{
    Date, ImageEntry, Kind, Library, Manifest, Museum, Order, StateDir, museum, purge,
};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

#[derive(Parser)]
#[command(name = "lumier", about = "A museum-shaped image viewer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the museum over HTTP.
    Serve(ServeArgs),
    /// Validate museum.yaml against the library and report what it finds.
    Check(LibraryArgs),
    /// Put back what was asked for, move marked files to the trash, and empty
    /// the trash of days kept long enough. Meant to run nightly, and the only
    /// command that moves or deletes library files.
    Purge(PurgeArgs),
}

#[derive(Args)]
struct PurgeArgs {
    /// Library root.
    #[arg(long)]
    root: PathBuf,

    /// Where trashed files go. Must be on the library's filesystem, outside it.
    #[arg(long)]
    trash_dir: PathBuf,

    /// Where `serve` records marks and restore requests.
    #[arg(long)]
    state_dir: PathBuf,

    /// Days a file stays in the trash before it is deleted.
    #[arg(long, default_value_t = 30)]
    trash_days: u32,

    /// Move nothing if more than this many files are marked. A guard against
    /// marks nobody meant: the server takes them from anyone on the LAN.
    #[arg(long, default_value_t = 500)]
    limit: usize,

    /// Lift the limit for this run.
    #[arg(long)]
    no_limit: bool,
}

#[derive(Args, Clone)]
struct LibraryArgs {
    /// Library root. Every path the server touches stays under it.
    #[arg(long)]
    root: PathBuf,

    /// Manifest to read, relative to the root unless absolute.
    #[arg(long, default_value = "museum.yaml")]
    manifest: PathBuf,
}

#[derive(Args)]
struct ServeArgs {
    #[command(flatten)]
    library: LibraryArgs,

    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Built frontend to serve. Omit during development and let the Vite dev
    /// server proxy the API instead.
    #[arg(long)]
    web_dir: Option<PathBuf>,

    /// Where rendered thumbnails are kept. Safe to delete; it refills on
    /// demand. Defaults to $XDG_CACHE_HOME/lumier, else ~/.cache/lumier.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Rescan the library and re-read the manifest every this many seconds,
    /// so files added over SMB reach the walls without a restart. Off by
    /// default; SIGHUP and `POST /api/rescan` rescan on demand either way.
    #[arg(long, value_name = "SECS")]
    rescan_interval: Option<u64>,

    /// Where to record deletion marks and restore requests. Together with
    /// --trash-dir this turns marking on; without both the server stays
    /// read-only.
    #[arg(long, requires = "trash_dir")]
    state_dir: Option<PathBuf>,

    /// The trash `lumier purge` moves marked files into. Read only, to list
    /// it and offer restores.
    #[arg(long, requires = "state_dir")]
    trash_dir: Option<PathBuf>,

    /// Days a file stays in the trash, for the countdown shown on it. Keep it
    /// equal to purge's --trash-days.
    #[arg(long, default_value_t = 30)]
    trash_days: u32,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".into()),
        )
        .init();

    let result = match Cli::parse().command {
        Command::Serve(args) => serve(args).await,
        Command::Check(args) => check(&args),
        Command::Purge(args) => run_purge(&args),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            // The chain carries the useful part — which file, which room.
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Opens the library and builds the validated room graph.
///
/// Both subcommands come through here, so `serve` can never run against a
/// manifest that `check` would reject.
fn load(args: &LibraryArgs) -> anyhow::Result<Loaded> {
    let mut library = Library::open(&args.root)
        .with_context(|| format!("cannot open library root {}", args.root.display()))?;

    let manifest_path = if args.manifest.is_absolute() {
        args.manifest.clone()
    } else {
        library.root().join(&args.manifest)
    };
    let manifest = Manifest::load(&manifest_path)?;

    let validated = museum::build(&manifest, library.root()).map_err(|errors| {
        anyhow::anyhow!(
            "{} is not a valid museum:\n{}",
            manifest_path.display(),
            bullets(&errors)
        )
    })?;

    let started = std::time::Instant::now();
    let (images, unreadable) = scan_rooms(&mut library, &validated.museum);
    Ok(Loaded {
        library,
        museum: validated.museum,
        warnings: validated.warnings,
        images,
        unreadable,
        scan_duration: started.elapsed(),
    })
}

struct Loaded {
    library: Library,
    museum: Museum,
    warnings: Vec<String>,
    images: HashMap<String, Vec<ImageEntry>>,
    /// Files that looked like images but could not be probed, so are not hung.
    unreadable: usize,
    /// How long probing took. Worth printing: this is the step that slows by
    /// orders of magnitude on a network mount.
    scan_duration: std::time::Duration,
}

/// Scans each room's folder once, up front.
///
/// A folder that cannot be read is reported and left empty rather than
/// aborting: one unreadable room should not close the whole museum.
fn scan_rooms(library: &mut Library, museum: &Museum) -> (HashMap<String, Vec<ImageEntry>>, usize) {
    let mut images = HashMap::new();
    let mut unreadable = 0;
    for room in museum.rooms() {
        let Some(rel_path) = &room.rel_path else {
            continue;
        };
        match library.scan_room(rel_path, room.order) {
            Ok((entries, stats)) => {
                unreadable += stats.unreadable;
                images.insert(room.id.clone(), entries);
            }
            Err(error) => {
                tracing::error!(room = %room.id, %rel_path, %error, "cannot scan room");
            }
        }
    }
    (images, unreadable)
}

/// Rescans on SIGHUP, which is what `systemctl reload` sends, and on a
/// timer if one was asked for.
fn spawn_rescan_triggers(state: &Arc<AppState>, interval: Option<u64>) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut hangups = signal(SignalKind::hangup()).context("cannot listen for SIGHUP")?;
    let on_hangup = Arc::clone(state);
    tokio::spawn(async move {
        while hangups.recv().await.is_some() {
            log_rescan("SIGHUP", on_hangup.rescan().await);
        }
    });

    if let Some(secs) = interval.filter(|&secs| secs > 0) {
        let on_timer = Arc::clone(state);
        tokio::spawn(async move {
            let period = std::time::Duration::from_secs(secs);
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            // A scan that overruns the period should not be followed by a
            // burst of catch-up scans.
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                log_rescan("timer", on_timer.rescan().await);
            }
        });
    }
    Ok(())
}

fn log_rescan(trigger: &str, result: Result<api::RescanReport, api::RescanError>) {
    match result {
        Ok(report) => tracing::info!(trigger, ?report, "rescanned"),
        Err(api::RescanError::Busy) => {
            tracing::info!(trigger, "rescan skipped: one is already running")
        }
        Err(api::RescanError::Load(error)) => {
            tracing::error!(
                trigger,
                "rescan failed, keeping the previous museum: {error:#}"
            );
        }
    }
}

/// Links in notes that lead nowhere, or to a name several rooms share.
/// Reported by `check` only: a broken link costs a reader nothing but the
/// link, and is marked where it stands.
fn note_link_warnings(loaded: &Loaded) -> Vec<String> {
    let index = LinkIndex::new(
        loaded
            .images
            .iter()
            .map(|(room, entries)| (room.as_str(), entries.as_slice())),
    );
    let mut warnings = Vec::new();
    for room in loaded.museum.rooms() {
        let notes = loaded
            .images
            .get(&room.id)
            .into_iter()
            .flatten()
            .filter(|entry| entry.kind == Kind::Markdown);
        for entry in notes {
            let Some(path) = loaded.library.image_path(&entry.id) else {
                continue;
            };
            let Ok(markdown) = note::read(&path) else {
                warnings.push(format!(
                    "note {} in room {:?} cannot be read",
                    entry.name, room.id
                ));
                continue;
            };
            let rendered = note::render(&markdown, &|name| index.resolve(name, &room.id));
            for issue in rendered.issues {
                warnings.push(match issue.resolution {
                    Resolution::Ambiguous(count) => format!(
                        "note {} in room {:?} links to {}, a name {count} files share outside its room; none is picked",
                        entry.name, room.id, issue.name
                    ),
                    _ => format!(
                        "note {} in room {:?} links to {}, which no room hangs",
                        entry.name, room.id, issue.name
                    ),
                });
            }
        }
    }
    warnings
}

/// Scans the trash: each day folder, and everything nested in it, newest
/// day first. A missing trash is simply empty, since nothing has been
/// purged yet; one that cannot be read is logged and left out.
fn scan_trash(dir: &Path) -> Option<TrashScan> {
    if !dir.exists() {
        return None;
    }
    let mut library = match Library::open_trash(dir) {
        Ok(library) => library,
        Err(error) => {
            tracing::error!(dir = %dir.display(), %error, "cannot open the trash");
            return None;
        }
    };
    let mut day_names: Vec<(Date, String)> = match std::fs::read_dir(library.root()) {
        Ok(read) => read
            .filter_map(Result::ok)
            .filter_map(|item| item.file_name().into_string().ok())
            .filter_map(|name| Some((name.parse::<Date>().ok()?, name)))
            .collect(),
        Err(error) => {
            tracing::error!(dir = %dir.display(), %error, "cannot read the trash");
            return None;
        }
    };
    day_names.sort_by_key(|day| std::cmp::Reverse(day.0));

    let mut days = Vec::new();
    for (day, name) in day_names {
        let mut dirs = match library.subdirs(&name) {
            Ok(dirs) => dirs,
            Err(error) => {
                tracing::error!(%name, %error, "cannot read a trash day");
                continue;
            }
        };
        dirs.sort_by(|a, b| natord::compare_ignore_case(a, b));
        let mut entries = Vec::new();
        for rel_dir in dirs {
            match library.scan_room(&rel_dir, Order::Name) {
                Ok((found, _)) => entries.extend(found),
                Err(error) => tracing::error!(%rel_dir, %error, "cannot scan the trash"),
            }
        }
        days.push((day, entries));
    }
    Some(TrashScan { library, days })
}

fn run_purge(args: &PurgeArgs) -> anyhow::Result<ExitCode> {
    let root = args
        .root
        .canonicalize()
        .with_context(|| format!("cannot open library root {}", args.root.display()))?;
    std::fs::create_dir_all(&args.trash_dir)
        .with_context(|| format!("cannot create trash {}", args.trash_dir.display()))?;
    let trash = args.trash_dir.canonicalize()?;
    if trash.starts_with(&root) || root.starts_with(&trash) {
        anyhow::bail!("the trash and the library must not contain each other");
    }
    // A move into the trash must be a rename. Across filesystems it would
    // turn into a copy and a delete, which can fail halfway.
    {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(&root)?.dev() != std::fs::metadata(&trash)?.dev() {
            anyhow::bail!(
                "the trash {} is not on the library's filesystem",
                trash.display()
            );
        }
    }
    let state = StateDir::open(&args.state_dir)
        .with_context(|| format!("cannot open state directory {}", args.state_dir.display()))?;

    let report = purge::run(&purge::Options {
        root: &root,
        trash: &trash,
        state: &state,
        keep_days: args.trash_days,
        limit: (!args.no_limit).then_some(args.limit),
        today: Date::today(),
    })?;
    tracing::info!(?report, "purged");
    println!(
        "restored {}, moved {}, emptied {} days from the trash",
        report.restored, report.moved, report.expired
    );
    if report.restore_blocked + report.stale + report.failed > 0 {
        println!(
            "left alone: {} restores blocked, {} marks stale, {} moves failed",
            report.restore_blocked, report.stale, report.failed
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn bullets(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("  - {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn check(args: &LibraryArgs) -> anyhow::Result<ExitCode> {
    let loaded = load(args)?;

    println!(
        "{} rooms, {} images, entrance {:?}, probed in {} ms",
        loaded.museum.rooms().len(),
        loaded.library.image_count(),
        loaded.museum.entrance(),
        loaded.scan_duration.as_millis()
    );
    if loaded.unreadable > 0 {
        println!("{} files skipped as unreadable", loaded.unreadable);
    }
    for room in loaded.museum.rooms() {
        println!(
            "  {:<32} {:>4} images   {} <- doors -> {}",
            room.id,
            loaded.images.get(&room.id).map_or(0, Vec::len),
            room.doors.left.len(),
            room.doors.right.len()
        );
    }

    let mut warnings = loaded.warnings.clone();
    warnings.extend(note_link_warnings(&loaded));
    if warnings.is_empty() {
        println!("\nno warnings");
    } else {
        println!("\n{} warnings:", warnings.len());
        println!("{}", bullets(&warnings));
    }
    Ok(ExitCode::SUCCESS)
}

async fn serve(args: ServeArgs) -> anyhow::Result<ExitCode> {
    let loaded = load(&args.library)?;

    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    if loaded.unreadable > 0 {
        tracing::warn!(count = loaded.unreadable, "images skipped as unreadable");
    }
    tracing::info!(
        root = %loaded.library.root().display(),
        rooms = loaded.museum.rooms().len(),
        images = loaded.library.image_count(),
        entrance = loaded.museum.entrance(),
        scan_ms = loaded.scan_duration.as_millis(),
        "museum ready"
    );

    let cache_dir = args.cache_dir.unwrap_or_else(thumbs::default_cache_dir);
    let marking = match &args.state_dir {
        Some(state_dir) => {
            let state = StateDir::open(state_dir)
                .with_context(|| format!("cannot open state directory {}", state_dir.display()))?;
            Some(marks::Marking::open(state, args.trash_days)?)
        }
        None => None,
    };
    let trash_dir = args.trash_dir.clone();
    let library_args = args.library.clone();
    let loader: api::Loader = Box::new(move || {
        let loaded = load(&library_args)?;
        for warning in &loaded.warnings {
            tracing::warn!("{warning}");
        }
        let trash = trash_dir.as_deref().and_then(scan_trash);
        Ok(Snapshot::new(
            loaded.library,
            loaded.museum,
            loaded.images,
            trash,
        ))
    });
    let trash = args.trash_dir.as_deref().and_then(scan_trash);
    if marking.is_some() {
        tracing::info!(
            trashed = trash
                .as_ref()
                .map_or(0, |trash| trash.library.image_count()),
            "deletion marks enabled"
        );
    }
    let state = Arc::new(AppState::new(
        Snapshot::new(loaded.library, loaded.museum, loaded.images, trash),
        loader,
        thumbs::Thumbnailer::new(cache_dir),
        marking,
    ));
    spawn_rescan_triggers(&state, args.rescan_interval)?;

    let mut app = api::routes(state).layer(TraceLayer::new_for_http());
    if let Some(web_dir) = &args.web_dir {
        // Unknown paths fall back to index.html so client-side routes survive
        // a reload.
        let index = ServeFile::new(web_dir.join("index.html"));
        app = app.fallback_service(ServeDir::new(web_dir).fallback(index));
    }

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("cannot bind {}", args.bind))?;
    tracing::info!(address = %args.bind, "listening");

    axum::serve(listener, app)
        .await
        .context("server stopped unexpectedly")?;
    Ok(ExitCode::SUCCESS)
}
