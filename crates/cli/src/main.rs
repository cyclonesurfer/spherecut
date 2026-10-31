//! The `spherecut` command.
//!
//! With `--json`, stdout gets exactly one JSON document per run, on
//! success and on failure, a panic included, and the exit status says how
//! the run ended (`contract`, D27); `render` writes its start and progress
//! before it, one envelope a line, and the document is the last (D51).
//! stderr is for people.

// Only `interrupt`, frame's and render's SIGINT handler, and `exit`, mcp's way out
// past a stuck call, are allowed unsafe code (D28).
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

mod call;
#[allow(unsafe_code)]
mod exit;
mod frame;
#[allow(unsafe_code)]
mod interrupt;
mod render;
mod skill;

#[derive(Parser)]
#[command(
    name = brand::APP_SLUG,
    version = brand::VERSION,
    about = "Native Linux 360° video editor",
    long_about = "Native Linux 360° video editor. With no command, opens the viewer \
                  (the files given, the first of them shown); the commands below \
                  work without a display.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    gui: GuiCli,
}

/// The viewer's options, with no command.
#[derive(clap::Args)]
struct GuiCli {
    /// Videos to open; the first is shown.
    files: Vec<PathBuf>,
    /// The frame cache's size in MiB, or `auto` (a quarter of the memory
    /// available, 512 MiB to 8 GiB). Over SPHERECUT_FRAME_CACHE_MIB.
    #[arg(long, value_name = "MIB", value_parser = cache_mib)]
    cache_mb: Option<spherecut_playback::CacheBudget>,
    /// The Vulkan adapter to render on: auto (a GPU, else lavapipe), cpu
    /// (lavapipe), or part of an adapter's name or its PCI bus id. Over
    /// SPHERECUT_ADAPTER.
    #[arg(long)]
    adapter: Option<String>,
    /// For hw-test: play this clip, write what was measured to
    /// --smoke-out as JSON (spherecut.smoke/1), and close.
    #[arg(
        long,
        hide = true,
        value_name = "CLIP",
        requires = "smoke_out",
        conflicts_with = "files"
    )]
    smoke: Option<PathBuf>,
    /// How long the smoke run plays, from the clip's first frame.
    #[arg(long, hide = true, value_name = "SECONDS", requires = "smoke", default_value_t = 20.0, value_parser = smoke_seconds)]
    smoke_seconds: f64,
    #[arg(long, hide = true, value_name = "JSON", requires = "smoke")]
    smoke_out: Option<PathBuf>,
    /// A screenshot at the run's end, at most 960 pixels wide.
    #[arg(long, hide = true, value_name = "PNG", requires = "smoke")]
    smoke_screenshot: Option<PathBuf>,
    /// An export of the clip's first 2 s, made while it plays (M2 plan
    /// section 11), to this MP4.
    #[arg(long, hide = true, value_name = "MP4", requires = "smoke")]
    smoke_export: Option<PathBuf>,
    /// The toolkit check's timeline prototype (D26) in place of the
    /// viewer; with --proto-out, a script drives it, what each UI pass
    /// took goes there as JSON (spherecut.timeline-proto/1), and it
    /// closes.
    #[arg(long, hide = true, conflicts_with_all = ["files", "smoke"])]
    timeline_proto: bool,
    /// How long the script runs.
    #[arg(long, hide = true, value_name = "SECONDS", requires = "timeline_proto", default_value_t = 20.0, value_parser = smoke_seconds)]
    proto_seconds: f64,
    #[arg(long, hide = true, value_name = "JSON", requires = "timeline_proto")]
    proto_out: Option<PathBuf>,
    /// A screenshot at the run's end, at most 960 pixels wide.
    #[arg(long, hide = true, value_name = "PNG", requires = "proto_out")]
    proto_screenshot: Option<PathBuf>,
    /// egui's zoom: points this many times the desktop's, 0.5 to 4.
    #[arg(long, hide = true, value_name = "FACTOR", requires = "timeline_proto", default_value_t = 1.0, value_parser = proto_zoom)]
    proto_zoom: f32,
}

fn proto_zoom(s: &str) -> Result<f32, String> {
    match s.parse::<f32>() {
        Ok(v) if (0.5..=4.0).contains(&v) => Ok(v),
        _ => Err(format!("{s:?} isn't a zoom from 0.5 to 4")),
    }
}

