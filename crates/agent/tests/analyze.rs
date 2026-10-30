//! `analyze` (a1-plan section 6.3 and 11.2) on clips synth writes:
//! timing finds what `jitter-dups` plants, in MP4 and in a Matroska
//! remux, and calls a constant rate constant; motion finds a cut; audio
//! measures `levels`' sections; a call short of time returns the buckets
//! it finished and where to go on from; errors name their argument.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::limits::{ANALYZE_RESERVE, RESULT_CHARS};
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_core::{FrameRate, Rational};
use spherecut_synth::audio::{LEVELS_CLIPPED, LEVELS_LOUD, LEVELS_QUIET, LEVELS_SILENT};
use spherecut_synth::media::{self, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("analyze-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), None).unwrap())
    }
}

fn call(tools: &Spherecut, args: Value, time: Duration) -> Result<Value, ToolError> {
    let cx = CallCx::standalone(Instant::now() + time);
    tools.call("analyze", args, &cx).map(|o| o.result)
}

fn analyze(tools: &Spherecut, args: Value) -> Value {
    let r = call(tools, args, Duration::from_secs(50)).unwrap();
    assert_eq!(r["complete"], true, "{r}");
    assert!(r.get("next_from").is_none(), "{r}");
    r
}

/// Slots as seconds, to the millisecond, as analyze gives times.
fn at(slots: &[u32]) -> Value {
    json!(
        slots
            .iter()
            .map(|&s| (f64::from(s) / 30.0 * 1000.0).round() / 1000.0)
            .collect::<Vec<_>>()
    )
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

/// A time's seconds, to the millisecond, after checking that its
/// `exact` reads back to them.
fn ms(t: &Value) -> f64 {
    let exact: Rational = t["exact"].as_str().unwrap().parse().unwrap();
    let s = f(&t["seconds"]);
    assert!((exact.to_f64() - s).abs() < 1e-9, "{t}");
    (s * 1000.0).round() / 1000.0
}

/// A list of times, as [`at`] gives them.
fn when(list: &Value) -> Value {
    json!(list.as_array().unwrap().iter().map(ms).collect::<Vec<_>>())
}

/// What `jitter-dups` plants, as timing should find it in `file`, whose
/// durations are within `ms` of the planted ones.
fn finds_what_jitter_dups_plants(tools: &Spherecut, file: &Path, ms: f64) {
    let r = analyze(tools, json!({"file": file, "kind": "timing"}));
    let s = &r["summary"];
    assert_eq!(r["kind"], "timing");
    assert_eq!(r["stream"], 0);
    assert_eq!(r["from"]["seconds"], 0.0);
    assert!((f(&r["to"]["seconds"]) - 10.0).abs() < 0.05, "{r}");
    assert_eq!(r["buckets"].as_array().unwrap().len(), 50);
    assert_eq!(s["frames"], media::jitter_dups_frames().len());
    assert_eq!(s["nominal_fps"], 30.0, "{s}");
    assert_eq!(s["vfr"], true);
    // Each gap is the frame before it lasting four slots.
    let gaps: Vec<u32> = media::JITTER_GAPS.iter().map(|g| g - 1).collect();
    assert_eq!(s["gaps"]["count"], 2, "{s}");
    assert_eq!(s["gaps"]["missing"], 2 * media::JITTER_GAP_SLOTS);
    assert_eq!(when(&s["gaps"]["at"]), at(&gaps));
    assert_eq!(s["dups"]["count"], 2, "{s}");
    assert_eq!(when(&s["dups"]["at"]), at(&media::JITTER_DUPS_AFTER));
    assert_eq!(s["repeated_pictures"]["count"], 10, "{s}");
    assert_eq!(
        when(&s["repeated_pictures"]["at"]),
        at(&media::JITTER_REPEATS)
    );
    assert_eq!(s["keyframes"], 10);
    // The jitter: 0.8 and 1.2 slots by turns, in the buckets from 2 s.
    let jitter = &r["buckets"][11];
    assert_eq!(jitter["from"], 2.2, "{jitter}");
    assert!((f(&jitter["min_ms"]) - 26.667).abs() < ms, "{jitter}");
    assert!((f(&jitter["max_ms"]) - 40.0).abs() < ms, "{jitter}");
    let steady = &r["buckets"][0];
    assert!((f(&steady["min_ms"]) - 33.333).abs() < ms, "{steady}");
    assert!((f(&steady["max_ms"]) - 33.333).abs() < ms, "{steady}");
    // The bucket holding the first gap: 4.8 s to 5 s.
    assert_eq!(r["buckets"][24]["missing"], 3, "{}", r["buckets"][24]);
    assert!(f(&s["mbps"]) > 0.0);
    assert_eq!(r["warnings"], json!([]));
}

#[test]
fn timing_finds_jitter_gaps_dups_and_repeats() {
    let dir = TempDir::new("jitter");
    let file = media::write_jitter_dups(&dir.0).unwrap();
    finds_what_jitter_dups_plants(&dir.tools(), &file, 0.02);
}

#[test]
fn timing_finds_them_in_matroska_too() {
    // Milliseconds: the duplicates tie exactly, and each duration
    // rounds by up to a millisecond.
    let dir = TempDir::new("jitter-mkv");
    let mp4 = media::write_jitter_dups(&dir.0).unwrap();
    let mkv = media::remux_to_mkv(&mp4, &dir.0).unwrap();
    finds_what_jitter_dups_plants(&dir.tools(), &mkv, 1.0);
}

#[test]
fn a_constant_rate_is_constant_in_mp4_and_matroska() {
    let dir = TempDir::new("cfr");
    let mp4 = media::write(
        &Spec {
            name: "ntsc".into(),
            rate: FrameRate::FPS_29_97,
            frames: 90,
            ..Spec::default()
        },
        &dir.0,
    )
    .unwrap();
    let mkv = media::remux_to_mkv(&mp4, &dir.0).unwrap();
    for file in [mp4, mkv] {
        let r = analyze(
            &dir.tools(),
            json!({"file": file, "kind": "timing", "buckets": 3}),
        );
        let s = &r["summary"];
        assert_eq!(s["vfr"], false, "{file:?}: {s}");
        assert_eq!(s["nominal_fps"], 29.97, "{file:?}: {s}");
        assert_eq!(s["frames"], 90);
        assert_eq!(s["gaps"]["count"], 0);
        assert_eq!(s["dups"]["count"], 0);
        // Frame numbers count at the clip's rate.
        let r = analyze(
            &dir.tools(),
            json!({"file": file, "kind": "timing", "from": "30f"}),
        );
        assert_eq!(r["from"]["exact"], "1001/1000", "{file:?}");
        assert_eq!(r["summary"]["frames"], 60);
    }
}

#[test]
fn from_and_to_pick_the_stretch() {
    let dir = TempDir::new("stretch");
    let file = media::write_jitter_dups(&dir.0).unwrap();
    let tools = dir.tools();
    let r = analyze(
        &tools,
        json!({"file": file, "kind": "timing", "from": "2", "to": [4, 1], "buckets": 4}),
    );
    assert_eq!(r["from"]["seconds"], 2.0);
    assert_eq!(r["to"]["seconds"], 4.0);
    assert_eq!(r["bucket"], 0.5);
    let b = r["buckets"].as_array().unwrap();
    assert_eq!(b.len(), 4);
    assert_eq!(b[0]["from"], 2.0);
    assert_eq!(b[3]["from"], 3.5);
    // Slots 61 to 119, with slot 103 a repeat: slot 60 is shown early,
    // before 2 s.
    assert_eq!(r["summary"]["frames"], 59);
    assert_eq!(when(&r["summary"]["repeated_pictures"]["at"]), at(&[103]));
    // To past the end is the end.
    let r = analyze(
        &tools,
        json!({"file": file, "kind": "timing", "from": "2", "to": "99"}),
    );
    assert!((f(&r["to"]["seconds"]) - 10.0).abs() < 0.05, "{r}");
    // A frame number needs a constant rate, which this clip hasn't.
    let e = call(
        &tools,
        json!({"file": file, "kind": "timing", "from": "60f"}),
        Duration::from_secs(50),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::BAD_ARGUMENT, Some("from"))
    );
}

