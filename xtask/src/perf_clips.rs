//! The perf clips hw-test makes for the playback benchmarks (M1 plan
//! section 12.4, DECISIONS D37): synth's [`media::perf_clips`], made once
//! into `<target>/hw-media/` and kept across runs, since x265 takes
//! minutes over each. A clip is made again when synth's spec of it
//! changes: the spec it was made from is kept beside it, in
//! `<file>.spec`. A clip is written in `.partial/` and moved into place
//! only once whole, so an interrupted run leaves nothing to reuse.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spherecut_synth::media::{self, Spec};

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
}

impl Clip {
    /// The average bitrate in Mbit/s, from the file's size.
    pub(crate) fn mbps(&self) -> Option<f64> {
        (self.bytes > 0 && self.seconds > 0.0).then(|| self.bytes as f64 * 8.0 / self.seconds / 1e6)
    }
}

/// What `spec` is stamped with: everything it says, so that any change
/// to it makes the clip again.
fn stamp(spec: &Spec) -> String {
    format!("{spec:#?}\n")
}

fn stamp_path(clip: &Path) -> PathBuf {
    let mut p = clip.as_os_str().to_owned();
    p.push(".spec");
    PathBuf::from(p)
}

/// Every perf clip in `dir`, made there unless an earlier run made it
/// from the same spec.
pub(crate) fn ensure(dir: &Path) -> Vec<Clip> {
    ensure_specs(dir, &media::perf_clips())
}

/// [`ensure`] for `specs`.
pub(crate) fn ensure_specs(dir: &Path, specs: &[Spec]) -> Vec<Clip> {
    specs.iter().map(|spec| ensure_one(dir, spec)).collect()
}

fn ensure_one(dir: &Path, spec: &Spec) -> Clip {
    let path = dir.join(spec.file_name());
    let stamp_file = stamp_path(&path);
    let want = stamp(spec);
    let seconds = f64::from(spec.frames) / spec.rate.to_f64();
    let clip = |made| Clip {
        name: spec.name.clone(),
        bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
        path: path.clone(),
        made,
        seconds,
    };
    let fresh = path.is_file() && std::fs::read_to_string(&stamp_file).ok().as_ref() == Some(&want);
    if fresh {
        return clip(Made::Cached);
    }
    eprintln!(
        "==> making perf clip {} (once: it is kept in {})",
        spec.file_name(),
        dir.display()
    );
    let start = Instant::now();
    match make(dir, spec, &path, &stamp_file, &want) {
        Ok(()) => clip(Made::Now(start.elapsed())),
        Err(why) => clip(Made::Failed(why)),
    }
}

/// Write the clip in `dir/.partial/`, then move it and its stamp into
/// place.
fn make(
    dir: &Path,
    spec: &Spec,
    path: &Path,
    stamp_file: &Path,
    stamp: &str,
) -> Result<(), String> {
    let _ = std::fs::remove_file(stamp_file);
    let _ = std::fs::remove_file(path);
    let partial = dir.join(".partial");
    let written = media::write(spec, &partial).map_err(|e| e.to_string());
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
    }
}