fn smoke_seconds(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(format!("{s:?} isn't a number of seconds above 0")),
    }
}

fn cache_mib(s: &str) -> Result<spherecut_playback::CacheBudget, String> {
    spherecut_playback::CacheBudget::parse_mib(s).map_err(|e| e.to_string())
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
    /// Export a project's sequence to an MP4, with its stored export
    /// settings; each flag overrides one for this run only. The file
    /// appears only once it is whole and has passed its checks.
    Render(render::RenderArgs),
    /// Serve Spherecut's tools to an AI agent over MCP, on stdin and
    /// stdout (docs/agents.md); they only read unless --allow-write. Logs
    /// go to stderr.
    Mcp {
        #[command(flatten)]
        agent: AgentArgs,
    },
    /// Run one of the agent tools that `spherecut mcp` serves, with the
    /// same roots and limits, and print its result as one JSON document
    /// (docs/agents.md).
    Call {
        /// The tool: probe, look, analyze, check or doctor.
        tool: String,
        /// Its arguments, a JSON object, as an agent sends them, such as
        /// '{"files": ["a.mp4"]}'.
        #[arg(default_value = "{}")]
        args: String,
        #[command(flatten)]
        agent: AgentArgs,
        /// Where look's images go, as spherecut-look-<ms>-<n>.jpg (or
        /// .png): the temporary folder by default. The document's
        /// `images` lists them.
        #[arg(long, value_name = "DIR")]
        image_dir: Option<PathBuf>,
    },
    /// Set up what AI agents read about Spherecut (docs/agents.md).
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
}

#[derive(Subcommand)]
enum AgentsCmd {
    /// Write Spherecut's skill, the recipes agents follow with its tools,
    /// to $XDG_DATA_HOME/spherecut/skills/spherecut/SKILL.md, and link
    /// that folder as ~/.claude/skills/spherecut (Claude Code) and
    /// ~/.agents/skills/spherecut (Codex, Gemini CLI, Copilot CLI).
    /// Anything else already at either name is left alone, and the run
    /// fails.
    InstallSkill {
        /// Say what it would do, and do nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// The largest `--image-budget-kb`: 5 MB of base64, Bedrock's limit on
/// an image and half the API's.
const MAX_IMAGE_BUDGET_KB: i64 = (spherecut_agent::limits::MAX_IMAGE_BUDGET / 1000) as i64;

/// What `spherecut mcp` and `spherecut call` share: the roots, and how
/// large an image inline may be.
#[derive(clap::Args)]
struct AgentArgs {
    #[command(flatten)]
    roots: RootArgs,
    /// The most base64 an image in a result may take, in kilobytes of
    /// 1000 bytes: look lowers its JPEG quality to fit, then refuses.
    /// Claude Code counts images against MAX_MCP_OUTPUT_TOKENS
    /// (docs/agents.md).
    #[arg(
        long,
        value_name = "KB",
        default_value_t = (spherecut_agent::limits::INLINE_IMAGE_BYTES / 1000) as u32,
        value_parser = clap::value_parser!(u32).range(1..=MAX_IMAGE_BUDGET_KB)
    )]
    image_budget_kb: u32,
}

impl AgentArgs {
    fn options(&self, roots: spherecut_agent::Roots) -> spherecut_agent::Options {
        spherecut_agent::Options {
            roots,
            image_budget: self.image_budget_kb as usize * 1000,
        }
    }
}

/// Where an agent's files may be read from and written to (D45).
#[derive(clap::Args)]
struct RootArgs {
    /// A folder the agent may read files from, and everything under it;
    /// repeat it for more. By default, the working directory. The agent's
    /// relative paths are under the first.
    #[arg(long = "read-root", value_name = "DIR")]
    read_roots: Vec<PathBuf>,
    /// Let tools write the output files an agent asks for, under
    /// --write-root only, and never over anything already there.
    #[arg(long, requires = "write_root")]
    allow_write: bool,
    /// The folder outputs go in, with --allow-write. It is read from too.
    #[arg(long, value_name = "DIR", requires = "allow_write")]
    write_root: Option<PathBuf>,
}

impl RootArgs {
    /// The roots, opened. A root that can't be used is the command line's
    /// fault, and stops the command before it starts.
    fn roots(&self) -> std::result::Result<spherecut_agent::Roots, spherecut_agent::RootsError> {
        spherecut_agent::Roots::new(&self.read_roots, self.write_root.as_deref())
    }