#[test]
fn the_largest_result_fits() {
    let dir = TempDir::new("size");
    let file = media::write_jitter_dups(&dir.0).unwrap();
    let r = analyze(
        &dir.tools(),
        json!({"file": file, "kind": "timing", "buckets": 200}),
    );
    let n = r.to_string().chars().count();
    assert!(n < RESULT_CHARS, "{n}");
    // About 120 bytes a bucket (a1-plan section 6.3).
    assert!(n < 200 * 130 + 2_000, "{n}");
}

#[test]
fn motion_finds_a_cut_and_nothing_else() {
    let dir = TempDir::new("motion");
    let file = media::write(
        &Spec {
            name: "cut".into(),
            frames: 60,
            cut_away: vec![30],
            ..Spec::default()
        },
        &dir.0,
    )
    .unwrap();
    let r = analyze(
        &dir.tools(),
        json!({"file": file, "kind": "motion", "buckets": 4}),
    );
    let s = &r["summary"];
    assert_eq!(s["frames"], 60, "{s}");
    // Into the other scene at frame 30, and out of it at 31.
    assert_eq!(s["cuts"]["count"], 2, "{s}");
    assert_eq!(when(&s["cuts"]["at"]), at(&[30, 31]));
    assert_eq!(s["cuts"]["at"][0]["exact"], "1", "{s}");
    assert!(f(&s["max"]) > 50.0, "{s}");
    assert!([1.0, 1.033].contains(&ms(&s["max_at"])), "{s}");
    let b = r["buckets"].as_array().unwrap();
    assert_eq!(b[2]["cuts"], 2);
    assert!(f(&b[0]["max"]) < 12.0, "{}", b[0]);
    assert_eq!(b[0]["frames"], 15);
    assert!(s["decode"]["path"].is_string(), "{s}");
}

