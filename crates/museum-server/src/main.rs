mod api;
mod thumbs;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use api::{AppState, Snapshot};
use clap::{Args, Parser, Subcommand};
use museum_core::{ImageEntry, Library, Manifest, Museum, museum};
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
        match library.scan_room(rel_path) {
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

    if loaded.warnings.is_empty() {
        println!("\nno warnings");
    } else {
        println!("\n{} warnings:", loaded.warnings.len());
        println!("{}", bullets(&loaded.warnings));
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
    let library_args = args.library.clone();
    let loader: api::Loader = Box::new(move || {
        let loaded = load(&library_args)?;
        for warning in &loaded.warnings {
            tracing::warn!("{warning}");
        }
        Ok(Snapshot::new(loaded.library, loaded.museum, loaded.images))
    });
    let state = Arc::new(AppState::new(
        Snapshot::new(loaded.library, loaded.museum, loaded.images),
        loader,
        thumbs::Thumbnailer::new(cache_dir),
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
