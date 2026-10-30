//! The viewer on lavapipe (M1 plan sections 7.3, 7.4 and 7.7): one
//! pixel per pixel at fractional scales, input rounded as the paint
//! callback rounds, the marker drag, zoom about the pointer, resizes, and
//! a projection that egui-wgpu recorded but never painted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use egui::epaint::ViewportInPixels;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use spherecut_app::viewer::{ViewerCallback, ViewerResources};
use spherecut_render::{Pixels, StillOptions, render_still};

/// The viewer's rect in the rendered image, as `prepare` rounds it.
fn viewport(
    h: &egui_kittest::Harness<'_, spherecut_app::SpherecutApp>,
    w: u32,
    ht: u32,
) -> ViewportInPixels {
    let rect = h
        .state()
        .state
        .viewer_rect
        .expect("the viewer was laid out");
    ViewportInPixels::from_points(&rect, h.ctx.pixels_per_point(), [w, ht])
}

#[test]
fn the_viewer_is_render_still_byte_for_byte_at_fractional_scales() {
    let gpu = common::gpu();
    for ppp in [1.0f32, 1.25, 1.5, 2.0] {
        let rs = common::render_state(&gpu);
        let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], ppp);
        common::until_shown(&mut h, &rs);
        let image = h.render().unwrap();
        let key = h.state().state.shown.clone().unwrap();
        let vp = viewport(&h, image.width(), image.height());
        let (w, ht) = (vp.width_px as u32, vp.height_px as u32);
        let player = h.state().state.player.as_ref().unwrap();
        let frame = player
            .cache()
            .get(0, key.frame.time)
            .expect("the frame shown is in the cache");
        let still = render_still(
            &gpu,
            &*frame,
            &key.view,
            (w, ht),
            StillOptions {
                filter: key.filter,
                ..StillOptions::default()
            },
        )
        .unwrap();
        let Pixels::Rgb8(want) = &still.pixels else {
            panic!("an 8-bit still")
        };
        let raw = image.as_raw();
        let mut differ = 0usize;
        let mut first = None;
        for y in 0..ht {
            for x in 0..w {
                let i =
                    (((vp.top_px as u32 + y) * image.width() + vp.left_px as u32 + x) * 4) as usize;
                let j = ((y * w + x) * 3) as usize;
                if raw[i..i + 3] != want[j..j + 3] {
                    differ += 1;
                    first.get_or_insert((
                        x,
                        y,
                        [raw[i], raw[i + 1], raw[i + 2]],
                        [want[j], want[j + 1], want[j + 2]],
                    ));
                }
            }
        }
        assert_eq!(
            differ, 0,
            "at {ppp} pixels a point, {w}x{ht} at ({}, {}): {differ} pixels differ, the first {first:?}",
            vp.left_px, vp.top_px
        );
    }
}

/// The centroid, in image pixels, of the red F marker's pixels inside the
/// viewer: the disc's red, without its letter.
fn red_centroid(image: &image::RgbaImage, vp: &ViewportInPixels) -> egui::Pos2 {
    let raw = image.as_raw();
    let (mut sx, mut sy, mut n) = (0f64, 0f64, 0f64);
    for y in vp.top_px..vp.top_px + vp.height_px {
        for x in vp.left_px..vp.left_px + vp.width_px {
            let i = ((y as u32 * image.width() + x as u32) * 4) as usize;
            if raw[i] >= 200 && raw[i + 1] <= 60 && raw[i + 2] <= 60 {
                sx += f64::from(x) + 0.5;
                sy += f64::from(y) + 0.5;
                n += 1.0;
            }
        }
    }
    assert!(n > 100.0, "the F marker is in view ({n} red pixels)");
    egui::pos2((sx / n) as f32, (sy / n) as f32)
}

fn angle(a: glam::DVec3, b: glam::DVec3) -> f64 {
    a.normalize()
        .cross(b.normalize())
        .length()
        .atan2(a.normalize().dot(b.normalize()))
}