#[test]
fn audio_measures_each_section() {
    let dir = TempDir::new("levels");
    let file = media::write(&media::levels(), &dir.0).unwrap();
    // Buckets of half a second.
    let r = analyze(
        &dir.tools(),
        json!({"file": file, "kind": "audio", "buckets": 6}),
    );
    let (s, b) = (&r["summary"], r["buckets"].as_array().unwrap());
    assert_eq!(s["rate"], 48_000);
    assert_eq!(s["channels"], 2);
    assert_eq!(r["stream"], 1);
    let bucket = |t: f64| &b[(t / 0.5) as usize];
    // The left channel's -6 dBFS, not the right's -12. AAC's coding
    // lifts a tone's peaks by up to a dB (FFmpeg's astats reads the same
    // from these files); its RMS stays within a few tenths.
    for t in [LEVELS_LOUD.0 + 0.25, LEVELS_LOUD.0 + 0.75] {
        let loud = bucket(t);
        assert!((-6.3..-5.0).contains(&f(&loud["peak_db"])), "{loud}");
        assert!((f(&loud["rms_db"]) + 9.03).abs() < 0.3, "{loud}");
        assert_eq!(loud["clipped"], 0);
    }
    // Silence, but for the loud tone's tail and AAC's pre-echo of the
    // clipped one, down at about -36 and -62 dBFS.
    for t in [LEVELS_SILENT.0 + 0.25, LEVELS_SILENT.0 + 0.75] {
        let silent = bucket(t);
        assert!(f(&silent["rms_db"]) < -60.0, "{silent}");
    }
    // Clipped twice over: AAC ripples the flat tops, below and past full
    // scale, so that only some samples still count as clipped, but the
    // peaks stay at full scale.
    let clipped = bucket(LEVELS_CLIPPED.0);
    assert!(f(&clipped["clipped"]) > 0.0, "{clipped}");
    assert!(f(&clipped["peak_db"]) > -0.5, "{clipped}");
    assert!(f(&bucket(LEVELS_CLIPPED.0 + 0.25)["peak_db"]) > -1.0);
    let quiet = bucket(LEVELS_QUIET.0);
    assert!((f(&quiet["peak_db"]) + 20.0).abs() < 1.0, "{quiet}");
    assert_eq!(quiet["clipped"], 0);
    // One silence, the section's, give or take an AAC frame's smear.
    assert_eq!(s["silences"]["count"], 1, "{s}");
    let range = &s["silences"]["at"][0];
    assert!((ms(&range[0]) - LEVELS_SILENT.0).abs() < 0.05, "{range}");
    assert!((ms(&range[1]) - LEVELS_SILENT.1).abs() < 0.05, "{range}");
    assert_eq!(s["clipped"], clipped["clipped"]);
}

