//! Tier 2c: P-TRK, the timing tables of Pat's X5 files (M3 plan 4.3).
//!
//! Both lens tracks of each master are compared field by field, as the
//! frame lock will compare them (section 9), and each LRV's clip time 0
//! with its master's. A table per file says what each field holds and
//! which of P-TRK's branches it takes: Pass when every field agrees, R1
//! for a constant offset in the first pts or the edit list, R2 for sync
//! samples that differ, and Kill for decode times that differ over the
//! common prefix, which fails the test, as does a difference no branch
//! names. Counts that differ only at the end take no branch: the pair
//! ends at the shorter lens. FFmpeg's demuxer reads the head of every
//! track as its tables say.
//!
//! Run in the cloud, with `SPHERECUT_TIER2` unset, by
//! `SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 cargo test
//! -p spherecut-media --test tier2_tracks -- --ignored samples_x5_tracks_equal`;
//! hw-test runs it with the rest.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use spherecut_media::mp4::{self, TrackTiming};
use spherecut_media::packets::{NextPacket, PacketReader};
use spherecut_testkit::control::{below, must_fail};
use spherecut_testkit::samples::{self, Role};
use spherecut_testkit::{fnv64, tier2};

/// What a field's difference between two lens tracks comes to under
/// P-TRK, in the order the worst of them decides a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Branch {
    /// The two agree.
    Same,
    /// They differ only at the end: the pair ends at the shorter lens.
    EndsApart,
    /// R1: a constant offset in the first pts or the edit list.
    R1,
    /// R2: the sync samples differ.
    R2,
    /// Kill: the decode times differ over the common prefix.
    Kill,
    /// A difference no branch of P-TRK names, for a person to look at.
    Unforeseen,
}

impl Branch {
    fn name(self) -> &'static str {
        match self {
            Branch::Same => "same",
            Branch::EndsApart => "ends apart",
            Branch::R1 => "R1",
            Branch::R2 => "R2",
            Branch::Kill => "Kill",
            Branch::Unforeseen => "unforeseen",
        }
    }

    /// Whether the lock can go on with it, as designed or by R1 or R2.
    fn allowed(self) -> bool {
        self < Branch::Kill
    }
}

/// One field of two tracks: what each holds, the branch, and why.
struct Field {
    name: &'static str,
    a: String,
    b: String,
    branch: Branch,
    why: String,
}

fn field(
    name: &'static str,
    a: String,
    b: String,
    branch: Branch,
    why: impl Into<String>,
) -> Field {
    Field {
        name,
        a,
        b,
        branch,
        why: why.into(),
    }
}

/// Seconds a frame of `t` lasts, by its first delta.
fn frame_s(t: &TrackTiming) -> f64 {
    let delta = t.stts.iter().find(|r| r.0 > 0).map_or(0, |r| r.1);
    f64::from(delta) / f64::from(t.timescale)
}

/// When `t`'s first frame is shown, in ms.
fn first_ms(t: &TrackTiming) -> f64 {
    t.first_pts as f64 * 1000.0 / f64::from(t.timescale)
}

/// `stts` as its runs, (samples) × (delta).
fn runs(stts: &[(u32, u32)]) -> String {
    let shown: Vec<String> = stts
        .iter()
        .take(3)
        .map(|(n, d)| format!("{n} × {d}"))
        .collect();
    match stts.len() {
        0..=3 => shown.join(" + "),
        n => format!("{} + … ({n} runs)", shown.join(" + ")),
    }
}

/// The sync samples, briefly.
fn sync(stss: Option<&[u32]>) -> String {
    let Some(s) = stss else {
        return "every sample".into();
    };
    if let [first, second, ..] = s {
        let gap = second - first;
        if s.windows(2).all(|w| w[1] - w[0] == gap) {
            let last = s[s.len() - 1];
            return format!("{first}, every {gap} to {last} ({} entries)", s.len());
        }
    }
    let head: Vec<String> = s.iter().take(6).map(u32::to_string).collect();
    match s.len() {
        0..=6 => format!("[{}]", head.join(", ")),
        n => format!("[{}, …] ({n} entries)", head.join(", ")),
    }
}

