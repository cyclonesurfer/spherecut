//! The `spherecut` command.
//!
//! With `--json`, stdout gets exactly one JSON document per run, on
//! success and on failure, a panic included, and the exit status says how
//! the run ended (`contract`, D27). stderr is for people.

// Only `interrupt`, frame's SIGINT handler, is allowed unsafe code (D28).
#![deny(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use spherecut_cli::contract::{self, Envelope, Exit, ProbeFile};
use spherecut_core::brand;

mod frame;
#[allow(unsafe_code)]
mod interrupt;
mod show;

#[derive(Parser)]
#[command(name = brand::APP_SLUG, version = brand::VERSION, about = "Native Linux 360° video editor")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show container, streams, codecs, color, timing, spherical metadata
    /// and the detected camera for one or more files. Fails if any file
    /// cannot be read, is not MP4/MOV or Matroska/WebM, is truncated or
    /// corrupt, or has no video or audio.
    Probe {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Print one JSON document instead of text (docs/cli-json.md).
        #[arg(long)]
        json: bool,
    },
    /// Check codecs and hardware decode/encode support, and print the
    /// command that fixes anything missing.
    Doctor {
        /// DRM render node to test for VA-API: by default the one hardware
        /// decoding uses (`/dev/dri/renderD128` until the GUI's adapter
        /// names its GPU).
        #[arg(long)]
        render_node: Option<PathBuf>,
        /// Print one JSON document instead of text (docs/cli-json.md).
        #[arg(long)]
        json: bool,
    },
    /// Render one frame of a stitched equirect through a virtual camera
    /// into a PNG.
    Frame(frame::FrameArgs),
}

fn main() -> ExitCode {
    init_logging();
    let args: Vec<OsString> = std::env::args_os().collect();
    let json = contract::wants_json(&args);
    if json {
        report_panics(contract::schema_for(&args));
    }
    // Parsing too, and reporting a command line that doesn't parse, so
    // that under --json a panic anywhere on this thread ends in exit 1.
    let parse_and_run = || match Cli::try_parse_from(&args) {
        Ok(cli) => run(cli, &args, json),
        Err(e) => Ok(usage(&e, &args, json)),
    };
    let exit = match std::panic::catch_unwind(parse_and_run) {
        Ok(Ok(exit)) => exit,
        Ok(Err(e)) => {
            to_stderr(&format!("error: {e:#}\n"));
            Exit::Failed
        }
        // Without --json, a panic ends the run as Rust's do.
        Err(panic) if !json => std::panic::resume_unwind(panic),
        Err(_) => Exit::Failed,
    };
    // A panic on another thread fails the run too.
    if PANICKED.load(Ordering::SeqCst) {
        Exit::Failed.into()
    } else {
        exit.into()
    }
}

/// Set when this run's one document starts to go out, so that no second
/// one follows it.
static DOCUMENT_STARTED: AtomicBool = AtomicBool::new(false);

/// Set by [`report_panics`]' hook.
static PANICKED: AtomicBool = AtomicBool::new(false);

/// Make a panic, on any thread, a failure of the run under `--json`: the
/// run's one document is an `internal` error of `schema` that says where
/// it panicked, unless the document has already started, and the exit
/// status is 1. Rust's own report of the panic still goes to stderr.
fn report_panics(schema: &'static str) {
    let rust = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        rust(info);
        PANICKED.store(true, Ordering::SeqCst);
        if DOCUMENT_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let at = info
            .location()
            .map(|l| format!(" at {l}"))
            .unwrap_or_default();
        // `PanicHookInfo::payload_as_str` is newer than our rust-version.
        let payload = info.payload();
        let what = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        let (envelope, _) = contract::internal(
            schema,
            format!("spherecut panicked{at}: {what}. This is a bug; please report it"),
        );
        if let Ok(line) = envelope.to_line() {
            let _ = emit(&line);
        }
    }));
}

/// Report a command line that doesn't parse, or the help or version it
/// asked for. Help and version are text, with `--json` too.
fn usage(e: &clap::Error, args: &[OsString], json: bool) -> Exit {
    // clap's own wording, for people, on stderr (help and version go to
    // stdout); a closed stream is no reason to fail differently.
    let _ = e.print();
    if !e.use_stderr() {
        return Exit::Ok;
    }
    #[cfg(debug_assertions)]
    debug_panic("usage");
    if json {
        let (envelope, exit) = contract::usage(args, &e.render().to_string());
        if let Ok(line) = envelope.to_line() {
            let _ = emit_document(&line);
        }
        return exit;
    }
    Exit::Usage
}

/// A usage error of `subcommand` that clap's parsing doesn't catch, worded
/// as clap's own are, with the subcommand's usage.
fn subcommand_error(subcommand: &str, message: String) -> clap::Error {
    let mut cli = Cli::command();
    cli.build();
    let kind = clap::error::ErrorKind::ValueValidation;
    match cli.find_subcommand_mut(subcommand) {
        Some(sub) => sub.error(kind, message),
        None => cli.error(kind, message),
    }
}

/// What the CLI logs by default: warnings, but not FFmpeg's own messages.
/// FFmpeg logs an error for every device doctor tries that isn't there,
/// and our errors already say what failed.
const DEFAULT_LOG: &str = "warn,ffmpeg=off";

/// Log to stderr as [`log_directives`] says for `SPHERECUT_LOG`.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter = match std::env::var("SPHERECUT_LOG") {
        Ok(v) if !v.trim().is_empty() => {
            EnvFilter::try_new(log_directives(&v)).unwrap_or_else(|e| {
                to_stderr(&format!("warning: ignoring SPHERECUT_LOG={v}: {e}\n"));
                EnvFilter::new(DEFAULT_LOG)
            })
        }
        _ => EnvFilter::new(DEFAULT_LOG),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        // It would report a failed write with eprintln!, which panics on
        // a closed stderr; see to_stderr.
        .log_internal_errors(false)
        .init();
}

