//! The sphere overview (M2 plan section 10.5; amendment 2 item 37): an
//! equirect thumbnail of the frame at the playhead, drawn by the
//! viewer's own projector (no second decode), and over it the view's
//! footprint (the output frame's edge on the sphere), its centre, the
//! centre's path over the trim, each orientation key's centre, the
//! look-at targets, and a 360 output's front. A click turns the view to
//! look there, level: for a flat output as a change of the view
//! (section 10.2), for a 360 output its front.

use glam::{DQuat, DVec2, DVec3};
use spherecut_core::project::{Orientation, Output};
use spherecut_render::ViewParams;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::{LonLat, equirect};

use crate::app::AppState;
use crate::doc::Doc;
use crate::viewer::OverviewCallback;

/// How many points the centre's path is thinned to.
pub const PATH_POINTS: usize = 600;
/// Points along each side of the footprint.
const EDGE_POINTS: usize = 32;

/// The overview's own state.
#[derive(Debug, Default)]
pub struct Overview {
    /// The thumbnail's rect at the last draw, in points.
    pub rect: Option<egui::Rect>,
}

/// Where direction `d` is on a thumbnail in `rect`.
pub fn to_thumb(rect: egui::Rect, d: DVec3) -> egui::Pos2 {
    let uv = equirect::from_dir(d);
    egui::pos2(
        rect.left() + uv.x as f32 * rect.width(),
        rect.top() + uv.y as f32 * rect.height(),
    )
}