/// The edit list, each edit's media time (or "empty") and duration in
/// movie ticks.
fn edits(t: &TrackTiming) -> String {
    let Some(list) = &t.edit else {
        return "none".into();
    };
    if list.edits.is_empty() {
        return "no edits".into();
    }
    let edit = |e: &mp4::Edit| {
        let what = match e.media_time {
            -1 => "empty".to_string(),
            m => format!("from {m}"),
        };
        let rate = match e.rate {
            0x0001_0000 => String::new(),
            r => format!(" at rate {r:#x}"),
        };
        format!("{what} for {}/{}{rate}", e.duration, list.movie_timescale)
    };
    let all: Vec<String> = list.edits.iter().map(edit).collect();
    all.join(", ")
}

/// Whether `t`'s composition offsets are all 0: no `ctts`, or one run of
/// offset 0 over every sample, which is what its hash is of.
fn zero_offsets(t: &TrackTiming) -> bool {
    let run = [(t.count as u32).to_be_bytes(), 0u32.to_be_bytes()].concat();
    t.ctts_hash.is_none_or(|h| h == fnv64(&run))
}

/// Whether `a` and `b` are equal as fractions of their timescales.
fn same_time(a: i64, ta: u32, b: i64, tb: u32) -> bool {
    i128::from(a) * i128::from(tb) == i128::from(b) * i128::from(ta)
}

/// How the edit lists of two tracks differ: not at all, only in how long
/// the last edit lasts, or otherwise. Times are compared in seconds.
fn edit_branch(a: &TrackTiming, b: &TrackTiming) -> (Branch, String) {
    let (Some(x), Some(y)) = (&a.edit, &b.edit) else {
        return match (&a.edit, &b.edit) {
            (None, None) => (Branch::Same, String::new()),
            _ => (
                Branch::R1,
                "one lens has an edit list, the other none".into(),
            ),
        };
    };
    if x.edits.len() != y.edits.len() {
        return (Branch::R1, "the lists have different edits".into());
    }
    let last = x.edits.len().saturating_sub(1);
    let mut end_differs = false;
    for (i, (e, f)) in x.edits.iter().zip(&y.edits).enumerate() {
        let starts = match (e.media_time, f.media_time) {
            (-1, -1) => true,
            (-1, _) | (_, -1) => false,
            (m, n) => same_time(m, a.timescale, n, b.timescale),
        };
        if !starts || e.rate != f.rate {
            return (Branch::R1, format!("edit {i} shows different media"));
        }
        let lasts = |d: u64, ts: u32| i128::from(d) * i128::from(ts);
        if lasts(e.duration, y.movie_timescale) != lasts(f.duration, x.movie_timescale) {
            if i < last {
                return (Branch::R1, format!("edit {i} lasts differently"));
            }
            end_differs = true;
        }
    }
    if end_differs {
        (
            Branch::EndsApart,
            "only the last edit's duration differs".into(),
        )
    } else {
        (Branch::Same, String::new())
    }
}

