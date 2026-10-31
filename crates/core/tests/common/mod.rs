//! Projects and edits for the project tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use spherecut_core::project::*;
use spherecut_core::{FrameRate, Rational, Time};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{LensShape, Projection};

/// A 3840×1920 30 fps media entry of `seconds` at `path`.
pub fn media(path: &str, seconds: i64) -> Media {
    Media {
        id: MediaId::NEW,
        path: PathBuf::from(path),
        fingerprint: Fingerprint {
            bytes: 1_000_000,
            head: "0123456789abcdef".into(),
        },
        video: MediaVideo {
            stream: 0,
            width: 3840,
            height: 1920,
            rate: FrameRate::FPS_30,
            duration: Time::from_secs(seconds),
        },
    }
}

pub fn ypr(yaw: f64, pitch: f64, roll: f64) -> Orientation {
    Orientation::from_ypr(Ypr::from_degrees(yaw, pitch, roll)).unwrap()
}

/// A new project on a 10 s clip at /clips/VID_1.mp4.
pub fn project() -> Project {
    project_at("/clips/VID_1.mp4")
}

/// A new project on a 10 s clip at `path`.
pub fn project_at(path: &str) -> Project {
    Project::untitled(
        media(path, 10),
        InitialView {
            orientation: ypr(20.0, -5.0, 0.0),
            fov_deg: 90.0,
            projection: Projection::Rectilinear,
        },
    )
    .unwrap()
}

/// Frame `n` at 30 fps.
pub fn frame(n: i64) -> Time {
    Time(Rational::new(n, 30).unwrap())
}

pub fn clip(p: &Project) -> ClipId {
    p.the_clip().id
}

pub fn sequence(p: &Project) -> SequenceId {
    p.the_sequence().id
}

pub fn orientation_key(at: Time, o: Orientation) -> AnyKey {
    AnyKey::Orientation(Key {
        id: KeyId::NEW,
        at,
        value: Orient::View(o),
        ease: Ease::SMOOTH,
        spin: 0,
    })
}

pub fn fov_key(at: Time, fov: f64) -> AnyKey {
    AnyKey::Fov(Key {
        id: KeyId::NEW,
        at,
        value: fov,
        ease: Ease::SMOOTH,
        spin: 0,
    })
}

pub fn projection_key(at: Time, p: Projection) -> AnyKey {
    AnyKey::Projection(Key {
        id: KeyId::NEW,
        at,
        value: LensShape::Pure(p),
        ease: Ease::STOP,
        spin: 0,
    })
}

/// A fixed target in `p`'s clip's media.
pub fn target(p: &Project, name: &str, yaw: f64, pitch: f64) -> Target {
    Target {
        id: TargetId::NEW,
        name: name.into(),
        media: p.the_clip().media,
        path: KeyTrack {
            keys: vec![Key {
                id: KeyId::NEW,
                at: Time::ZERO,
                value: AimDir {
                    yaw_deg: yaw,
                    pitch_deg: pitch,
                },
                ease: Ease::SMOOTH,
                spin: 0,
            }],
        },
    }
}

/// Every id in `p` with what it names.
pub fn ids(p: &Project) -> Vec<(u64, &'static str)> {
    let mut out = Vec::new();
    for m in p.media() {
        out.push((m.id.0, "media"));
    }
    for t in p.targets() {
        out.push((t.id.0, "target"));
        out.extend(t.path.keys.iter().map(|k| (k.id.0, "target key")));
    }
    for s in p.sequences() {
        out.push((s.id.0, "sequence"));
        for t in &s.tracks {
            out.push((t.id.0, "track"));
            for c in &t.clips {
                out.push((c.id.0, "clip"));
                out.extend(c.markers.iter().map(|m| (m.id.0, "marker")));
                let v = &c.view;
                out.extend(
                    v.orientation
                        .keys
                        .iter()
                        .map(|k| (k.id.0, "orientation key")),
                );
                out.extend(v.fov.keys.iter().map(|k| (k.id.0, "fov key")));
                out.extend(v.projection.keys.iter().map(|k| (k.id.0, "projection key")));
            }
        }
    }
    out
}

/// A fresh directory under the system's temporary folder, removed when
/// dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("spherecut-core-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::fmt::Debug for TempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
