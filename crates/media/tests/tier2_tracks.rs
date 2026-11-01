//! Tier 2c: P-TRK, the timing tables of Pat's X5 files (M3 plan 4.3).
//!
//! Both lens tracks of each master are compared field by field, as the
//! frame lock will compare them (section 9), and each LRV's clip time 0
//! with its master's. A table per file says what each field holds and
//! which of P-TRK's branches it takes: Pass when every field agrees, R1
//! for a constant offset in the first pts or the edit list, R2 for sync
//! samples that differ, and Kill for decode times that differ over the
//! common prefix, which fails the test, as does a difference no branch
//! names. An edit list differs by a constant offset only when each shows
//! the media in one edit, after any empty ones, and the two part only in
//! the empty edits or where that edit starts. Counts that differ only at the end take no branch: the pair
//! ends at the shorter lens. An LRV passes within one master frame of
//! its master, and otherwise its offset is reported for the proxy's time
//! map (section 15). FFmpeg's demuxer reads the head of every track as
//! its tables say.
//!
//! Run in the cloud, with `SPHERECUT_TIER2` unset, by
//! `SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 cargo test
//! -p spherecut-media --test tier2_tracks -- --ignored samples_x5_tracks_equal`;
//! hw-test runs it with the rest.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use spherecut_media::mp4::{self, Edit, EditList, TrackTiming};
use spherecut_media::packets::{NextPacket, PacketReader};
use spherecut_testkit::control::{below, must_fail};
use spherecut_testkit::samples::{self, Role};
use spherecut_testkit::{fnv64, tier2};

/// Rate 1.0 in an edit: 16.16 fixed point.
const RATE_1: u32 = 0x0001_0000;

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