/// Lens 0 against lens 1, field by field, as P-TRK reads them.
fn compare(a: &TrackTiming, b: &TrackTiming) -> Vec<Field> {
    let mut out = Vec::new();

    let same = a.timescale == b.timescale;
    out.push(field(
        "timescale",
        a.timescale.to_string(),
        b.timescale.to_string(),
        if same {
            Branch::Same
        } else {
            Branch::Unforeseen
        },
        if same {
            ""
        } else {
            "the lock compares time bases (section 9)"
        },
    ));

    let same = a.count == b.count;
    out.push(field(
        "count",
        a.count.to_string(),
        b.count.to_string(),
        if same {
            Branch::Same
        } else {
            Branch::EndsApart
        },
        if same {
            ""
        } else {
            "the pair ends at the shorter lens"
        },
    ));

    let at = |t: &TrackTiming| format!("{} ({:.3} ms)", t.first_pts, first_ms(t));
    let (branch, why) = if same_time(a.first_pts, a.timescale, b.first_pts, b.timescale) {
        (Branch::Same, String::new())
    } else {
        let ms = first_ms(b) - first_ms(a);
        let frames = ms / 1000.0 / frame_s(a);
        let why = format!("lens 1 starts {ms:+.3} ms, {frames:+.3} frames from lens 0");
        (Branch::R1, why)
    };
    out.push(field("first_pts", at(a), at(b), branch, why));

    let (branch, why) = edit_branch(a, b);
    out.push(field("edit", edits(a), edits(b), branch, why));

    let (branch, why) = match a.stts_mismatch(b) {
        None if a.stts == b.stts => (Branch::Same, String::new()),
        None => (Branch::Same, "the same times, in runs split apart".into()),
        Some(k) => (Branch::Kill, format!("decode times part at sample {k}")),
    };
    out.push(field("stts", runs(&a.stts), runs(&b.stts), branch, why));

    let hash = |t: &TrackTiming| match t.ctts_hash {
        None => "none".into(),
        Some(h) if zero_offsets(t) => format!("{h:#018x} (every offset 0)"),
        Some(h) => format!("{h:#018x}"),
    };
    let (branch, why) = if a.ctts_hash == b.ctts_hash || zero_offsets(a) && zero_offsets(b) {
        (Branch::Same, "")
    } else if a.count == b.count {
        (Branch::Unforeseen, "the composition offsets differ")
    } else {
        (
            Branch::Unforeseen,
            "the hash covers each whole table, so with the counts apart compare the tables by hand",
        )
    };
    out.push(field("ctts_hash", hash(a), hash(b), branch, why));

    let (branch, why) = match a.stss_mismatch(b) {
        None => (Branch::Same, String::new()),
        Some(k) => (
            Branch::R2,
            format!("sample {k} is a sync sample in one lens only"),
        ),
    };
    out.push(field(
        "stss",
        sync(a.stss.as_deref()),
        sync(b.stss.as_deref()),
        branch,
        why,
    ));
    out
}

/// Whether the lock can go on, or the fields that stop it.
fn agree(fields: &[Field]) -> Result<(), String> {
    let stop: Vec<String> = fields
        .iter()
        .filter(|f| !f.branch.allowed())
        .map(|f| format!("{}: {} ({})", f.name, f.branch.name(), f.why))
        .collect();
    if stop.is_empty() {
        Ok(())
    } else {
        Err(stop.join("; "))
    }
}

fn print_table(title: &str, fields: &[Field]) {
    eprintln!("{title}");
    eprintln!("  {:<10} {:<38} {:<38} branch", "field", "lens 0", "lens 1");
    for f in fields {
        let why = if f.why.is_empty() {
            String::new()
        } else {
            format!(": {}", f.why)
        };
        eprintln!(
            "  {:<10} {:<38} {:<38} {}{why}",
            f.name,
            f.a,
            f.b,
            f.branch.name()
        );
    }
}

/// How much later than its master's lens 0 an LRV's first frame is
/// shown, in ms and in master frames. Each file's clip time 0 is its
/// first frame, shown `first_pts` after the start of its movie.
fn lrv_offset(master: &TrackTiming, lrv: &TrackTiming) -> (f64, f64) {
    let ms = first_ms(lrv) - first_ms(master);
    (ms, ms / 1000.0 / frame_s(master))
}

