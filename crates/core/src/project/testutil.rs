//! Projects for tests.

use std::path::PathBuf;

use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::Projection;

use super::{Fingerprint, InitialView, Media, MediaId, MediaVideo, Orientation};
use crate::time::{FrameRate, Time};

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

/// Looking 20° right and 5° down, rectilinear at 90°.
pub fn view() -> InitialView {
    InitialView {
        orientation: Orientation::from_ypr(Ypr::from_degrees(20.0, -5.0, 0.0)).unwrap(),
        fov_deg: 90.0,
        projection: Projection::Rectilinear,
    }
}
