//! The perf clips hw-test makes for the playback benchmarks (M1 plan
//! section 12.4, DECISIONS D37): synth's [`media::perf_clips`], and from
//! M3.1 its two-track [`media::perf_pairs`], made once into
//! `<target>/hw-media/` and kept across runs, since x265 takes minutes
//! over each. A clip is made again when synth's spec of it changes: the
//! spec it was made from is kept beside it, in `<file>.spec`; a pair's
//! stamp also holds the corner codes' layout, the noise's format and the
//! x265 that coded it, as its streams name it. A run that can't read its
//! x265 keeps a pair whose stamp holds all the rest, and says so. A clip
//! is written in `.partial/` and moved into place only once whole, so an
//! interrupted run leaves nothing to reuse.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spherecut_synth::code;
use spherecut_synth::media::{self, PairSpec, Spec, TrackCoding, X265Version};

/// How a clip came to be there.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Made {
    /// From an earlier run, made from the same spec.
    Cached,
    /// Now, in this long.
    Now(Duration),
    /// It couldn't be made, and why.
    Failed(String),
}

/// A perf clip, as the report gives it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Clip {
    pub name: String,
    pub path: PathBuf,
    pub made: Made,
    pub bytes: u64,
    /// Its length, from the spec.
    pub seconds: f64,
    /// For a pair, what is read back from each lens, lens 0 first, so
    /// that each lens is rated on its own; empty for a clip of one
    /// stream, or a pair that isn't there.
    pub lenses: Vec<LensRead>,
    /// For a pair taken as made though this run couldn't read its x265,
    /// why: the x265 that coded it wasn't checked against its stamp.
    pub unchecked: Option<String>,
}

/// What hw-test reads back from one lens of a pair it made or found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LensRead {
    /// The bytes of its samples, or why they couldn't be counted.
    pub bytes: Result<u64, String>,
    /// Its sync samples and its pictures' NAL types, or why they
    /// couldn't be read.
    pub coding: Result<TrackCoding, String>,
}

impl Clip {
    /// The average bitrate in Mbit/s, from the file's size.
    pub(crate) fn mbps(&self) -> Option<f64> {
        rate(self.bytes, self.seconds)
    }

    /// The average bitrate of a pair's lens `k` in Mbit/s, from its
    /// samples.
    pub(crate) fn track_mbps(&self, k: usize) -> Option<f64> {
        rate(*self.lenses.get(k)?.bytes.as_ref().ok()?, self.seconds)
    }
}

fn rate(bytes: u64, seconds: f64) -> Option<f64> {
    (bytes > 0 && seconds > 0.0).then(|| bytes as f64 * 8.0 / seconds / 1e6)
}

/// What a perf clip is made from: a clip of one stream, or a pair of
/// lenses in one file.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    One(Box<Spec>),
    Pair(Box<PairSpec>),
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Source::One(s) => &s.name,
            Source::Pair(p) => &p.name,
        }
    }

    fn file_name(&self) -> String {
        match self {
            Source::One(s) => s.file_name(),
            Source::Pair(p) => p.file_name(),
        }
    }

    /// Its spec, a pair's lens 0's.
    fn first(&self) -> &Spec {
        match self {
            Source::One(s) => s,
            Source::Pair(p) => &p.lenses[0],
        }
    }

    /// Its length: a pair's lens 0's.
    fn seconds(&self) -> f64 {
        f64::from(self.first().frames) / self.first().rate.to_f64()
    }

    /// What it is stamped with: everything it says, so that any change
    /// to it makes the clip again, and for a pair the corner codes'
    /// layout, the noise's format and `x265`, the x265 that codes it now
    /// ([`x265`]), so that a pair coded otherwise is made again too. A
    /// clip of one stream is stamped as it was before pairs, so that M2's
    /// clips aren't made again.
    fn stamp(&self, x265: &Result<X265Version, String>) -> String {
        match self {
            Source::One(_) => self.stamp_but_x265(),
            Source::Pair(_) => format!("{}{}", self.stamp_but_x265(), x265_line(x265)),
        }
    }

    /// Its stamp up to a pair's x265 line: all of a clip of one stream's.
    fn stamp_but_x265(&self) -> String {
        match self {
            Source::One(s) => format!("{s:#?}\n"),
            Source::Pair(p) => format!(
                "{p:#?}\ncorner codes: format {}\nnoise: format {}\n",
                code::FORMAT,
                media::NOISE_FORMAT
            ),
        }
    }

    fn write(&self, dir: &Path) -> Result<PathBuf, String> {
        match self {
            Source::One(s) => media::write(s, dir),
            Source::Pair(p) => media::write_pair(p, dir),
        }
        .map_err(|e| e.to_string())
    }

    /// What is read back from each lens of the pair at `path`, or
    /// nothing for a clip of one stream.
    fn lenses(&self, path: &Path) -> Vec<LensRead> {
        match self {
            Source::One(_) => Vec::new(),
            Source::Pair(_) if !path.is_file() => Vec::new(),
            Source::Pair(_) => (0..2)
                .map(|k| LensRead {
                    bytes: media::track_bytes(path, k).map_err(|e| e.to_string()),
                    coding: media::track_coding(path, k).map_err(|e| e.to_string()),
                })
                .collect(),
        }
    }
}