/// Whether FFmpeg's demuxer reads the head of `stream` as `t` says: its
/// time base, the first frame's pts, and which packets are keyframes,
/// through the second sync sample and one past it. The packets are the
/// samples from the first only when the edit list hides none, as on the
/// X5, so a track whose list hides some is reported rather than read.
fn demuxer_agrees(path: &Path, stream: usize, t: &TrackTiming) -> Result<(), String> {
    let shown = t
        .edit
        .as_ref()
        .and_then(|e| e.edits.iter().find(|e| e.media_time != -1));
    if let Some(e) = shown.filter(|e| e.media_time != 0) {
        return Err(format!(
            "its edit list starts at media time {}, which this check doesn't follow",
            e.media_time
        ));
    }
    let n = match &t.stss {
        Some(s) => s.get(1).map_or(t.count, |&k| u64::from(k) + 1),
        None => 10,
    }
    .min(t.count);
    let mut r = PacketReader::open(path, stream, Arc::new(AtomicBool::new(false)))
        .map_err(|e| e.to_string())?;
    let tb = r.time_base();
    if (tb.num(), tb.den()) != (1, i64::from(t.timescale)) {
        return Err(format!(
            "its time base is {}/{}, but its timescale {}",
            tb.num(),
            tb.den(),
            t.timescale
        ));
    }
    let (mut read, mut keys, mut first) = (0u64, Vec::new(), None::<i64>);
    while read < n {
        match r.next_packet() {
            NextPacket::Packet(p) => {
                read += 1;
                if p.key {
                    keys.push(read);
                }
                if let Some(pts) = p.pts {
                    first = Some(first.map_or(pts, |f| f.min(pts)));
                }
            }
            NextPacket::End { .. } | NextPacket::Stopped => break,
        }
    }
    let sync: Vec<u64> = match &t.stss {
        Some(s) => s
            .iter()
            .map(|&k| u64::from(k))
            .filter(|&k| k <= read)
            .collect(),
        None => (1..=read).collect(),
    };
    if read != n {
        Err(format!("the demuxer gave {read} packets of the first {n}"))
    } else if first != Some(t.first_pts) {
        Err(format!(
            "the demuxer's first pts is {first:?}, the tables' {}",
            t.first_pts
        ))
    } else if keys != sync {
        Err(format!(
            "the demuxer's keyframes are {keys:?}, the tables' {sync:?}"
        ))
    } else {
        Ok(())
    }
}

/// A take's three video tracks: the master's two lenses and the LRV's.
struct Take {
    role: &'static Role,
    lens: [TrackTiming; 2],
    lrv: TrackTiming,
}