#[test]
fn input_maps_through_the_rounding_prepare_uses() {
    let gpu = common::gpu();
    for (ppp, size) in [
        (1.25f32, [640.4f32, 420.6f32]),
        (1.5, [633.3, 401.1]),
        (1.25, [517.0, 333.0]),
    ] {
        let rs = common::render_state(&gpu);
        let mut h = common::harness(common::grid_app(&gpu), &rs, size, ppp);
        h.step();
        let image = h.render().unwrap();
        let rect = h.state().state.viewer_rect.unwrap();
        let ui_side = spherecut_app::viewer::viewport(rect, &h.ctx);
        let prepare_side = viewport(&h, image.width(), image.height());
        assert_eq!(
            (
                ui_side.left_px,
                ui_side.top_px,
                ui_side.width_px,
                ui_side.height_px
            ),
            (
                prepare_side.left_px,
                prepare_side.top_px,
                prepare_side.width_px,
                prepare_side.height_px
            ),
            "{ppp} {size:?}"
        );
        // And the target prepare made is the size input maps through.
        assert_eq!(
            common::target_extent(&rs),
            Some(spherecut_app::viewer::physical(&ui_side)),
            "{ppp} {size:?}"
        );
        // The rect's corners are whole pixels, so nothing is rounded.
        for v in [rect.left(), rect.top(), rect.right(), rect.bottom()] {
            let px = v * ppp;
            assert!((px - px.round()).abs() < 1e-3, "{v} at {ppp}");
        }
        // A pointer on pixel (i, j)'s centre is the lens point the shader
        // gives that pixel.
        let px = spherecut_app::viewer::physical(&prepare_side);
        for (i, j) in [(0u32, 0u32), (px.0 - 1, px.1 - 1), (px.0 / 3, px.1 / 2)] {
            let pos = egui::pos2(
                (prepare_side.left_px as f32 + i as f32 + 0.5) / ppp,
                (prepare_side.top_px as f32 + j as f32 + 0.5) / ppp,
            );
            let got = spherecut_app::viewer::lens_point(pos, &ui_side, ppp);
            let want = spherecut_sphere::pixel::to_lens(px, f64::from(i) + 0.5, f64::from(j) + 0.5);
            assert!(
                (got - want).length() < 1e-5,
                "{got} {want} at ({i}, {j}), {ppp}"
            );
        }
    }
}

/// Press at `from`, move to `to` in `n` moves, and let go.
fn drag(
    h: &mut egui_kittest::Harness<'_, spherecut_app::SpherecutApp>,
    from: egui::Pos2,
    to: egui::Pos2,
    n: u32,
) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for k in 1..=n {
        h.hover_at(from + (to - from) * (k as f32 / n as f32));
        h.step();
    }
    h.drop_at(to);
    h.step();
}

/// The marker drag of section 7.4, through the fisheye: an equidistant
/// lens moves a disc's image nearly rigidly, where rectilinear's
/// stretch away from the centre moves the 8° disc's centroid 0.7 px off
/// the grabbed point over this drag at 1.0 points a pixel (measured),
/// though the grabbed point itself lands exactly (the level solve).
#[test]
fn a_drag_puts_the_marker_under_the_pointer() {
    let gpu = common::gpu();
    for ppp in [1.0f32, 1.25] {
        let rs = common::render_state(&gpu);
        let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], ppp);
        h.key_press(egui::Key::Num2);
        for _ in 0..20 {
            h.step();
        }
        assert_eq!(
            h.state().state.view.preset,
            spherecut_app::view::Preset::Fisheye
        );
        common::until_shown(&mut h, &rs);
        let image = h.render().unwrap();
        let vp = viewport(&h, image.width(), image.height());
        let c0 = red_centroid(&image, &vp);
        let from = egui::pos2(c0.x / ppp, c0.y / ppp);
        let to = from + egui::vec2(30.0, 18.0);
        let (q0, lens) = (h.state().state.view.q, h.state().state.view.lens);
        drag(&mut h, from, to, 6);
        common::until_shown(&mut h, &rs);
        let image = h.render().unwrap();
        let c1 = red_centroid(&image, &vp);
        let miss = (c1 - egui::pos2(to.x * ppp, to.y * ppp)).length();
        eprintln!(
            "at {ppp} points a pixel, the centroid lands {miss:.3} px from the release point"
        );
        assert!(
            miss <= 0.5,
            "at {ppp}: the marker's centroid is {miss} px from the release point"
        );
        // The app's orientation is the level solve's.
        let lp = |p| spherecut_app::viewer::lens_point(p, &vp, ppp);
        let start = spherecut_sphere::grab::start(q0, &lens, lp(from));
        let (want, fit) = spherecut_sphere::grab::solve_level(&start, &lens, lp(to));
        assert_eq!(fit, spherecut_sphere::grab::GrabFit::Exact);
        let q = h.state().state.view.q;
        assert!(want.angle_between(q) < 1e-9, "{want} {q}");
        assert!(h.state().state.drag.is_none());
    }
}