/// The x265 that codes the perf clips, as the streams it codes name it
/// ([`media::x265_version`], from a frame coded into `dir`), or why that
/// couldn't be read: the report gives it beside `pkg-config`'s, and a
/// pair's stamp holds it.
pub(crate) fn x265(dir: &Path) -> Result<X265Version, String> {
    media::x265_version(dir).map_err(|e| e.to_string())
}

/// A pair's stamp's last line: the x265 that coded it, or "not known",
/// without why, which may name a file of the run that made it.
fn x265_line(x265: &Result<X265Version, String>) -> String {
    let named = x265
        .as_ref()
        .map_or("not known".into(), X265Version::to_string);
    format!("x265: {named}\n")
}

/// Whether `found`, a pair's stamp, holds `but_x265` and then one x265
/// line, whatever x265 it names.
fn stamped_but_x265(found: &str, but_x265: &str) -> bool {
    found
        .strip_prefix(but_x265)
        .and_then(|rest| rest.strip_prefix("x265: "))
        .and_then(|rest| rest.strip_suffix('\n'))
        .is_some_and(|named| !named.contains('\n'))
}

fn stamp_path(clip: &Path) -> PathBuf {
    let mut p = clip.as_os_str().to_owned();
    p.push(".spec");
    PathBuf::from(p)
}

/// Every perf clip in `dir`, made there unless an earlier run made it
/// from the same spec (and a pair with the same `x265`, [`x265`]):
/// synth's clips of one stream, then its pairs.
pub(crate) fn ensure(dir: &Path, x265: &Result<X265Version, String>) -> Vec<Clip> {
    let sources: Vec<Source> = media::perf_clips()
        .into_iter()
        .map(|s| Source::One(Box::new(s)))
        .chain(
            media::perf_pairs()
                .into_iter()
                .map(|p| Source::Pair(Box::new(p))),
        )
        .collect();
    ensure_sources(dir, &sources, x265)
}

/// [`ensure`] for `specs`, clips of one stream.
#[cfg(test)]
pub(crate) fn ensure_specs(dir: &Path, specs: &[Spec]) -> Vec<Clip> {
    let sources: Vec<Source> = specs
        .iter()
        .map(|s| Source::One(Box::new(s.clone())))
        .collect();
    ensure_sources(dir, &sources, &Err("not asked".into()))
}

/// [`ensure`] for `sources`.
pub(crate) fn ensure_sources(
    dir: &Path,
    sources: &[Source],
    x265: &Result<X265Version, String>,
) -> Vec<Clip> {
    sources
        .iter()
        .map(|source| ensure_one(dir, source, x265))
        .collect()
}