#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_x5_tracks_equal() {
    let started = Instant::now();
    tier2::require_samples_cpu();
    let shot: Vec<&'static Role> = samples::roles()
        .iter()
        .filter(|r| r.file.is_some())
        .collect();
    assert!(!shot.is_empty());
    let mut problems = Vec::new();
    let mut takes: Vec<(Take, Vec<Field>)> = Vec::new();
    for &role in &shot {
        let (master, lrv) = (samples::role(role.name), samples::lrv(role.name));
        let timing = |path: &Path, stream| {
            mp4::track_timing(path, stream)
                .unwrap_or_else(|e| panic!("{} stream {stream}: {e}", path.display()))
        };
        let take = Take {
            role,
            lens: [timing(&master, 0), timing(&master, 1)],
            lrv: timing(&lrv, 0),
        };
        let fields = compare(&take.lens[0], &take.lens[1]);
        let name = master.file_name().unwrap().to_string_lossy().into_owned();
        print_table(
            &format!("{name} ({}, {})", role.name, role.mode.name()),
            &fields,
        );
        if let Err(e) = agree(&fields) {
            problems.push(format!("{name}: {e}"));
        }

        let (ms, frames) = lrv_offset(&take.lens[0], &take.lrv);
        let l = &take.lrv;
        let within = below(frames.abs(), 1.0);
        eprintln!(
            "  LRV: {} at {}, {} frames ({}), first pts {}, {}; {ms:+.3} ms, {frames:+.3} master frames from lens 0: {}",
            runs(&l.stts),
            l.timescale,
            l.count,
            sync(l.stss.as_deref()),
            l.first_pts,
            edits(l),
            if within.passes() == Some(true) {
                "within one frame"
            } else {
                "an offset for the proxy's time map (15)"
            },
        );
        if within.passes() != Some(true) {
            problems.push(format!("{name}'s LRV: {within} master frames"));
        }

        for (path, stream, t) in [
            (&master, 0, &take.lens[0]),
            (&master, 1, &take.lens[1]),
            (&lrv, 0, l),
        ] {
            if let Err(e) = demuxer_agrees(path, stream, t) {
                problems.push(format!("{} stream {stream}: {e}", path.display()));
            }
        }
        takes.push((take, fields));
    }

    // P-TRK's verdict per field over the masters, and the worst of all.
    eprintln!("P-TRK over {} masters:", takes.len());
    let mut worst = Branch::Same;
    for i in 0..takes[0].1.len() {
        let mut by: BTreeMap<Branch, Vec<String>> = BTreeMap::new();
        for (take, fields) in &takes {
            let f = &fields[i];
            by.entry(f.branch)
                .or_default()
                .push(take.role.file.unwrap_or("?").to_string());
            worst = worst.max(f.branch);
        }
        let verdicts: Vec<String> = by
            .iter()
            .map(|(b, files)| format!("{} in {} ({})", b.name(), files.len(), files.join(", ")))
            .collect();
        eprintln!("  {:<10} {}", takes[0].1[i].name, verdicts.join("; "));
    }
    let verdict = match worst {
        Branch::Same | Branch::EndsApart => "Pass".to_string(),
        b => b.name().to_string(),
    };
    eprintln!("P-TRK: {verdict}");
    let offsets: Vec<f64> = takes
        .iter()
        .map(|(t, _)| lrv_offset(&t.lens[0], &t.lrv).1.abs())
        .collect();
    eprintln!(
        "LRVs: largest offset {:.3} master frames over {}",
        offsets.iter().copied().fold(0.0, f64::max),
        offsets.len()
    );

    // Controls: lens 0 of the 8K30 master against the 8K25 master's,
    // whose frames part at the second; an LRV a master frame late; and
    // tables the 8K30 file's packets don't have.
    let at = |name: &str| &takes.iter().find(|(t, _)| t.role.name == name).unwrap().0;
    let (k30, k25) = (at("clip1_8k30"), at("clip2_8k25"));
    let fields = compare(&k30.lens[0], &k25.lens[0]);
    print_table("control: 004's lens 0 against 005's lens 0", &fields);
    let why = must_fail("29.97 against 25 fps", agree(&fields));
    assert!(
        why.contains("stts: Kill (decode times part at sample 1)"),
        "{why}"
    );
    let late = TrackTiming {
        first_pts: k30.lrv.first_pts + i64::from(k30.lens[0].stts[0].1),
        ..k30.lrv.clone()
    };
    assert_eq!(late.timescale, k30.lens[0].timescale);
    let (_, frames) = lrv_offset(&k30.lens[0], &late);
    assert_eq!(
        must_fail("an LRV a frame late", below(frames.abs(), 1.0)),
        frames.abs()
    );
    let lens = &k30.lens[0];
    let planted = [
        (
            "005's tables",
            k25.lens[0].clone(),
            "its time base is 1/30000, but its timescale 25000",
        ),
        (
            "the LRV's sync samples",
            TrackTiming {
                stss: k30.lrv.stss.clone(),
                ..lens.clone()
            },
            "the demuxer's keyframes are [1], the tables' [1, 31]",
        ),
        (
            "a first frame a tick late",
            TrackTiming {
                first_pts: lens.first_pts + 1,
                ..lens.clone()
            },
            "the demuxer's first pts is Some(0), the tables' 1",
        ),
    ];
    let master = samples::role("clip1_8k30");
    for (name, tables, failure) in planted {
        let why = must_fail(name, demuxer_agrees(&master, 0, &tables));
        assert_eq!(why, failure);
    }

    eprintln!(
        "samples_x5_tracks_equal took {:.2} s",
        started.elapsed().as_secs_f64()
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