#[test]
fn a_call_short_of_time_returns_the_buckets_it_finished() {
    let dir = TempDir::new("short");
    let file = media::write(
        &Spec {
            name: "long".into(),
            width: 1024,
            height: 512,
            frames: 600,
            ..Spec::default()
        },
        &dir.0,
    )
    .unwrap();
    let tools = dir.tools();
    let time = ANALYZE_RESERVE + Duration::from_millis(1200);
    let started = Instant::now();
    let r = call(&tools, json!({"file": file, "kind": "motion"}), time).unwrap();
    assert!(started.elapsed() < time, "{:?}", started.elapsed());
    assert_eq!(r["complete"], false, "{r}");
    let done = r["buckets"].as_array().unwrap().len();
    assert!((1..50).contains(&done), "{done}");
    // next_from is where the next bucket starts, and a call from there
    // goes on.
    let next = &r["next_from"];
    assert!(
        (f(&next["seconds"]) - 0.4 * done as f64).abs() < 1e-6,
        "{next}"
    );
    let rest = analyze(
        &tools,
        json!({"file": file, "kind": "motion", "from": next["exact"], "buckets": 50 - done}),
    );
    assert_eq!(rest["from"], *next);
    let total = f(&r["summary"]["frames"]) + f(&rest["summary"]["frames"]);
    assert_eq!(total, 600.0);
}

#[test]
fn errors_name_their_argument() {
    let dir = TempDir::new("errors");
    let file = media::write(&Spec::default(), &dir.0).unwrap();
    let tools = dir.tools();
    let err = |args: Value| call(&tools, args, Duration::from_secs(50)).unwrap_err();
    let e = err(json!({"file": file, "kind": "audio"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::NO_AUDIO, Some("file"))
    );
    let e = err(json!({"file": file, "kind": "timing", "stream": 3}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::BAD_ARGUMENT, Some("stream"))
    );
    assert_eq!(e.valid.as_deref(), Some("video streams: 0"));
    let e = err(json!({"file": file, "kind": "timing", "from": "5"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::OUT_OF_RANGE, Some("from"))
    );
    let e = err(json!({"file": file, "kind": "timing", "from": "0.5", "to": "0.5"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::BAD_ARGUMENT, Some("to"))
    );
    let e = err(json!({"file": file, "kind": "timing", "to": 0.5}));
    assert_eq!(e.code, codes::BAD_ARGUMENT, "{e:?}");
    let e = err(json!({"file": file, "kind": "speed"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::BAD_ARGUMENT, Some("kind"))
    );
    std::fs::write(dir.0.join("bad.mp4"), b"not a video\n").unwrap();
    let e = err(json!({"file": dir.0.join("bad.mp4"), "kind": "timing"}));
    assert_eq!(e.argument.as_deref(), Some("file"), "{e:?}");
    assert!(!e.message.contains("Invalid data"), "{e:?}");
}