/// The direction at point `p` of a thumbnail in `rect`.
pub fn from_thumb(rect: egui::Rect, p: egui::Pos2) -> DVec3 {
    let u = f64::from((p.x - rect.left()) / rect.width().max(1.0));
    let v = f64::from((p.y - rect.top()) / rect.height().max(1.0));
    equirect::to_dir(DVec2::new(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
}

/// A level view looking along `d`: roll 0 (section 10.5).
pub fn level_at(d: DVec3) -> DQuat {
    let ll = LonLat::from_dir(d);
    Ypr::new(ll.lon, ll.lat, 0.0).to_quat()
}

/// The output frame's edge as seen through `view`, as directions: the
/// frame's four sides in a `size` output, corner to corner, in order.
/// A lens that can't show a point leaves it out.
pub fn footprint(view: &ViewParams, size: (u32, u32)) -> Vec<DVec3> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let n = EDGE_POINTS as f64;
    let side = |a: (f64, f64), b: (f64, f64)| {
        (0..EDGE_POINTS).map(move |i| {
            let f = i as f64 / n;
            (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
        })
    };
    side((0.0, 0.0), (w, 0.0))
        .chain(side((w, 0.0), (w, h)))
        .chain(side((w, h), (0.0, h)))
        .chain(side((0.0, h), (0.0, 0.0)))
        .chain(std::iter::once((0.0, 0.0)))
        .map(|(x, y)| view.dir(size, x, y))
        .filter(|d| d.is_finite())
        .collect()
}

/// The centre's path over the trim, a sample a frame, thinned to
/// [`PATH_POINTS`].
pub fn path(doc: &Doc) -> Vec<DVec3> {
    let r = doc.clip().source;
    let (start, end) = (r.start.to_secs_f64(), r.end.to_secs_f64());
    let frame = doc.rate().frame_duration().to_secs_f64().max(1e-6);
    let frames = (((end - start) / frame).ceil() as usize).max(1);
    let step = frames.div_ceil(PATH_POINTS).max(1);
    let anim = doc.anim();
    let targets = doc.project.targets();
    (0..frames)
        .step_by(step)
        .map(|i| start + i as f64 * frame)
        .chain(std::iter::once((end - frame).max(start)))
        .map(|t| anim.orientation_at(t, targets) * DVec3::NEG_Z)
        .collect()
}

/// `points` on the thumbnail as polylines, broken where they cross the
/// seam behind.
fn polylines(rect: egui::Rect, points: impl Iterator<Item = DVec3>) -> Vec<Vec<egui::Pos2>> {
    let mut lines: Vec<Vec<egui::Pos2>> = vec![Vec::new()];
    for d in points {
        let p = to_thumb(rect, d);
        let line = lines.last_mut().map(|l| (l.last().copied(), l));
        match line {
            Some((Some(last), _)) if (p.x - last.x).abs() > rect.width() / 2.0 => {
                lines.push(vec![p]);
            }
            Some((_, l)) => l.push(p),
            None => lines.push(vec![p]),
        }
    }
    lines.retain(|l| l.len() >= 2);
    lines
}

/// The Sphere pane.
pub fn overview_ui(s: &mut AppState, ui: &mut egui::Ui) {
    let avail = ui.available_rect_before_wrap();
    let w = avail.width().min(avail.height() * 2.0).max(1.0);
    let rect = egui::Rect::from_min_size(avail.min, egui::vec2(w, w / 2.0));
    let rect = crate::viewer::pixel_rect(rect, ui.ctx().pixels_per_point());
    let response = ui.allocate_rect(rect, egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "The sphere"));
    s.reframe.overview.rect = Some(rect);
    let shown = s.overview_frame.clone().filter(|_| !s.raw_view());
    let has_picture = shown.is_some();
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        OverviewCallback { rect, shown },
    ));
    let painter = ui.painter_at(rect);
    if !has_picture {
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No picture",
            egui::FontId::proportional(13.0),
            egui::Color32::GRAY,
        );
    }
    let t = s.frame_time();
    let Some(doc) = s.reframe.doc.as_ref() else {
        return;
    };
    let out = doc.output();
    let path_color = egui::Color32::from_rgba_unmultiplied(255, 220, 90, 200);
    for line in polylines(rect, path(doc).into_iter()) {
        painter.add(egui::Shape::line(line, egui::Stroke::new(1.5, path_color)));
    }
    let anim = doc.anim();
    let targets = doc.project.targets();
    for k in &anim.orientation.keys {
        let d = anim.orientation_at(k.at.to_secs_f64(), targets) * DVec3::NEG_Z;
        let p = to_thumb(rect, d);
        let fill = if doc.selection.contains(&k.id) {
            ui.visuals().selection.stroke.color
        } else {
            path_color
        };
        painter.circle_filled(p, 3.5, fill);
    }
    let font = egui::FontId::proportional(11.0);
    for tg in targets.iter().filter(|tg| tg.media == doc.clip().media) {
        let a = tg.at(t.to_secs_f64());
        let p = to_thumb(
            rect,
            LonLat {
                lon: a.yaw,
                lat: a.pitch,
            }
            .to_dir(),
        );
        let c = egui::Color32::from_rgb(120, 230, 255);
        let k = 5.0;
        painter.line_segment(
            [p - egui::vec2(k, k), p + egui::vec2(k, k)],
            egui::Stroke::new(1.5, c),
        );
        painter.line_segment(
            [p + egui::vec2(-k, k), p + egui::vec2(k, -k)],
            egui::Stroke::new(1.5, c),
        );
        painter.text(
            p + egui::vec2(7.0, -7.0),
            egui::Align2::LEFT_BOTTOM,
            &tg.name,
            font.clone(),
            c,
        );
    }
    let red = egui::Color32::from_rgb(255, 70, 60);
    match out {
        Output::Flat { .. } => {
            // What the viewer shows: the keys' view, or a look-around.
            let view = ViewParams::new(s.view.q, s.view.lens);
            for line in polylines(rect, footprint(&view, out.size()).into_iter()) {
                painter.add(egui::Shape::line(line, egui::Stroke::new(2.0, red)));
            }
            let c = to_thumb(rect, s.view.q * DVec3::NEG_Z);
            painter.circle_stroke(c, 4.0, egui::Stroke::new(2.0, red));
        }
        Output::Equirect { orientation, .. } => {
            let q = orientation.quat();
            let c = to_thumb(rect, q * DVec3::NEG_Z);
            painter.circle_stroke(c, 6.0, egui::Stroke::new(2.0, red));
            painter.line_segment(
                [c - egui::vec2(9.0, 0.0), c + egui::vec2(9.0, 0.0)],
                egui::Stroke::new(1.5, red),
            );
            painter.line_segment(
                [c - egui::vec2(0.0, 9.0), c + egui::vec2(0.0, 9.0)],
                egui::Stroke::new(1.5, red),
            );
            painter.text(
                c + egui::vec2(10.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                "Front",
                font.clone(),
                red,
            );
        }
    }
    ui.weak(match out {
        Output::Flat { .. } => "Click to look there, level",
        Output::Equirect { .. } => "Click to turn the 360 output's front there, level",
    });
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let q = level_at(from_thumb(rect, p));
        match out {
            Output::Flat { .. } => s.look_at(q),
            Output::Equirect { .. } => {
                let edits = Orientation::new(q)
                    .map(|o| doc.set_front(o))
                    .unwrap_or_default();
                s.edit("Turn the front", &edits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use spherecut_sphere::perspective::{Lens, Projection};

    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 200.0))
    }

    #[test]
    fn the_thumbnail_maps_both_ways() {
        let r = rect();
        // Forward is the middle; yaw 90° is three quarters across.
        let mid = to_thumb(r, DVec3::NEG_Z);
        assert!((mid.x - 210.0).abs() < 1e-3 && (mid.y - 120.0).abs() < 1e-3);
        let right = to_thumb(
            r,
            Ypr::from_degrees(90.0, 0.0, 0.0).to_quat() * DVec3::NEG_Z,
        );
        assert!((right.x - 310.0).abs() < 1e-3, "{right:?}");
        let up = to_thumb(
            r,
            Ypr::from_degrees(0.0, 45.0, 0.0).to_quat() * DVec3::NEG_Z,
        );
        assert!((up.y - 70.0).abs() < 1e-3, "{up:?}");
        let p = egui::pos2(123.0, 45.0);
        let back = to_thumb(r, from_thumb(r, p));
        assert!((back - p).length() < 1e-3);
    }

    #[test]
    fn a_click_looks_there_level() {
        let d = Ypr::from_degrees(60.0, 20.0, 0.0).to_quat() * DVec3::NEG_Z;
        let y = Ypr::from_quat(level_at(d));
        assert!((y.yaw.to_degrees() - 60.0).abs() < 1e-9);
        assert!((y.pitch.to_degrees() - 20.0).abs() < 1e-9);
        assert!(y.roll.abs() < 1e-9);
    }

    #[test]
    fn the_footprint_is_the_frame_around_the_centre() {
        let q = Ypr::from_degrees(30.0, 0.0, 0.0).to_quat();
        let view = ViewParams::new(q, Lens::new(Projection::Rectilinear, 90f64.to_radians()));
        let f = footprint(&view, (1920, 1080));
        assert_eq!(f.len(), 4 * EDGE_POINTS + 1);
        // The left and right edges' middles are 45° either side.
        let left = LonLat::from_dir(f[3 * EDGE_POINTS + EDGE_POINTS / 2]);
        let right = LonLat::from_dir(f[EDGE_POINTS + EDGE_POINTS / 2]);
        assert!(
            (left.lon.to_degrees() + 15.0).abs() < 1e-6,
            "{}",
            left.lon.to_degrees()
        );
        assert!(
            (right.lon.to_degrees() - 75.0).abs() < 1e-6,
            "{}",
            right.lon.to_degrees()
        );
    }

    #[test]
    fn a_line_across_the_seam_breaks() {
        let r = rect();
        let pts = [170.0f64, 179.0, -179.0, -170.0]
            .map(|yaw| Ypr::from_degrees(yaw, 0.0, 0.0).to_quat() * DVec3::NEG_Z);
        let lines = polylines(r, pts.into_iter());
        assert_eq!(lines.len(), 2);
    }
}