#[test]
fn zoom_keeps_the_direction_under_the_pointer() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.25);
    common::until_shown(&mut h, &rs);
    let image = h.render().unwrap();
    let vp = viewport(&h, image.width(), image.height());
    let rect = h.state().state.viewer_rect.unwrap();
    let at = rect.center() + egui::vec2(120.0, -60.0);
    let lp = spherecut_app::viewer::lens_point(at, &vp, 1.25);
    let dir = |a: &spherecut_app::SpherecutApp| a.state.view.q * a.state.view.lens.unproject(lp);
    let before = dir(h.state());
    let hfov0 = h.state().state.view.lens.hfov;
    h.hover_at(at);
    h.step();
    // A wheel's scroll up, then a pinch out (Ctrl+scroll arrives so too).
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..40 {
        h.step();
    }
    let hfov1 = h.state().state.view.lens.hfov;
    assert!(hfov1 < hfov0 * 0.9, "{hfov0} → {hfov1}");
    assert!(angle(before, dir(h.state())) < 1e-9);
    h.event(egui::Event::Zoom(0.5));
    h.step();
    let hfov2 = h.state().state.view.lens.hfov;
    assert!(
        (hfov2 - (hfov1 * 2.0).min(170f64.to_radians())).abs() < 1e-9,
        "{hfov1} → {hfov2}"
    );
    assert!(angle(before, dir(h.state())) < 1e-9);
}

#[test]
fn the_projection_switch_morphs_over_a_quarter_second() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.0);
    common::until_shown(&mut h, &rs);
    let projected = common::draws(&rs).0;
    h.key_press(egui::Key::Num3);
    h.step();
    let mut morphs = Vec::new();
    for _ in 0..30 {
        h.step();
        let l = h.state().state.view.lens;
        morphs.push(l.morph);
    }
    let view = &h.state().state.view;
    assert!(!view.animating());
    assert_eq!(view.preset, spherecut_app::view::Preset::TinyPlanet);
    assert_eq!(
        view.lens,
        spherecut_sphere::perspective::Lens::new(
            spherecut_sphere::perspective::Projection::Stereographic,
            300f64.to_radians()
        )
    );
    assert!((view.ypr().pitch + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    assert!(morphs.iter().any(|&m| m > 0.1 && m < 0.9), "{morphs:?}");
    // Every frame of the morph was projected anew.
    assert!(common::draws(&rs).0 >= projected + 10);
}

#[test]
fn an_unchanged_frame_and_view_only_encode() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.0);
    common::until_shown(&mut h, &rs);
    let (p0, e0) = common::draws(&rs);
    for _ in 0..10 {
        h.step();
    }
    let (p1, e1) = common::draws(&rs);
    assert_eq!(p1, p0, "nothing changed, so nothing was projected");
    assert!(e1 >= e0 + 10);
    // A resize projects again.
    h.set_size(egui::vec2(600.0, 400.0));
    h.step();
    assert_eq!(common::draws(&rs).0, p1 + 1);
}

/// egui-wgpu runs the callbacks' `prepare` before it acquires the
/// window's surface, and drops what `prepare` recorded when that fails
/// (Outdated, Lost, Timeout, Occluded): `paint` doesn't follow. Such a
/// projection is made again, and only one that was painted is encoded
/// alone.
#[test]
fn a_projection_never_painted_is_projected_again() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let size = [640u32, 420u32];
    let mut h = common::harness(
        common::grid_app(&gpu),
        &rs,
        [size[0] as f32, size[1] as f32],
        1.0,
    );
    common::until_shown(&mut h, &rs);
    let key = h.state().state.shown.clone().unwrap();
    let rect = h.state().state.viewer_rect.unwrap();
    let presented = h
        .state()
        .state
        .player
        .as_ref()
        .unwrap()
        .present(std::time::Instant::now())
        .expect("a frame on screen");
    let frame = presented.frames.first().unwrap();
    assert_eq!((frame.slot, frame.frame), (key.slot, key.frame));
    let cb = ViewerCallback {
        rect,
        shown: Some((frame.gpu.clone(), key.clone())),
        perf: None,
    };
    let mut res = CallbackResources::new();
    res.insert(ViewerResources::new(&gpu, rs.target_format));
    let sd = ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    };
    let prepare = |res: &mut CallbackResources| {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let extra = cb.prepare(&gpu.device, &gpu.queue, &sd, &mut enc, res);
        assert!(extra.is_empty());
        gpu.queue.submit([enc.finish()]);
    };
    let draws = |res: &CallbackResources| {
        let v = res.get::<ViewerResources>().unwrap();
        (v.projected, v.encoded_only)
    };

    prepare(&mut res);
    assert_eq!(draws(&res), (1, 0));
    assert_eq!(res.get::<ViewerResources>().unwrap().last(), None);
    // The surface wasn't acquired: no paint. The same frame and view are
    // projected again.
    prepare(&mut res);
    assert_eq!(draws(&res), (2, 0));

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("a window's surface"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: rs.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = enc
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui's pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        let info = egui::PaintCallbackInfo {
            viewport: rect,
            clip_rect: rect,
            pixels_per_point: 1.0,
            screen_size_px: size,
        };
        cb.paint(info, &mut pass, &res);
    }
    gpu.queue.submit([enc.finish()]);
    assert_eq!(res.get::<ViewerResources>().unwrap().last(), Some(&key));
    // Painted: the next draw of it only encodes.
    prepare(&mut res);
    assert_eq!(draws(&res), (2, 1));
    drop(presented);
}

