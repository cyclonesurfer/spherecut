//! Every limit the server holds an agent to, in one place (PLAN A1,
//! a1-plan section 9). `docs/agents.md` lists them, and each has a test.

use std::time::Duration;

/// The longest message the server reads, in bytes, newline excluded. A
/// longer line is read to its end, dropped, and answered with -32600.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// Tool calls that run at once. Later ones wait their turn, in the order
/// they came; `ping` and `tools/list` are answered meanwhile.
pub const CALLS_AT_ONCE: usize = 2;

/// How long a tool call may take, from its request, before it stops with
/// `timed_out`: under Codex's default of 60 s.
pub const CALL_TIME: Duration = Duration::from_secs(50);

/// How long past its deadline a call may go on before the server
/// answers `timed_out` for it and gives up on its worker (amendment 20):
/// the time a call has to notice that it should stop.
pub const STUCK_GRACE: Duration = Duration::from_secs(2);

/// Workers the server gives up on before it exits with status 1, so that
/// the client starts a fresh one: past this, something below Spherecut
/// (a driver, a network file system) is wedged.
pub const MAX_ABANDONED: usize = 4;

/// Tool calls that may wait for a worker. One more gets `busy`.
pub const MAX_QUEUED: usize = 8;

/// How long the server waits for running calls to stop once stdin
/// closes, before it exits anyway (PLAN: within 2 s).
pub const EXIT_GRACE: Duration = Duration::from_millis(1500);

/// The fewest milliseconds between two progress notifications of a call.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Files in one `probe` call.
pub const FILES_PER_CALL: usize = 64;

/// Files, or a folder's clips, that one `probe` gives in full (`detail:
/// "full"`, amendment 19): a report can take several thousand
/// characters.
pub const FULL_PER_CALL: usize = 8;

/// A folder's clips in a page of `probe`, unless `page_size` says.
pub const PAGE_SIZE: usize = 20;

/// The most clips a page of `probe` may hold.
pub const MAX_PAGE_SIZE: usize = 50;

/// The most characters of subfolder names a page of `probe` lists; the
/// rest are counted in `folders_left_out`.
pub const FOLDERS_CHARS: usize = 4_000;

/// The most characters a result's text may take (amendment 19): under
/// the 50,000 past which Claude Code saves a result to a file rather
/// than showing it. Images are counted apart.
pub const RESULT_CHARS: usize = 40_000;

/// The most entries a folder's listing keeps (`roots::Listing`).
pub const FOLDER_ENTRIES: usize = 10_000;

/// The most a string read from a file may take in a result, in bytes
/// (a1-plan section 4.3). Longer ones are cut, and end in `…`.
pub const FIELD_BYTES: usize = 1024;

/// The longest error a tool returns, in characters of its JSON.
pub const ERROR_CHARS: usize = 500;

/// The most bytes of JSON one `check` result takes (a1-plan section
/// 6.4): passed rules' messages, then the advice, go first to fit.
pub const CHECK_BYTES: usize = 2 << 10;

/// How long doctor's `speed` decodes a file for, at most: less when the
/// call has less than twice this left.
pub const SPEED_TIME: Duration = Duration::from_secs(5);

/// The most tiles one `look` draws: times × views.
pub const MAX_TILES: usize = 24;

/// The long edge of `look`'s image, in pixels, unless `size` says.
pub const IMAGE_DEFAULT_EDGE: u32 = 768;

/// The longest edge `look` returns inline. Past it the image must go to
/// a file (`output`).
pub const IMAGE_MAX_EDGE: u32 = 1568;

/// The longest edge `look` draws at all, to a file: an image of
/// 4096 × 4096 is 64 MiB of RGBA (amendment 27).
pub const IMAGE_LIMIT_EDGE: u32 = 4096;

/// The narrowest a tile may be: a label's second line (`y90 p-20 f100`)
/// is 13 glyphs, 104 pixels (amendment 16).
pub const MIN_TILE_WIDTH: u32 = 112;

/// The most base64 an inline image may take, in bytes, unless
/// `--image-budget-kb` says (amendment 19): 15,000 tokens even if a
/// client counts it as text, under Claude Code's 25,000.
pub const INLINE_IMAGE_BYTES: usize = 60_000;

/// The most `--image-budget-kb` may raise [`INLINE_IMAGE_BYTES`] to: 5 MB
/// of base64, Bedrock's limit on an image and half the API's (a1-plan
/// section 7.1).
pub const MAX_IMAGE_BUDGET: usize = 5_000_000;