/// P-TRK's verdict on the branches some fields took: Pass when no field
/// took one beyond ending at the shorter lens, and otherwise every
/// branch that fired, such as "R1 + R2".
fn verdict(branches: impl IntoIterator<Item = Branch>) -> String {
    let fired: BTreeSet<Branch> = branches
        .into_iter()
        .filter(|&b| b > Branch::EndsApart)
        .collect();
    if fired.is_empty() {
        return "Pass".into();
    }
    let names: Vec<&str> = fired.iter().map(|b| b.name()).collect();
    names.join(" + ")
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

/// Whether `a` and `b` are equal as fractions of their timescales. A
/// time here is at most a sum of 2^24 edits of 64 bits, so the products
/// fit in an i128.
fn same_time(a: i128, ta: u32, b: i128, tb: u32) -> bool {
    a * i128::from(tb) == b * i128::from(ta)
}

/// An edit list that shows the media once, from one point on: empty
/// edits, if any, then one edit that shows media. No list, or a list of
/// no edits, which FFmpeg ignores, shows all of it from the start.
struct Shown {
    /// The empty edits before it, in ticks of the movie's timescale.
    delay: u128,
    /// Where in the media it starts, in ticks of the track's.
    start: i64,
    /// How long it shows, as (ticks, ticks a second).
    lasts: (i128, u32),
    rate: u32,
    movie_timescale: u32,
}

/// `t`'s edit list as one [`Shown`], or `None` when it shows media in
/// more than one edit.
fn shown(t: &TrackTiming) -> Option<Shown> {
    let media: u64 = t
        .stts
        .iter()
        .map(|&(n, d)| u64::from(n) * u64::from(d))
        .sum();
    let all = Shown {
        delay: 0,
        start: 0,
        lasts: (i128::from(media), t.timescale),
        rate: RATE_1,
        movie_timescale: 1,
    };
    let Some(list) = t.edit.as_ref().filter(|l| !l.edits.is_empty()) else {
        return Some(all);
    };
    // track_timing refuses a list whose edits are all empty.
    let k = list.edits.iter().position(|e| e.media_time != -1)?;
    let [e] = &list.edits[k..] else {
        return None;
    };
    Some(Shown {
        delay: list.edits[..k].iter().map(|e| u128::from(e.duration)).sum(),
        start: e.media_time,
        lasts: (i128::from(e.duration), list.movie_timescale),
        rate: e.rate,
        movie_timescale: list.movie_timescale,
    })
}

/// How the edit lists of two tracks differ: not at all, only in how long
/// they last, by a constant offset (R1), or otherwise. Times are compared
/// in seconds.
fn edit_branch(a: &TrackTiming, b: &TrackTiming) -> (Branch, String) {
    let (Some(x), Some(y)) = (shown(a), shown(b)) else {
        return lists_branch(a, b);
    };
    if x.rate != y.rate {
        return (
            Branch::Unforeseen,
            format!("the edits play at rates {:#x} and {:#x}", x.rate, y.rate),
        );
    }
    let delays = same_time(
        x.delay as i128,
        x.movie_timescale,
        y.delay as i128,
        y.movie_timescale,
    );
    let starts = same_time(x.start.into(), a.timescale, y.start.into(), b.timescale);
    if !delays || !starts {
        // When each lens shows a media time, in seconds from the start of
        // the movie less that media time.
        let at = |s: &Shown, t: &TrackTiming| {
            s.delay as f64 / f64::from(s.movie_timescale) - s.start as f64 / f64::from(t.timescale)
        };
        let ms = (at(&y, b) - at(&x, a)) * 1000.0;
        let frames = ms / 1000.0 / frame_s(a);
        return (
            Branch::R1,
            format!(
                "a constant offset: lens 1 shows each media time {ms:+.3} ms, {frames:+.3} frames, from lens 0"
            ),
        );
    }
    if same_time(x.lasts.0, x.lasts.1, y.lasts.0, y.lasts.1) {
        (Branch::Same, String::new())
    } else {
        (Branch::EndsApart, "only how long they show differs".into())
    }
}

/// How two edit lists differ when one of them shows media in more than
/// one edit: not at all, only in how long the last edit lasts, or
/// otherwise, which no constant offset is.
fn lists_branch(a: &TrackTiming, b: &TrackTiming) -> (Branch, String) {
    let shows = |t: &TrackTiming| {
        t.edit
            .as_ref()
            .map_or(1, |l| l.edits.iter().filter(|e| e.media_time != -1).count())
    };
    let (Some(x), Some(y)) = (&a.edit, &b.edit) else {
        return (
            Branch::Unforeseen,
            format!(
                "the lenses show their media in {} and {} edits, which no constant offset makes the same",
                shows(a),
                shows(b)
            ),
        );
    };
    if x.edits.len() != y.edits.len() {
        return (
            Branch::Unforeseen,
            format!(
                "the lists hold {} and {} edits, which no constant offset makes the same",
                x.edits.len(),
                y.edits.len()
            ),
        );
    }
    let last = x.edits.len().saturating_sub(1);
    let mut end_differs = false;
    for (i, (e, f)) in x.edits.iter().zip(&y.edits).enumerate() {
        let starts = match (e.media_time, f.media_time) {
            (-1, -1) => true,
            (-1, _) | (_, -1) => false,
            (m, n) => same_time(m.into(), a.timescale, n.into(), b.timescale),
        };
        if !starts || e.rate != f.rate {
            return (
                Branch::Unforeseen,
                format!("edit {i} shows different media"),
            );
        }
        let lasts = |d: u64, ts: u32| i128::from(d) * i128::from(ts);
        if lasts(e.duration, y.movie_timescale) != lasts(f.duration, x.movie_timescale) {
            if i < last {
                return (Branch::Unforeseen, format!("edit {i} lasts differently"));
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
    let (branch, why) = if same_time(
        a.first_pts.into(),
        a.timescale,
        b.first_pts.into(),
        b.timescale,
    ) {
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
/// first frame, shown `first_pts` after the start of its movie. The
/// offset is taken in whole ticks of both timescales, so that a frame
/// late is exactly 1.
fn lrv_offset(master: &TrackTiming, lrv: &TrackTiming) -> (f64, f64) {
    let (tm, tl) = (i128::from(master.timescale), i128::from(lrv.timescale));
    // tm × tl of these a second.
    let ticks = i128::from(lrv.first_pts) * tm - i128::from(master.first_pts) * tl;
    let delta = master.stts.iter().find(|r| r.0 > 0).map_or(0, |r| r.1);
    let ms = ticks as f64 * 1000.0 / (tm * tl) as f64;
    (ms, ticks as f64 / (i128::from(delta) * tl) as f64)
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
    // The LRVs whose offset goes into the proxy's time map (section 15).
    let mut time_map = Vec::new();
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
        let lrv_verdict = match within.passes() {
            Some(true) => "within one frame".to_string(),
            Some(false) => {
                time_map.push(format!("{} ({ms:+.3} ms)", role.file.unwrap_or("?")));
                format!("time map, {ms:+.3} ms, {frames:+.3} frames (section 15)")
            }
            None => {
                problems.push(format!("{name}'s LRV: {within} decides nothing"));
                "undecided".to_string()
            }
        };
        eprintln!(
            "  LRV: {} at {}, {} frames ({}), first pts {}, {}; {ms:+.3} ms, {frames:+.3} master frames from lens 0: {lrv_verdict}",
            runs(&l.stts),
            l.timescale,
            l.count,
            sync(l.stss.as_deref()),
            l.first_pts,
            edits(l),
        );

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

    // P-TRK's verdict per field over the masters, and over all of them.
    eprintln!("P-TRK over {} masters:", takes.len());
    for i in 0..takes[0].1.len() {
        let mut by: BTreeMap<Branch, Vec<String>> = BTreeMap::new();
        for (take, fields) in &takes {
            let f = &fields[i];
            by.entry(f.branch)
                .or_default()
                .push(take.role.file.unwrap_or("?").to_string());
        }
        let verdicts: Vec<String> = by
            .iter()
            .map(|(b, files)| format!("{} in {} ({})", b.name(), files.len(), files.join(", ")))
            .collect();
        eprintln!("  {:<10} {}", takes[0].1[i].name, verdicts.join("; "));
    }
    let all = takes.iter().flat_map(|(_, f)| f.iter().map(|f| f.branch));
    eprintln!("P-TRK: {}", verdict(all));
    let offsets: Vec<f64> = takes
        .iter()
        .map(|(t, _)| lrv_offset(&t.lens[0], &t.lrv).1.abs())
        .collect();
    let mapped = match time_map.len() {
        0 => "none needs the time map".to_string(),
        n => format!("{n} need the time map: {}", time_map.join(", ")),
    };
    eprintln!(
        "LRVs: largest offset {:.3} master frames over {}; {} within one frame, {mapped}",
        offsets.iter().copied().fold(0.0, f64::max),
        offsets.len(),
        offsets.len() - time_map.len(),
    );

    // Controls: lens 0 of the 8K30 master against the 8K25 master's,
    // whose frames part at the second; lens 1 of the 8K30 master shown a
    // frame late, which R1 lets through, and skipping a frame, which no
    // branch names; an LRV a master frame late; and tables the 8K30
    // file's packets don't have.
    let at = |name: &str| &takes.iter().find(|(t, _)| t.role.name == name).unwrap().0;
    let (k30, k25) = (at("clip1_8k30"), at("clip2_8k25"));
    let fields = compare(&k30.lens[0], &k25.lens[0]);
    print_table("control: 004's lens 0 against 005's lens 0", &fields);
    let why = must_fail("29.97 against 25 fps", agree(&fields));
    assert!(
        why.contains("stts: Kill (decode times part at sample 1)"),
        "{why}"
    );
    let lens1 = &k30.lens[1];
    let list = lens1.edit.as_ref().unwrap();
    let (delta, mts) = (u64::from(lens1.stts[0].1), list.movie_timescale);
    // A frame in the movie's ticks, which on the X5 are the track's.
    let frame = delta * u64::from(mts) / u64::from(lens1.timescale);
    assert_eq!(frame * u64::from(lens1.timescale), delta * u64::from(mts));
    let edit = |edits: Vec<Edit>| {
        Some(EditList {
            movie_timescale: mts,
            edits,
        })
    };
    let empty = Edit {
        duration: frame,
        media_time: -1,
        rate: RATE_1,
    };
    let shifted = TrackTiming {
        first_pts: lens1.first_pts + delta as i64,
        edit: edit([vec![empty], list.edits.clone()].concat()),
        ..lens1.clone()
    };
    let fields = compare(&k30.lens[0], &shifted);
    print_table("control: 004's lens 1 a frame late", &fields);
    assert_eq!(agree(&fields), Ok(()));
    let fired: Vec<(&str, Branch)> = fields
        .iter()
        .filter(|f| f.branch != Branch::Same)
        .map(|f| (f.name, f.branch))
        .collect();
    assert_eq!(fired, [("first_pts", Branch::R1), ("edit", Branch::R1)]);
    assert_eq!(verdict(fired.iter().map(|f| f.1)), "R1");
    // Frames 0 to 49, then 51 on: no one offset lines the lenses up.
    let skip = TrackTiming {
        edit: edit(vec![
            Edit {
                duration: 50 * frame,
                media_time: 0,
                rate: RATE_1,
            },
            Edit {
                duration: (lens1.count - 51) * frame,
                media_time: 51 * delta as i64,
                rate: RATE_1,
            },
        ]),
        ..lens1.clone()
    };
    let fields = compare(&k30.lens[0], &skip);
    print_table("control: 004's lens 1 skipping frame 50", &fields);
    let why = must_fail("lens 1 skipping frame 50", agree(&fields));
    assert_eq!(
        why,
        "edit: unforeseen (the lists hold 1 and 2 edits, which no constant offset makes the same)"
    );
    let late = TrackTiming {
        first_pts: k30.lrv.first_pts + i64::from(k30.lens[0].stts[0].1),
        ..k30.lrv.clone()
    };
    assert_eq!(late.timescale, k30.lens[0].timescale);
    let (_, frames) = lrv_offset(&k30.lens[0], &late);
    assert_eq!(
        must_fail("an LRV a frame late", below(frames.abs(), 1.0)),
        1.0
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