    /// [`roots`](Self::roots), or the usage error on stderr.
    fn open(&self, command: &str) -> std::result::Result<spherecut_agent::Roots, Exit> {
        self.roots().map_err(|e| {
            eprintln!("spherecut {command}: {e}");
            Exit::Usage
        })
    }
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
    let Some(cmd) = cli.cmd else {
        return Ok(gui(cli.gui));
    };
    match cmd {
        Cmd::Frame(frame) => match frame.check() {
            Ok(()) => frame::run(frame),
            Err(message) => Ok(usage(&subcommand_error("frame", message), args, json)),
        },
        Cmd::Render(render) => render::run(render),
        Cmd::Probe { files, json } => probe(&files, json),
        Cmd::Doctor { render_node, json } => {
            // The lookup hardware decoding uses (D30); no adapter is known
            // before the GUI's (M1 slice 3).
            let node = spherecut_media::hw::render_node(None, render_node.as_deref());
            doctor(&node.path, json)
        }
        Cmd::Mcp { agent } => mcp(&agent),
        Cmd::Call {
            tool,
            args,
            agent,
            image_dir,
        } => call::run(&tool, &args, &agent, image_dir.as_deref()),
        Cmd::Agents {
            cmd: AgentsCmd::InstallSkill { dry_run },
        } => skill::install(
            &skill::Places::from_env()?,
            dry_run,
            &mut std::io::stdout().lock(),
        ),
    }
}

/// An MCP session on stdin and stdout, until stdin closes (A1, D43).
fn mcp(agent: &AgentArgs) -> Result<Exit> {
    let roots = match agent.roots.open("mcp") {
        Ok(roots) => roots,
        Err(exit) => return Ok(exit),
    };
    // stdout is the protocol's alone (a1-plan section 3.1): the server
    // writes to a copy of it, and fd 1 itself goes to stderr from here
    // on, so that a stray print from FFmpeg, a driver or a dependency
    // lands among the logs rather than in the middle of a message.
    // Close-on-exec, so that no child of a library inherits it.
    let protocol = rustix::io::fcntl_dupfd_cloexec(std::io::stdout(), 3)?;
    rustix::stdio::dup2_stdout(std::io::stderr())?;
    #[cfg(debug_assertions)]
    if std::env::var("SPHERECUT_DEBUG_PANIC").as_deref() == Ok("mcp_print") {
        // The guard's test: this must reach stderr.
        println!("SPHERECUT_DEBUG_PANIC=mcp_print: a stray print");
    }
    let ended = spherecut_agent::serve(
        std::io::BufReader::new(std::io::stdin()),
        std::fs::File::from(protocol),
        agent.options(roots),
    );
    if ended.gave_up {
        exit::now(Exit::Failed as u8);
    }
    if ended.stuck > 0 {
        exit::now(Exit::Ok as u8);
    }
    Ok(Exit::Ok)
}

/// The viewer, until its window closes. Without a display it exits 3
/// (`no_display`), as it does without a GPU (D27).
fn gui(args: GuiCli) -> Exit {
    let gui = spherecut_app::GuiArgs {
        files: args.files,
        cache: args.cache_mb,
        adapter: args
            .adapter
            .as_deref()
            .map(spherecut_render::AdapterChoice::parse),
        smoke: args.smoke.map(|clip| spherecut_app::smoke::SmokeArgs {
            clip,
            seconds: args.smoke_seconds,
            out: args.smoke_out.unwrap_or_default(),
            screenshot: args.smoke_screenshot,
            export: args.smoke_export,
        }),
        proto: args
            .timeline_proto
            .then_some(spherecut_app::proto::ProtoArgs {
                seconds: args.proto_seconds,
                out: args.proto_out,
                screenshot: args.proto_screenshot,
                zoom: args.proto_zoom,
            }),
    };
    match spherecut_app::run(gui) {
        Ok(()) => Exit::Ok,
        Err(e) => {
            to_stderr(&format!("error: {e}\n"));
            contract::frame_exit(e.code())
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
            Err(e) if !json => spherecut_media::probe::ffmpeg_lines(&e.ffmpeg_log),
            _ => String::new(),
        };
        let file = ProbeFile::new(path, probed);
        if !json
            && reading
            && let Some(r) = &file.report
        {
            reading = emit(&spherecut_media::probe::render_text(r))?;
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
        emit(&spherecut_media::doctor::render_text(&report))?;
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