/// The quantizer scale a JPEG starts at (2 is FFmpeg's best).
pub const JPEG_QSCALE: u8 = 3;

/// The coarsest quantizer scale a JPEG may take to fit the inline
/// budget; past it the image is `image_too_large`.
pub const JPEG_QSCALE_MAX: u8 = 12;

/// The most video streams `look`'s `raw` mode draws side by side.
pub const RAW_STREAMS: usize = 4;

/// `analyze`'s buckets, unless `buckets` says.
pub const BUCKETS: u32 = 50;

/// The most buckets `analyze` gives: each is about 120 bytes of JSON.
pub const MAX_BUCKETS: u32 = 200;

/// How long before its deadline `analyze` stops to answer with the
/// buckets it finished, plus twice the longest step it took (a packet, a
/// frame, a chunk of audio).
pub const ANALYZE_RESERVE: Duration = Duration::from_millis(1500);

/// The most packets `timing` holds: 4.8 hours at 60 fps.
pub const TIMING_PACKETS: usize = 1 << 20;

/// The most times a list in `analyze`'s summary gives; its `count` says
/// how many there were.
pub const LISTED: usize = 20;

// `analyze`'s thresholds (a1-plan section 6.3). They aren't limits, but
// one place keeps them findable; docs/agents.md gives each.

/// A frame lasting more than this many nominal frames is a gap, and
/// round(its duration over the nominal) − 1 frames are missing.
pub const GAP_OVER: f64 = 1.5;

/// Durations varying by more than this share of the nominal one, plus a
/// tick of the time base, are a variable frame rate (amendment 25).
pub const VFR_SPREAD: f64 = 0.01;

/// A non-key packet under this share of the median non-key packet is a
/// picture coded as a repeat of the one before. An encoder refines a
/// lossy picture when it repeats, so the first of a run costs a few
/// percent of a P-frame (`jitter-dups`' cost 4 %); the rest are a few
/// bytes. A scene that doesn't move codes the same way.
pub const REPEAT_UNDER: f64 = 0.05;

/// The grid `motion` area-averages each frame's luma to, columns × rows.
pub const MOTION_GRID: (usize, usize) = (64, 32);

/// A frame whose change from the one before is more than this many times
/// the median change of the frames around it, and more than
/// [`CUT_FLOOR`], is a cut.
pub const CUT_OVER_MEDIAN: f64 = 6.0;

/// The least change (0 to 255) that is a cut.
pub const CUT_FLOOR: f64 = 12.0;

/// Frames in the window [`CUT_OVER_MEDIAN`]'s median is taken over,
/// centred on the frame.
pub const CUT_WINDOW: usize = 15;

/// A sample at or past this magnitude, in a run of [`CLIP_RUN`] or more,
/// is clipped.
pub const CLIP_LEVEL: f32 = 0.999;

/// Samples in a row at [`CLIP_LEVEL`] that are clipping.
pub const CLIP_RUN: usize = 3;

/// Audio quieter than this on every channel, as RMS over 10 ms windows,
/// is silent.
pub const SILENCE_DBFS: f64 = -60.0;

/// The shortest run of silent windows that is a silence.
pub const SILENCE_MIN: Duration = Duration::from_millis(500);

/// How loud nothing is: the floor of every dBFS `analyze` gives.
pub const DBFS_FLOOR: f64 = -120.0;

/// The most edits one `edit` call may make, as one undo step (a2-plan
/// 4.3).
pub const EDITS_PER_CALL: usize = 1000;

/// The most characters of an `edit` call's label.
pub const LABEL_CHARS: usize = 200;

/// The most keys a track `project`'s view part gives at once, from
/// `from`; `more` says where to go on from.
pub const VIEW_KEYS: usize = 200;

/// The most characters of keys a `view` part lists, so that it fits in
/// [`RESULT_CHARS`] with its envelope: past it, `next_from` says where
/// to go on.
pub const VIEW_CHARS: usize = 30_000;

/// The newest steps `project`'s history part gives.
pub const HISTORY_STEPS: usize = 50;

/// The first markers `project`'s summary lists; the markers part has all.
pub const SUMMARY_MARKERS: usize = 20;

/// How long a call waits for another call's hold of a project's lock
/// (a2-plan 3.2). The GUI's is never waited for.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);
