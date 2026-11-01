//! The perf clips hw-test makes for the playback benchmarks (M1 plan
//! section 12.4, DECISIONS D37): synth's [`media::perf_clips`], and from
//! M3.1 its two-track [`media::perf_pairs`], made once into
//! `<target>/hw-media/` and kept across runs, since x265 takes minutes
//! over each. A clip is made again when synth's spec of it changes: the
//! spec it was made from is kept beside it, in `<file>.spec`. A clip is
//! written in `.partial/` and moved into place only once whole, so an
//! interrupted run leaves nothing to reuse.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spherecut_synth::media::{self, PairSpec, Spec};

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
    /// For a pair, the bytes of each lens's samples, lens 0 first, so
    /// that each lens is rated on its own; empty for a clip of one
    /// stream, or a pair that isn't there.
    pub track_bytes: Vec<u64>,
}

impl Clip {
    /// The average bitrate in Mbit/s, from the file's size.
    pub(crate) fn mbps(&self) -> Option<f64> {
        rate(self.bytes, self.seconds)
    }

    /// The average bitrate of a pair's lens `k` in Mbit/s, from its
    /// samples.
    pub(crate) fn track_mbps(&self, k: usize) -> Option<f64> {
        rate(*self.track_bytes.get(k)?, self.seconds)
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

    /// Its length: a pair's lens 0's.
    fn seconds(&self) -> f64 {
        let spec = match self {
            Source::One(s) => s,
            Source::Pair(p) => &p.lenses[0],
        };
        f64::from(spec.frames) / spec.rate.to_f64()
    }

    /// What it is stamped with: everything it says, so that any change
    /// to it makes the clip again. A clip of one stream is stamped as it
    /// was before pairs, so that M2's clips aren't made again.
    fn stamp(&self) -> String {
        match self {
            Source::One(s) => format!("{s:#?}\n"),
            Source::Pair(p) => format!("{p:#?}\n"),
        }
    }

    fn write(&self, dir: &Path) -> Result<PathBuf, String> {
        match self {
            Source::One(s) => media::write(s, dir),
            Source::Pair(p) => media::write_pair(p, dir),
        }
        .map_err(|e| e.to_string())
    }

    /// The bytes of each lens's samples of the pair at `path`, or none
    /// for a clip of one stream.
    fn track_bytes(&self, path: &Path) -> Vec<u64> {
        match self {
            Source::One(_) => Vec::new(),
            Source::Pair(_) if !path.is_file() => Vec::new(),
            Source::Pair(_) => (0..2)
                .map(|k| media::track_bytes(path, k).unwrap_or(0))
                .collect(),
        }
    }
}

fn stamp_path(clip: &Path) -> PathBuf {
    let mut p = clip.as_os_str().to_owned();
    p.push(".spec");
    PathBuf::from(p)
}

/// Every perf clip in `dir`, made there unless an earlier run made it
/// from the same spec: synth's clips of one stream, then its pairs.
pub(crate) fn ensure(dir: &Path) -> Vec<Clip> {
    let sources: Vec<Source> = media::perf_clips()
        .into_iter()
        .map(|s| Source::One(Box::new(s)))
        .chain(
            media::perf_pairs()
                .into_iter()
                .map(|p| Source::Pair(Box::new(p))),
        )
        .collect();
    ensure_sources(dir, &sources)
}

/// [`ensure`] for `specs`, clips of one stream.
#[cfg(test)]
pub(crate) fn ensure_specs(dir: &Path, specs: &[Spec]) -> Vec<Clip> {
    let sources: Vec<Source> = specs
        .iter()
        .map(|s| Source::One(Box::new(s.clone())))
        .collect();
    ensure_sources(dir, &sources)
}

/// [`ensure`] for `sources`.
pub(crate) fn ensure_sources(dir: &Path, sources: &[Source]) -> Vec<Clip> {
    sources
        .iter()
        .map(|source| ensure_one(dir, source))
        .collect()
}

fn ensure_one(dir: &Path, source: &Source) -> Clip {
    let path = dir.join(source.file_name());
    let stamp_file = stamp_path(&path);
    let want = source.stamp();
    let seconds = source.seconds();
    let clip = |made| Clip {
        name: source.name().to_string(),
        bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
        path: path.clone(),
        made,
        seconds,
        track_bytes: source.track_bytes(&path),
    };
    let fresh = path.is_file() && std::fs::read_to_string(&stamp_file).ok().as_ref() == Some(&want);
    if fresh {
        return clip(Made::Cached);
    }
    eprintln!(
        "==> making perf clip {} (once: it is kept in {})",
        source.file_name(),
        dir.display()
    );
    let start = Instant::now();
    match make(dir, source, &path, &stamp_file, &want) {
        Ok(()) => clip(Made::Now(start.elapsed())),
        Err(why) => clip(Made::Failed(why)),
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

    #[test]
    fn a_pair_is_made_once_and_rated_a_lens_at_a_time() {
        let d =
            Dir(std::env::temp_dir()
                .join(format!("spherecut-xtask-perf-pair-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        let pair = Source::Pair(Box::new(media::pair_64(
            "pair",
            [media::LensTrack {
                frames: 4,
                keyint: 2,
            }; 2],
        )));
        let first = ensure_sources(&d.0, std::slice::from_ref(&pair));
        assert!(matches!(first[0].made, Made::Now(_)), "{first:?}");
        assert_eq!(first[0].track_bytes.len(), 2);
        assert!(first[0].track_bytes.iter().all(|&b| b > 0), "{first:?}");
        // The lenses' samples are less than the file, which holds its index
        // too.
        assert!(first[0].track_bytes.iter().sum::<u64>() < first[0].bytes);
        assert!(first[0].track_mbps(1).unwrap() > 0.0);
        assert_eq!(first[0].track_mbps(2), None);
        let again = ensure_sources(&d.0, &[pair]);
        assert_eq!(again[0].made, Made::Cached);
        assert_eq!(again[0].track_bytes, first[0].track_bytes);
        // A clip of one stream has no lenses to rate.
        let one = ensure_specs(&d.0, &[tiny("one", 2)]);
        assert!(one[0].track_bytes.is_empty());
        // Its stamp is the spec's alone, as before pairs.
        assert_eq!(
            std::fs::read_to_string(stamp_path(&one[0].path)).unwrap(),
            format!("{:#?}\n", tiny("one", 2))
        );
    }
}