#[test]
fn steps_show_the_next_and_previous_frames() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.0);
    common::until_shown(&mut h, &rs);
    let t = |h: &egui_kittest::Harness<'_, spherecut_app::SpherecutApp>| {
        h.state().state.shown.as_ref().map(|k| k.frame.time)
    };
    let frame = |n: i64| Some(spherecut_core::FrameRate::FPS_30.frame_to_time(n).unwrap());
    assert_eq!(t(&h), frame(0));
    for (key, want) in [
        (egui::Key::ArrowRight, 1),
        (egui::Key::Period, 2),
        (egui::Key::ArrowLeft, 1),
        (egui::Key::End, 11),
        (egui::Key::Home, 0),
    ] {
        h.key_press(key);
        common::step_until(
            &mut h,
            std::time::Duration::from_secs(10),
            &format!("{key:?}"),
            |a| a.state.shown.as_ref().map(|k| k.frame.time) == frame(want),
        );
    }
}

#[test]
fn a_resize_sweep_never_panics() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.0);
    common::until_shown(&mut h, &rs);
    for size in [
        [1.0f32, 1.0],
        [64.0, 48.0],
        [3.0, 700.0],
        [700.0, 3.0],
        [481.0, 321.0],
        [1280.0, 720.0],
        [1920.0, 1080.0],
        [3840.0, 2160.0],
        [2.0, 2.0],
    ] {
        h.set_size(egui::Vec2::from(size));
        h.step();
        h.step();
    }
}

/// While the window is minimized or occluded, eframe runs only the app's
/// `logic`: the presenter goes on with it, frame by frame, and ends the
/// clip. Without it, nothing ends the clip.
#[test]
fn playback_goes_on_while_the_window_is_hidden() {
    use std::time::{Duration, Instant};
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(common::grid_app(&gpu), &rs, [640.0, 420.0], 1.0);
    common::until_shown(&mut h, &rs);
    let ctx = h.ctx.clone();
    let snapshot = |h: &egui_kittest::Harness<'_, spherecut_app::SpherecutApp>| {
        h.state().state.player.as_ref().unwrap().snapshot()
    };
    // Hidden from here: logic alone, at eframe's 10 ms a pass.
    let hidden =
        |h: &mut egui_kittest::Harness<'_, spherecut_app::SpherecutApp>,
         done: &dyn Fn(&egui_kittest::Harness<'_, spherecut_app::SpherecutApp>) -> bool| {
            let start = Instant::now();
            let mut times = std::collections::BTreeSet::new();
            while !done(h) {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "not done: {:?}",
                    snapshot(h)
                );
                h.state_mut().logic(&ctx);
                if let Some(p) = &h.state().state.presented {
                    times.extend(p.frames.iter().map(|f| f.frame.time));
                }
                common::pace(Duration::from_millis(10));
            }
            times
        };
    h.state_mut().state.toggle_play();
    let times = hidden(&mut h, &|h| snapshot(h).at_end);
    assert_eq!(snapshot(&h).state, spherecut_playback::PlayState::Paused);
    assert!(times.len() >= 2, "frames presented while hidden: {times:?}");
    // Shown again, as eframe runs a pass (logic, then the UI): the app is
    // where logic left it.
    h.state_mut().logic(&ctx);
    h.step();
    let s = &h.state().state;
    assert!(s.snapshot.at_end);
    assert_eq!(
        s.shown.as_ref().map(|k| k.frame.time),
        times.last().copied()
    );

    // The control: back to the start, and played with no passes at all.
    // The clock runs past the 0.4 s clip's end, and nothing ends it.
    h.state_mut().state.seek(
        spherecut_core::Time::ZERO,
        spherecut_media::backend::SeekMode::Exact,
    );
    hidden(&mut h, &|h| {
        let s = snapshot(h);
        !s.at_end && s.state == spherecut_playback::PlayState::Paused
    });
    h.state_mut().state.toggle_play();
    common::pace(Duration::from_secs(1));
    assert!(!snapshot(&h).at_end);
    h.state_mut().logic(&ctx);
    assert!(snapshot(&h).at_end);
}