fn ensure_one(dir: &Path, source: &Source, x265: &Result<X265Version, String>) -> Clip {
    let path = dir.join(source.file_name());
    let stamp_file = stamp_path(&path);
    let want = source.stamp(x265);
    let seconds = source.seconds();
    let clip = |made, unchecked| Clip {
        name: source.name().to_string(),
        bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
        path: path.clone(),
        made,
        seconds,
        lenses: source.lenses(&path),
        unchecked,
    };
    let found = path
        .is_file()
        .then(|| std::fs::read_to_string(&stamp_file).ok())
        .flatten();
    match (source, x265) {
        // A run that can't read its x265 can't tell whether a pair's
        // coder has changed: it keeps a pair stamped with all the rest,
        // rather than delete hours of x265's work it may be unable to
        // redo (this FFmpeg may lack libx265), and leaves its stamp as it
        // was.
        (Source::Pair(_), Err(why))
            if found
                .as_deref()
                .is_some_and(|f| stamped_but_x265(f, &source.stamp_but_x265())) =>
        {
            return clip(
                Made::Cached,
                Some(format!("this run's x265 wasn't read: {why}")),
            );
        }
        (_, Ok(_)) | (Source::One(_), _) if found.as_ref() == Some(&want) => {
            return clip(Made::Cached, None);
        }
        _ => {}
    }
    eprintln!(
        "==> making perf clip {} (once: it is kept in {})",
        source.file_name(),
        dir.display()
    );
    let start = Instant::now();
    match make(dir, source, &path, &stamp_file, &want) {
        Ok(()) => clip(Made::Now(start.elapsed()), None),
        Err(why) => clip(Made::Failed(why), None),
    }
}

/// Write the clip in `dir/.partial/`, then move it and its stamp into
/// place.
fn make(
    dir: &Path,
    source: &Source,
    path: &Path,
    stamp_file: &Path,
    stamp: &str,
) -> Result<(), String> {
    let _ = std::fs::remove_file(stamp_file);
    let _ = std::fs::remove_file(path);
    let partial = dir.join(".partial");
    let written = source.write(&partial);
    let moved = written.and_then(|p| {
        std::fs::rename(&p, path).map_err(|e| format!("moving {} into place: {e}", p.display()))
    });
    let _ = std::fs::remove_dir_all(&partial);
    moved?;
    std::fs::write(stamp_file, stamp).map_err(|e| format!("writing {}: {e}", stamp_file.display()))
}

#[cfg(test)]
mod tests {
    use spherecut_synth::media::Codec;
    use spherecut_testkit::control::must_fail;

    use super::*;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tiny(name: &str, frames: u32) -> Spec {
        Spec {
            name: name.into(),
            width: 64,
            height: 32,
            codec: Codec::H264,
            frames,
            ..Spec::default()
        }
    }

