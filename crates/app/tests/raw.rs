//! The raw view (M1 plan section 7.8), through the UI: a camera's raw
//! file, two video streams of one size, opens on its best stream; once
//! the Info pane's probe has found the lenses, it opens again with both,
//! and the viewer shows them side by side and flat, each in its column
//! where `raw_place` puts it, pixel for pixel the flat still of its frame,
//! black around them. A drag there leaves the view alone, and a notice
//! says why the pictures aren't stitched. The grid equirect opened after
//! is projected again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use egui::epaint::ViewportInPixels;
use egui_kittest::Harness;
use spherecut_app::SpherecutApp;
use spherecut_app::notices::NoticeKind;
use spherecut_app::viewer::{self, RawShown, ViewerResources};
use spherecut_media::backend::FfmpegBackend;
use spherecut_render::{Pixels, StillOptions, ViewParams, render_still};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Codec, Spec};

/// Each lens's picture: square, as a dual-fisheye camera's are.
const LENS: (u32, u32) = (384, 384);

/// Two lenses, the grid and the seam pattern, in one Matroska file.
fn lenses_clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        let lens = |name: &str, pattern| {
            media::write(
                &Spec {
                    name: name.into(),
                    pattern,
                    width: LENS.0,
                    height: LENS.1,
                    frames: 12,
                    codec: Codec::H264Lossless,
                    ..Spec::default()
                },
                common::tmp(),
            )
            .unwrap()
        };
        let a = lens("lens-front", Pattern::Grid);
        let b = lens("lens-back", Pattern::Seam);
        spherecut_media::encode::mux_video_streams(&[&a, &b], &common::tmp().join("lenses.mkv"))
            .unwrap()
    })
}

/// What each raw view column shows.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's read of the renderer lock, between frames"
)]
fn raw_last(rs: &egui_wgpu::RenderState) -> Vec<Option<RawShown>> {
    let r = rs.renderer.read();
    r.callback_resources
        .get::<ViewerResources>()
        .map_or_else(Vec::new, ViewerResources::raw_last)
}

fn viewport(h: &Harness<'_, SpherecutApp>, w: u32, ht: u32) -> ViewportInPixels {
    let rect = h
        .state()
        .state
        .viewer_rect
        .expect("the viewer was laid out");
    ViewportInPixels::from_points(&rect, h.ctx.pixels_per_point(), [w, ht])
}

#[test]
fn a_cameras_lenses_show_side_by_side_and_flat() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![lenses_clip().to_path_buf()];
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [1000.0, 640.0], 1.25);

    common::step_until(&mut h, Duration::from_secs(20), "both lenses open", |a| {
        a.state.snapshot.streams.len() == 2
    });
    assert_eq!(h.state().state.columns(), 2);
    assert!(h.state().state.raw_view());
    let notice = h
        .state()
        .state
        .notices
        .get(&NoticeKind::Lenses)
        .expect("the lenses' notice");
    assert!(
        notice.what.contains("2 lenses side by side"),
        "{}",
        notice.what
    );
    common::step_until(
        &mut h,
        Duration::from_secs(20),
        "both columns drawn",
        |_| {
            let last = raw_last(&rs);
            last.len() == 2 && last.iter().all(Option::is_some)
        },
    );
    // A pass more, so the notice's panel has settled the viewer's size.
    for _ in 0..3 {
        h.step();
    }

    let image = h.render().unwrap();
    let vp = viewport(&h, image.width(), image.height());
    let size = viewer::physical(&vp);
    let shown: Vec<RawShown> = raw_last(&rs).into_iter().map(Option::unwrap).collect();
    let player = h.state().state.player.as_ref().unwrap();
    let view = ViewParams::new(h.state().state.view.q, h.state().state.view.lens);
    let raw = image.as_raw();
    let at = |x: u32, y: u32| {
        let i = (((vp.top_px as u32 + y) * image.width() + vp.left_px as u32 + x) * 4) as usize;
        [raw[i], raw[i + 1], raw[i + 2]]
    };
    for (column, s) in shown.iter().enumerate() {
        assert_eq!(
            (s.origin, s.size),
            viewer::raw_place(size, 2, column, LENS),
            "column {column} in a {size:?} viewer"
        );
        let frame = player
            .cache()
            .get(column, s.key.frame.time)
            .expect("the frame shown is in the cache");
        let still = render_still(
            &gpu,
            &*frame,
            &view,
            s.size,
            StillOptions {
                filter: s.key.filter,
                flat: true,
                ..StillOptions::default()
            },
        )
        .unwrap();
        let Pixels::Rgb8(want) = &still.pixels else {
            panic!("an 8-bit still")
        };
        let (w, ht) = s.size;
        let mut differ = 0usize;
        let mut first = None;
        for y in 0..ht {
            for x in 0..w {
                let got = at(s.origin[0] + x, s.origin[1] + y);
                let j = ((y * w + x) * 3) as usize;
                if got[..] != want[j..j + 3] {
                    differ += 1;
                    first.get_or_insert((x, y, got, [want[j], want[j + 1], want[j + 2]]));
                }
            }
        }
        assert_eq!(
            differ, 0,
            "column {column}, {w}x{ht} at {:?}: {differ} pixels differ, the first {first:?}",
            s.origin
        );
    }
    // The two pictures differ, and the rest of the viewer is black.
    assert_ne!(
        at(shown[0].origin[0] + 5, shown[0].origin[1] + 5),
        at(shown[1].origin[0] + 5, shown[1].origin[1] + 5)
    );
    let inside = |s: &RawShown, x: u32, y: u32| {
        (s.origin[0]..s.origin[0] + s.size.0).contains(&x)
            && (s.origin[1]..s.origin[1] + s.size.1).contains(&y)
    };
    let mut lit = 0usize;
    let mut outside = 0usize;
    for y in 0..size.1 {
        for x in 0..size.0 {
            if shown.iter().any(|s| inside(s, x, y)) {
                continue;
            }
            outside += 1;
            if at(x, y) != [0, 0, 0] {
                lit += 1;
            }
        }
    }
    assert!(outside > 0, "the lenses leave the viewer's sides");
    assert_eq!(
        lit, 0,
        "{lit} of the {outside} pixels around the pictures aren't black"
    );

    // A drag leaves the view as it was.
    let q0 = h.state().state.view.q;
    let rect = h.state().state.viewer_rect.unwrap();
    let from = rect.center();
    h.drag_at(from);
    h.step();
    for k in 1..=4 {
        h.hover_at(from + egui::vec2(20.0 * k as f32, 10.0 * k as f32));
        h.step();
    }
    h.drop_at(from + egui::vec2(80.0, 40.0));
    h.step();
    assert_eq!(h.state().state.view.q, q0);
    assert!(h.state().state.drag.is_none());

    // The grid equirect: projected, the columns gone, the notice too.
    h.state_mut().state.open(common::grid_clip().to_path_buf());
    common::until_shown(&mut h, &rs);
    let s = &h.state().state;
    assert_eq!(s.columns(), 1);
    assert!(!s.raw_view());
    assert!(s.notices.get(&NoticeKind::Lenses).is_none());
    assert!(raw_last(&rs).is_empty());
}