/// The `EnvFilter` directives for a `SPHERECUT_LOG` value, e.g. `debug`,
/// `ffmpeg=debug` or `spherecut_media=trace`, which add to the default.
/// FFmpeg's messages show when the value names the `ffmpeg` target, or
/// sets a level for everything that is more verbose than the default's
/// (`info`, `debug`, `trace`); `warn` and `error` leave them off.
fn log_directives(v: &str) -> String {
    use tracing_subscriber::filter::LevelFilter;
    // EnvFilter splits on commas too, but takes spaces as part of a
    // directive. An empty directive would parse as a level (ERROR).
    let directives: Vec<&str> = v
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .collect();
    // The last global level wins in EnvFilter. `ffmpeg=off` would outrank
    // it (a directive naming the target wins), so a more verbose one
    // replaces the default. A directive naming `ffmpeg` comes later and
    // simply overrides it.
    let global = directives
        .iter()
        .rev()
        .find_map(|d| d.parse::<LevelFilter>().ok());
    let base = match global {
        Some(level) if level > LevelFilter::WARN => "warn",
        _ => DEFAULT_LOG,
    };
    std::iter::once(base)
        .chain(directives)
        .collect::<Vec<_>>()
        .join(",")
}

fn run(cli: Cli, args: &[OsString], json: bool) -> Result<Exit> {
    #[cfg(debug_assertions)]
    debug_panic("run");
    match cli.cmd {
        Cmd::Frame(frame) => match frame.check() {
            Ok(()) => frame::run(frame),
            Err(message) => Ok(usage(&subcommand_error("frame", message), args, json)),
        },
        Cmd::Probe { files, json } => probe(&files, json),
        Cmd::Doctor { render_node, json } => {
            // The lookup hardware decoding uses (D30); no adapter is known
            // before the GUI's (M1 slice 3).
            let node = spherecut_media::hw::render_node(None, render_node.as_deref());
            doctor(&node.path, json)
        }
    }
}

fn probe(paths: &[PathBuf], json: bool) -> Result<Exit> {
    let mut files = Vec::with_capacity(paths.len());
    // Cleared when the reader goes away; the rest are still probed for the
    // exit code and the errors on stderr.
    let mut reading = true;
    for path in paths {
        let probed = spherecut_media::probe::probe(path);
        // FFmpeg's own word on why it could not open the file, under ours.
        // In JSON it is the error's `ffmpeg_log`.
        let ffmpeg = match &probed {
            Err(e) if !json => show::ffmpeg_lines(&e.ffmpeg_log),
            _ => String::new(),
        };
        let file = ProbeFile::new(path, probed);
        if !json
            && reading
            && let Some(r) = &file.report
        {
            reading = emit(&show::probe(r))?;
        }
        if let Some(e) = &file.error {
            to_stderr(&format!("error: {}\n{ffmpeg}", e.message));
        }
        files.push(file);
    }
    let (envelope, exit) = contract::probe(files);
    if json {
        print_json(&envelope, exit)
    } else {
        Ok(exit)
    }
}

fn doctor(render_node: &Path, json: bool) -> Result<Exit> {
    let report = spherecut_media::doctor::run(render_node);
    let (envelope, exit) = contract::doctor(&report);
    if json {
        print_json(&envelope, exit)
    } else {
        emit(&show::doctor(&report))?;
        Ok(exit)
    }
}

/// In debug builds, panic when `SPHERECUT_DEBUG_PANIC` asks, so that the
/// contract test can show what a panic prints: as a command starts to
/// `run`, on this thread (`main`) or another (`thread`), and as a
/// command line that doesn't parse is reported (`usage`). `stage` is
/// where this call is. Release builds have no such switch.
#[cfg(debug_assertions)]
fn debug_panic(stage: &str) {
    match (stage, std::env::var("SPHERECUT_DEBUG_PANIC").as_deref()) {
        ("run", Ok("main")) => panic!("SPHERECUT_DEBUG_PANIC=main"),
        ("run", Ok("thread")) => {
            let _ = std::thread::spawn(|| panic!("SPHERECUT_DEBUG_PANIC=thread")).join();
        }
        ("usage", Ok("usage")) => panic!("SPHERECUT_DEBUG_PANIC=usage"),
        _ => {}
    }
}

/// Print `envelope` as the run's one document, and end the run with
/// `exit`. A reader that went away gets nothing, and the exit stays.
fn print_json<T: Serialize>(envelope: &Envelope<T>, exit: Exit) -> Result<Exit> {
    let (line, exit) = match envelope.to_line() {
        Ok(line) => (line, exit),
        Err(e) => {
            let (envelope, exit) = contract::internal(
                envelope.schema(),
                format!("cannot write the JSON document: {e}"),
            );
            (envelope.to_line()?, exit)
        }
    };
    emit_document(&line)?;
    Ok(exit)
}

/// Write `line` as the run's one document, unless a panic has already
/// written one (see [`report_panics`]).
fn emit_document(line: &str) -> Result<bool> {
    if DOCUMENT_STARTED.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    emit(line)
}

/// Write `s` to stderr. Errors are ignored: with nowhere to report them,
/// a closed stderr (`spherecut probe *.mp4 2>&1 | head`) must not turn the
/// exit code into a panic's.
fn to_stderr(s: &str) {
    use std::io::Write;
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

/// Write `s` to stdout. Returns false when the reader has gone away, as in
/// `spherecut probe *.mp4 | head`, which is not an error.
fn emit(s: &str) -> Result<bool> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(anyhow::Error::new(e).context("cannot write to stdout")),
    }
}