    #[test]
    fn a_clip_is_made_once_and_again_when_its_spec_changes() {
        let d =
            Dir(std::env::temp_dir().join(format!("spherecut-xtask-perf-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        let specs = [tiny("a", 4), tiny("b", 4)];
        let first = ensure_specs(&d.0, &specs);
        assert!(
            first.iter().all(|c| matches!(c.made, Made::Now(_))),
            "{first:?}"
        );
        assert!(first.iter().all(|c| c.bytes > 0 && c.path.is_file()));
        assert!(!d.0.join(".partial").exists());
        let mbps = first[0].mbps().unwrap();
        assert!(mbps > 0.0, "{mbps}");

        let again = ensure_specs(&d.0, &specs);
        assert!(again.iter().all(|c| c.made == Made::Cached), "{again:?}");
        assert_eq!(again[0].bytes, first[0].bytes);

        // A changed spec, or a clip without its stamp (an interrupted
        // run's), is made again.
        std::fs::remove_file(stamp_path(&first[1].path)).unwrap();
        let changed = [tiny("a", 6), tiny("b", 4)];
        let third = ensure_specs(&d.0, &changed);
        assert!(matches!(third[0].made, Made::Now(_)), "{third:?}");
        assert!(matches!(third[1].made, Made::Now(_)), "{third:?}");
        assert!(third[0].seconds > first[0].seconds);
    }

    #[test]
    fn a_clip_that_cant_be_made_says_why() {
        let d =
            Dir(std::env::temp_dir()
                .join(format!("spherecut-xtask-perf-bad-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        // A lossless clip has no bitrate to set.
        let bad = Spec {
            codec: Codec::H264Lossless,
            bitrate_kbps: Some(1000),
            ..tiny("bad", 2)
        };
        let c = ensure_specs(&d.0, &[bad]);
        assert!(
            matches!(&c[0].made, Made::Failed(why) if why.contains("bitrate")),
            "{c:?}"
        );
        assert!(!c[0].path.exists());
        assert!(!stamp_path(&c[0].path).exists());
    }

    #[test]
    fn the_perf_clips_are_synths() {
        let names: Vec<String> = media::perf_clips().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                media::PERF_5K7_8BIT,
                media::PERF_5K7_10BIT,
                media::PERF_8K_8BIT
            ]
        );
        let pairs: Vec<String> = media::perf_pairs().into_iter().map(|p| p.name).collect();
        assert_eq!(
            pairs,
            [
                media::PERF_PAIR_3840_2997,
                media::PERF_PAIR_2880_5994,
                media::PERF_PAIR_2880_2997
            ]
        );
    }

    /// x265 as two machines' streams might name it.
    fn coder(build: u32, version: &str) -> Result<X265Version, String> {
        Ok(X265Version {
            build,
            version: version.into(),
        })
    }

    fn pair_of(name: &str) -> Source {
        Source::Pair(Box::new(media::pair_64(
            name,
            [media::LensTrack {
                frames: 4,
                keyint: 2,
            }; 2],
        )))
    }

    #[test]
    fn a_pair_is_made_once_and_rated_a_lens_at_a_time() {
        let d =
            Dir(std::env::temp_dir()
                .join(format!("spherecut-xtask-perf-pair-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        let pair = pair_of("pair");
        let x265 = coder(199, "3.5+1-f0c1022b6");
        let first = ensure_sources(&d.0, std::slice::from_ref(&pair), &x265);
        assert!(matches!(first[0].made, Made::Now(_)), "{first:?}");
        assert_eq!(first[0].lenses.len(), 2);
        assert!(
            first[0]
                .lenses
                .iter()
                .all(|l| l.bytes.as_ref().is_ok_and(|&b| b > 0)),
            "{first:?}"
        );
        // The lenses' samples are less than the file, which holds its index
        // too.
        let lens_bytes: u64 = first[0]
            .lenses
            .iter()
            .map(|l| l.bytes.clone().unwrap())
            .sum();
        assert!(lens_bytes < first[0].bytes);
        assert!(first[0].track_mbps(1).unwrap() > 0.0);
        assert_eq!(first[0].track_mbps(2), None);
        // Each lens's coding is read back from the file: an IDR every 2
        // frames, and P frames (TRAIL_R) between. Which IDR type this
        // machine's x265 writes isn't pinned: either reads as an IDR.
        for l in &first[0].lenses {
            let c = l.coding.as_ref().unwrap();
            assert_eq!(c.sync, Some(vec![1, 3]), "{c:?}");
            assert_eq!(c.types.len(), 4, "{c:?}");
            for (i, &t) in c.types.iter().enumerate() {
                if i % 2 == 0 {
                    assert!(matches!(t, 19 | 20), "picture {i}: {c:?}");
                } else {
                    assert_eq!(t, 1, "picture {i}: {c:?}");
                }
            }
        }
        let again = ensure_sources(&d.0, std::slice::from_ref(&pair), &x265);
        assert_eq!(again[0].made, Made::Cached);
        assert_eq!(again[0].lenses, first[0].lenses);
        // A clip of one stream has no lenses to rate.
        let one = ensure_specs(&d.0, &[tiny("one", 2)]);
        assert!(one[0].lenses.is_empty());
        // Its stamp is the spec's alone, as before pairs.
        assert_eq!(
            std::fs::read_to_string(stamp_path(&one[0].path)).unwrap(),
            format!("{:#?}\n", tiny("one", 2))
        );

        // A lens that can't be read says why, rather than showing no size:
        // the file is no MP4 now, though its stamp says it was made.
        std::fs::write(&first[0].path, b"not an MP4").unwrap();
        let broken = ensure_sources(&d.0, &[pair], &x265);
        assert_eq!(broken[0].made, Made::Cached);
        for l in &broken[0].lenses {
            assert!(l.bytes.as_ref().is_err_and(|why| !why.is_empty()), "{l:?}");
            assert!(l.coding.as_ref().is_err_and(|why| !why.is_empty()), "{l:?}");
        }
        assert_eq!(broken[0].track_mbps(0), None);
    }

    #[test]
    fn a_pair_coded_by_another_x265_or_code_layout_is_made_again() {
        let d =
            Dir(std::env::temp_dir()
                .join(format!("spherecut-xtask-perf-x265-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        let pair = pair_of("pair");
        let ensure = |x265: &Result<X265Version, String>| {
            ensure_sources(&d.0, std::slice::from_ref(&pair), x265)
                .remove(0)
                .made
        };
        let stamp_file = stamp_path(&d.0.join("pair.mp4"));
        let x35 = coder(199, "3.5+1-f0c1022b6");
        assert!(matches!(ensure(&x35), Made::Now(_)));
        let stamp = std::fs::read_to_string(&stamp_file).unwrap();
        assert!(
            stamp.ends_with(&format!(
                "\ncorner codes: format {}\nnoise: format {}\nx265: 3.5+1-f0c1022b6 (build \
                 199)\n",
                code::FORMAT,
                media::NOISE_FORMAT
            )),
            "{stamp}"
        );
        assert_eq!(ensure(&x35), Made::Cached);
        // Another x265 makes it again, once.
        let x41 = coder(215, "4.1");
        assert!(matches!(ensure(&x41), Made::Now(_)));
        assert_eq!(ensure(&x41), Made::Cached);
        // A pair made by a run that can't read its x265 is stamped without
        // why, which may name a file of the run, so that it is made once,
        // not every run.
        std::fs::remove_file(&stamp_file).unwrap();
        assert!(matches!(ensure(&Err("a.mp4: no SEI".into())), Made::Now(_)));
        assert!(
            std::fs::read_to_string(&stamp_file)
                .unwrap()
                .ends_with("\nx265: not known\n")
        );
        assert_eq!(ensure(&Err("b.mp4: no SEI".into())), Made::Cached);

        // Its control: a pair stamped by its spec alone, as hw-test stamped
        // the pairs before the stamp held the layout and the x265 (as on
        // odin), isn't taken as made by them.
        let Source::Pair(spec) = &pair else {
            unreachable!()
        };
        std::fs::write(&stamp_file, format!("{spec:#?}\n")).unwrap();
        must_fail(
            "a pair stamped by its spec alone taken as made",
            ensure(&x35) == Made::Cached,
        );
    }

    #[test]
    fn a_pair_is_kept_when_this_runs_x265_cant_be_read() {
        let d = Dir(std::env::temp_dir().join(format!(
            "spherecut-xtask-perf-unread-{}",
            std::process::id()
        )));
        let _ = std::fs::remove_dir_all(&d.0);
        let pair = pair_of("pair");
        let ensure = |x265: &Result<X265Version, String>| {
            ensure_sources(&d.0, std::slice::from_ref(&pair), x265).remove(0)
        };
        let x35 = coder(199, "3.5+1-f0c1022b6");
        let made = ensure(&x35);
        assert!(matches!(made.made, Made::Now(_)), "{made:?}");
        assert_eq!(made.unchecked, None);
        let stamp_file = stamp_path(&made.path);
        let (file, stamp) = (
            std::fs::read(&made.path).unwrap(),
            std::fs::read_to_string(&stamp_file).unwrap(),
        );
        // A run whose x265 can't be read (no libx265, or `hw-media`
        // unwritable) keeps the pair, file and stamp as they were, and
        // says its coder wasn't checked.
        let unread = ensure(&Err("no libx265".into()));
        assert_eq!(unread.made, Made::Cached, "{unread:?}");
        assert_eq!(
            unread.unchecked.as_deref(),
            Some("this run's x265 wasn't read: no libx265")
        );
        assert_eq!(std::fs::read(&made.path).unwrap(), file);
        assert_eq!(std::fs::read_to_string(&stamp_file).unwrap(), stamp);
        assert_eq!(unread.lenses, made.lenses);
        // A run that reads the same x265 again takes it as made, checked.
        let again = ensure(&x35);
        assert_eq!((again.made, again.unchecked), (Made::Cached, None));

        // Its control: the pair of another code layout, its x265 line
        // kept, isn't kept by a run that can't read its x265.
        let other = stamp.replace(
            &format!("corner codes: format {}\n", code::FORMAT),
            "corner codes: format 0\n",
        );
        assert_ne!(other, stamp);
        std::fs::write(&stamp_file, other).unwrap();
        let remade = ensure(&Err("no SEI".into()));
        must_fail(
            "a pair of another code layout kept while this run's x265 isn't read",
            remade.made == Made::Cached,
        );
        assert!(matches!(remade.made, Made::Now(_)), "{remade:?}");
    }
}
