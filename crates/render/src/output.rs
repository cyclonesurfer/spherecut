//! What one exported frame renders (M2 plan sections 7.1, 7.2 and 7.4;
//! amendment 2 items 5 and 17): a sequence's output as a size and its
//! views, from the clip's keyed view at the frame's time. The export and
//! the parity reference ([`StillRenderer`](crate::StillRenderer)) both
//! take their views from here, so they render the same frame.

use spherecut_core::project::{Output, Target, ViewAnim};

use crate::blur;
use crate::project::ViewParams;

/// The share of a frame the shutter is open for with motion blur on:
/// 180°, half a frame, centred on the frame's time.
pub const SHUTTER: f64 = 0.5;

/// One exported frame's views: one, or with motion blur the shutter's,
/// which `StillRenderer::render_blurred` and the export average.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameViews {
    /// The output's size, width then height.
    pub size: (u32, u32),
    pub views: Vec<ViewParams>,
    /// The frame needed more than [`blur::MAX_SAMPLES`] views and has
    /// that many (the export's `blur.capped` counts these).
    pub capped: bool,
}

/// The view `output` renders of `anim` at `t`, seconds of the clip's
/// source time: for a flat output, the keyed view, its FOV spanning the
/// output's width (section 7.1); for equirect, the whole sphere turned
/// by the output's orientation, which the keys don't move.
pub fn view_at(anim: &ViewAnim, targets: &[Target], output: &Output, t: f64) -> ViewParams {
    match output {
        Output::Flat { .. } => {
            let v = anim.at(t, targets);
            ViewParams::new(v.orientation, v.lens)
        }
        Output::Equirect { orientation, .. } => ViewParams::equirect(orientation.quat()),
    }
}

/// The views of the frame at `t` that lasts `frame_s` seconds. With
/// `blur`, as many as the frame's movement over the shutter needs
/// ([`blur::sample_count`] of [`blur::displacement`] between the views
/// at the shutter's two ends), at the middles of equal parts of it.
pub fn frame_views(
    anim: &ViewAnim,
    targets: &[Target],
    output: &Output,
    t: f64,
    frame_s: f64,
    blur: bool,
) -> FrameViews {
    let size = output.size();
    let at = |t| view_at(anim, targets, output, t);
    if !blur {
        return FrameViews {
            size,
            views: vec![at(t)],
            capped: false,
        };
    }
    let open = frame_s * SHUTTER;
    let s = blur::displacement(&at(t - open / 2.0), &at(t + open / 2.0), size);
    let (n, capped) = blur::sample_count(s);
    FrameViews {
        size,
        views: blur::sample_times(t, open, n).into_iter().map(at).collect(),
        capped,
    }
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::{
        Aspect, Ease, Key, KeyId, KeyTrack, Orient, OrientMode, Orientation,
    };
    use spherecut_core::time::{Rational, Time};
    use spherecut_sphere::orient::Ypr;
    use spherecut_sphere::perspective::{LensShape, Projection};

    use super::*;
    use crate::project::OutKind;

    const WIDE: Output = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };

    fn orient(yaw_deg: f64) -> Orient {
        Orient::View(Orientation::from_ypr(Ypr::from_degrees(yaw_deg, 0.0, 0.0)).unwrap())
    }

    /// Rectilinear 90°, panning right from yaw 0 at 0 s to `yaw_deg` at 1 s
    /// at a steady rate.
    fn pan(yaw_deg: f64) -> ViewAnim {
        let key = |id, ms, yaw| Key {
            id: KeyId(id),
            at: Time::seconds(Rational::new(ms, 1000).unwrap()),
            value: orient(yaw),
            ease: Ease::LINEAR,
            spin: 0,
        };
        ViewAnim {
            mode: OrientMode::Level,
            orientation: KeyTrack {
                keys: vec![key(1, 0, 0.0), key(2, 1000, yaw_deg)],
            },
            fov: KeyTrack::one(KeyId(3), 90.0, Ease::SMOOTH),
            projection: KeyTrack::one(
                KeyId(4),
                LensShape::Pure(Projection::Rectilinear),
                Ease::SMOOTH,
            ),
        }
    }

    #[test]
    fn a_flat_output_renders_the_keyed_view_and_equirect_the_whole_sphere() {
        let a = pan(90.0);
        let v = view_at(&a, &[], &WIDE, 0.5);
        let keyed = a.at(0.5, &[]);
        assert_eq!((v.orientation, v.lens), (keyed.orientation, keyed.lens));
        assert_eq!((v.out, v.framing), (OutKind::Lens, None));
        let q = Ypr::from_degrees(30.0, 10.0, 0.0).to_quat();
        let sphere = Output::Equirect {
            width: 3840,
            orientation: Orientation::new(q).unwrap(),
        };
        for t in [0.0, 0.5, 1.0] {
            let v = view_at(&a, &[], &sphere, t);
            assert_eq!(v.out, OutKind::Equirect);
            assert!(v.orientation.abs_diff_eq(q, 1e-12));
        }
        let f = frame_views(&a, &[], &sphere, 0.5, 1.0 / 30.0, true);
        assert_eq!((f.size, f.views.len(), f.capped), ((3840, 1920), 1, false));
    }

    #[test]
    fn blur_takes_as_many_views_as_the_frame_moves() {
        let frame_s = 1.0 / 30.0;
        // Off, or a still view: the frame's own view.
        let still = pan(0.0);
        for blur in [false, true] {
            let f = frame_views(&still, &[], &WIDE, 0.5, frame_s, blur);
            assert_eq!(f.size, (1920, 1080));
            assert_eq!(f.views, vec![view_at(&still, &[], &WIDE, 0.5)]);
            assert!(!f.capped);
        }
        let one = frame_views(&pan(90.0), &[], &WIDE, 0.5, frame_s, false);
        assert_eq!(one.views.len(), 1);
        // 90°/s over a 1/60 s shutter is 1.5°: 25 pixels at the middle of
        // a 1920-wide frame and 54 at its corners, so 36 views, spread
        // over the shutter.
        let a = pan(90.0);
        let f = frame_views(&a, &[], &WIDE, 0.5, frame_s, true);
        let open = frame_s * SHUTTER;
        let s = blur::displacement(
            &view_at(&a, &[], &WIDE, 0.5 - open / 2.0),
            &view_at(&a, &[], &WIDE, 0.5 + open / 2.0),
            (1920, 1080),
        );
        let n = f.views.len();
        assert_eq!(n as u32, blur::sample_count(s).0, "{s} pixels");
        assert!((30..=40).contains(&n), "{n} views for {s} pixels");
        assert!(!f.capped);
        for (k, v) in f.views.iter().enumerate() {
            let t = 0.5 - open / 2.0 + (k as f64 + 0.5) * open / n as f64;
            assert_eq!(*v, view_at(&a, &[], &WIDE, t));
        }
        // Twice as fast needs more than 64: capped at 64.
        let f = frame_views(&pan(180.0), &[], &WIDE, 0.5, frame_s, true);
        assert_eq!((f.views.len(), f.capped), (64, true));
        // The views turn the right way: yaw grows over the shutter.
        let yaw = |v: &ViewParams| {
            let d = v.orientation * glam::DVec3::NEG_Z;
            d.x.atan2(-d.z)
        };
        assert!(yaw(&f.views[0]) < yaw(&f.views[63]));
    }
}
