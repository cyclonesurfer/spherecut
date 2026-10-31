//! The curve editor (M2 plan sections 6.4 and 10.4; amendment 2 item
//! 39): the view's tracks over the clip's source time, hand-drawn with
//! egui's painter. A row each for yaw, pitch and roll in degrees (yaw
//! and roll unwrapped, so a turn shows as yaw rising past 180°), the
//! field of view in degrees, and the projection as a band per segment
//! with the morph's progress drawn in its ease's shape. A key is a dot;
//! dragging one up or down changes that value of that key only, shown
//! through the project's preview and kept as one step on release, with
//! section 6.4's sticky spin. Ctrl and the wheel zoom, the wheel or a
//! drag of the background pans, a click seeks.

use std::collections::BTreeSet;

use spherecut_core::Time;
use spherecut_core::project::{
    AnyValue, Edit, KeyId, Orient, OrientMode, Target, ViewAnim, follow_spin,
};
use spherecut_core::time::Rational;
use spherecut_media::backend::SeekMode;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{LensShape, MIN_HFOV_DEG};
use spherecut_sphere::spline::spun;

use crate::app::AppState;
use crate::doc::{self, Doc, KeyMark, Lane, MAX_FOV_DEG};
use crate::timeline::EASES;
use crate::transport::format_time;

/// Past this pitch, in degrees, yaw and roll are drawn broken and
/// greyed, and their dots aren't dragged (section 6.4).
pub const STEEP_DEG: f64 = 85.0;
/// The rows' names and numbers, left of them.
const GUTTER: f32 = 64.0;
const DOT: f32 = 4.0;
/// The narrowest span the zoom reaches, in frames.
const MIN_FRAMES: f64 = 8.0;

/// A row of the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Row {
    Yaw,
    Pitch,
    Roll,
    Fov,
    Projection,
}

impl Row {
    pub const ALL: [Row; 5] = [Row::Yaw, Row::Pitch, Row::Roll, Row::Fov, Row::Projection];

    pub fn label(self) -> &'static str {
        match self {
            Row::Yaw => "Yaw",
            Row::Pitch => "Pitch",
            Row::Roll => "Roll",
            Row::Fov => "FOV",
            Row::Projection => "Projection",
        }
    }

    fn lane(self) -> Lane {
        match self {
            Row::Yaw | Row::Pitch | Row::Roll => Lane::Orientation,
            Row::Fov => Lane::Fov,
            Row::Projection => Lane::Projection,
        }
    }

    /// Which of yaw, pitch and roll it is.
    fn angle(self) -> Option<usize> {
        match self {
            Row::Yaw => Some(0),
            Row::Pitch => Some(1),
            Row::Roll => Some(2),
            Row::Fov | Row::Projection => None,
        }
    }
}

/// A key's dot being dragged.
#[derive(Debug, Clone, PartialEq)]
struct CurveDrag {
    row: Row,
    key: KeyId,
    /// The value at the start, and the pointer's height then.
    start: f64,
    y0: f32,
    /// The row's range at the start, kept through the drag so its scale
    /// doesn't move under the pointer.
    range: (f64, f64),
    value: f64,
    /// The spins the drag has set to keep the path turning the way it
    /// went (section 6.4).
    spins: Vec<(KeyId, i8)>,
}

/// The editor's own state.
#[derive(Debug, Default)]
pub struct Curves {
    /// The source time at the left edge and the seconds shown; none
    /// shows the whole source.
    span: Option<(f64, f64)>,
    drag: Option<CurveDrag>,
}

impl Curves {
    /// A drag of a key runs.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The drag let go, unkept (the caller drops the preview).
    pub fn cancel(&mut self) {
        self.drag = None;
    }
}

/// The curves at a list of times.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Samples {
    pub t: Vec<f64>,
    /// Degrees: yaw and roll unwrapped, pitch in [−90, 90].
    pub yaw: Vec<f64>,
    pub pitch: Vec<f64>,
    pub roll: Vec<f64>,
    /// |pitch| > [`STEEP_DEG`]: yaw and roll there mean little.
    pub steep: Vec<bool>,
    /// The horizontal field of view in degrees.
    pub fov: Vec<f64>,
    /// How far each projection segment's morph has gone, 0 to 1, shaped
    /// by its ease; 0 between keys of one shape.
    pub morph: Vec<f64>,
}

/// `a` in (−180, 180].
fn wrap_deg(a: f64) -> f64 {
    let w = (a + 180.0).rem_euclid(360.0) - 180.0;
    if w <= -180.0 { w + 360.0 } else { w }
}

/// The orientation keys' times, with yaw and roll unwrapped from key to
/// key: each segment the shortest way plus its spin's turns (Level mode;
/// Free spins about its own axis, so its yaw takes the shortest way).
fn anchors(anim: &ViewAnim, targets: &[Target]) -> Vec<(f64, f64, f64)> {
    let mut out: Vec<(f64, f64, f64)> = Vec::new();
    let keys = &anim.orientation.keys;
    for (i, k) in keys.iter().enumerate() {
        let t = k.at.to_secs_f64();
        let y = Ypr::from_quat(anim.orientation_at(t, targets));
        let (yaw, roll) = (y.yaw.to_degrees(), y.roll.to_degrees());
        match out.last() {
            None => out.push((t, yaw, roll)),
            Some(&(_, py, pr)) => {
                let spin = match anim.mode {
                    OrientMode::Level => keys[i - 1].spin,
                    OrientMode::Free => 0,
                };
                out.push((
                    t,
                    py + spun(wrap_deg(yaw - py), spin, 360.0),
                    pr + wrap_deg(roll - pr),
                ));
            }
        }
    }
    out
}

/// `anim`'s curves at `times`, seconds, in increasing order (section
/// 6.4): yaw and roll unwrapped against the sample before, and at the
/// first sample past each key against the keys' own turns, so a spin
/// shows as the turns it makes.
pub fn sample(anim: &ViewAnim, targets: &[Target], times: &[f64]) -> Samples {
    let anchors = anchors(anim, targets);
    let mut s = Samples::default();
    let mut last_anchor = None;
    for &t in times {
        let v = anim.at(t, targets);
        let y = Ypr::from_quat(v.orientation);
        let (yaw, pitch, roll) = (
            y.yaw.to_degrees(),
            y.pitch.to_degrees(),
            y.roll.to_degrees(),
        );
        let i = anchors.partition_point(|a| a.0 <= t).saturating_sub(1);
        let fresh = last_anchor != Some(i) || s.t.is_empty();
        last_anchor = Some(i);
        let (ry, rr) = match (fresh, s.yaw.last(), s.roll.last()) {
            (false, Some(&py), Some(&pr)) => (py, pr),
            _ => match (anchors.get(i), anchors.get(i + 1)) {
                (Some(a), Some(b)) if t > a.0 && b.0 > a.0 => {
                    let f = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
                    (a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
                }
                (Some(a), _) => (a.1, a.2),
                _ => (yaw, roll),
            },
        };
        s.t.push(t);
        s.yaw.push(ry + wrap_deg(yaw - ry));
        s.pitch.push(pitch);
        s.roll.push(rr + wrap_deg(roll - rr));
        s.steep.push(pitch.abs() > STEEP_DEG);
        s.fov.push(v.lens.hfov.to_degrees());
        s.morph.push(morph_progress(anim, t, v.lens.shape()));
    }
    s
}

/// How far the projection segment at `t` has gone from its first key's
/// shape to its second's, 0 to 1.
fn morph_progress(anim: &ViewAnim, t: f64, shape: LensShape) -> f64 {
    let keys = &anim.projection.keys;
    let i = keys.partition_point(|k| k.at.to_secs_f64() <= t);
    let (Some(a), Some(b)) = (i.checked_sub(1).and_then(|i| keys.get(i)), keys.get(i)) else {
        return 0.0;
    };
    let period = LensShape::PERIMETER;
    let wrap = |d: f64| spherecut_sphere::spline::wrap_period(d, period);
    let pa = a.value.perimeter();
    let d = wrap(b.value.perimeter() - pa);
    if d.abs() < 1e-12 {
        return 0.0;
    }
    (wrap(shape.perimeter() - pa) / d).clamp(0.0, 1.0)
}

/// A shape's name in the band.
fn shape_label(s: LensShape) -> String {
    match s {
        LensShape::Pure(p) => crate::view::projection_label(p).to_owned(),
        LensShape::Morph(m) => format!(
            "{}–{} {:.2}",
            crate::view::projection_label(m.from),
            crate::view::projection_label(m.to),
            m.morph
        ),
    }
}

/// Seconds along the plots.
#[derive(Debug, Clone, Copy)]
struct Axis {
    left: f32,
    width: f32,
    t0: f64,
    dur: f64,
}

impl Axis {
    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.t0) / self.dur) as f32 * self.width
    }

    fn t(&self, x: f32) -> f64 {
        self.t0 + f64::from((x - self.left) / self.width.max(1.0)) * self.dur
    }
}

fn y_of(rect: egui::Rect, (lo, hi): (f64, f64), v: f64) -> f32 {
    rect.bottom() - (((v - lo) / (hi - lo).max(1e-9)) as f32) * rect.height()
}

/// A row's range: its values' with a margin, at least `least` across.
fn fit(values: impl Iterator<Item = f64>, margin: f64, least: f64) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in values.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !lo.is_finite() {
        return (-least / 2.0, least / 2.0);
    }
    let (mut lo, mut hi) = (lo - margin, hi + margin);
    if hi - lo < least {
        let mid = (lo + hi) / 2.0;
        lo = mid - least / 2.0;
        hi = mid + least / 2.0;
    }
    (lo, hi)
}

enum Action {
    Seek(Time),
    Select(KeyId, bool),
    Drag(Option<CurveDrag>),
    Preview(Option<ViewAnim>),
    Edit(&'static str, Vec<Edit>),
    Span(f64, f64),
}

/// The Curves pane.
pub fn curves_ui(s: &mut AppState, ui: &mut egui::Ui) {
    let at = s.frame_time().to_secs_f64();
    let Some(doc) = s.reframe.doc.as_ref() else {
        ui.weak("No project: open a video or a project from the Files pane");
        return;
    };
    let rect = ui.available_rect_before_wrap();
    let back = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    back.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "The curves"));
    let duration = doc.duration().to_secs_f64().max(1e-6);
    let frame = doc.rate().frame_duration().to_secs_f64();
    let (t0, dur) = s.reframe.curves.span.unwrap_or((0.0, duration));
    let axis = Axis {
        left: rect.left() + GUTTER,
        width: (rect.width() - GUTTER - 2.0 * DOT).max(1.0),
        t0,
        dur,
    };
    let mut actions = Vec::new();
    span_input(ui, &back, &axis, duration, frame, doc, &mut actions);

    // The samples: one each two points, and at every key in sight.
    let anim = doc.anim();
    let targets = doc.project.targets();
    let n = ((axis.width / 2.0) as usize).clamp(2, 2000);
    let mut times: Vec<f64> = (0..=n)
        .map(|i| t0 + dur * i as f64 / n as f64)
        .chain(
            doc.keys()
                .iter()
                .map(|k| k.at.to_secs_f64())
                .filter(|&k| k > t0 && k < t0 + dur),
        )
        .collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    let samples = sample(anim, targets, &times);

    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let rows = Row::ALL.len() as f32;
    let row_h = (rect.height() / rows).max(36.0);
    let drag = s.reframe.curves.drag.clone();
    let trim = doc.clip().source;
    let (start, end) = (trim.start.to_secs_f64(), trim.end.to_secs_f64());
    for (i, row) in Row::ALL.into_iter().enumerate() {
        let top = rect.top() + row_h * i as f32;
        let plot = egui::Rect::from_min_max(
            egui::pos2(axis.left, top + 6.0),
            egui::pos2(rect.right() - DOT, top + row_h - 6.0),
        );
        painter.rect_filled(plot, 2.0, visuals.extreme_bg_color);
        // The source past the trim, dimmed.
        let dim = egui::Color32::from_black_alpha(90);
        let (xs, xe) = (
            axis.x(start).max(plot.left()),
            axis.x(end).min(plot.right()),
        );
        if xs > plot.left() {
            painter.rect_filled(
                egui::Rect::from_x_y_ranges(plot.left()..=xs, plot.y_range()),
                0.0,
                dim,
            );
        }
        if xe < plot.right() {
            painter.rect_filled(
                egui::Rect::from_x_y_ranges(xe..=plot.right(), plot.y_range()),
                0.0,
                dim,
            );
        }
        let values: &[f64] = match row {
            Row::Yaw => &samples.yaw,
            Row::Pitch => &samples.pitch,
            Row::Roll => &samples.roll,
            Row::Fov => &samples.fov,
            Row::Projection => &samples.morph,
        };
        let range = match &drag {
            Some(d) if d.row == row => d.range,
            _ => match row {
                Row::Pitch => (-90.0, 90.0),
                Row::Projection => (-0.1, 1.1),
                Row::Fov => fit(values.iter().copied(), 5.0, 10.0),
                Row::Yaw | Row::Roll => fit(values.iter().copied(), 10.0, 20.0),
            },
        };
        let font = egui::FontId::proportional(11.0);
        painter.text(
            egui::pos2(rect.left() + 2.0, plot.center().y),
            egui::Align2::LEFT_CENTER,
            row.label(),
            font.clone(),
            visuals.text_color(),
        );
        if row != Row::Projection {
            for (v, align) in [
                (range.1, egui::Align2::RIGHT_TOP),
                (range.0, egui::Align2::RIGHT_BOTTOM),
            ] {
                let y = if align == egui::Align2::RIGHT_TOP {
                    plot.top()
                } else {
                    plot.bottom()
                };
                painter.text(
                    egui::pos2(axis.left - 4.0, y),
                    align,
                    format!("{v:.0}°"),
                    egui::FontId::proportional(9.0),
                    visuals.weak_text_color(),
                );
            }
        }
        if matches!(row, Row::Yaw | Row::Pitch | Row::Roll) && range.0 < 0.0 && range.1 > 0.0 {
            let y = y_of(plot, range, 0.0);
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(0.5, visuals.weak_text_color()),
            );
        }
        if row == Row::Projection {
            bands(&painter, plot, &axis, doc, anim, &visuals);
        }
        curve(
            &painter, plot, &axis, &samples, values, range, row, &visuals,
        );
        dots(
            ui,
            plot,
            &axis,
            doc,
            &samples,
            range,
            row,
            &drag,
            (start, end),
            &mut actions,
        );
    }
    let x = axis.x(at);
    if x >= axis.left && x <= rect.right() {
        painter.line_segment(
            [
                egui::pos2(x, rect.top()),
                egui::pos2(x, rect.top() + row_h * rows),
            ],
            egui::Stroke::new(1.5, visuals.strong_text_color()),
        );
    }
    apply(s, actions);
}

/// Zoom with Ctrl and the wheel about the pointer, pan with the wheel or
/// a drag of the background, seek with a click.
fn span_input(
    ui: &egui::Ui,
    back: &egui::Response,
    axis: &Axis,
    duration: f64,
    frame: f64,
    doc: &Doc,
    actions: &mut Vec<Action>,
) {
    let (mut t0, mut dur) = (axis.t0, axis.dur);
    let least = (frame * MIN_FRAMES).min(duration);
    if back.hovered() {
        let (zoom, scroll, pos) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom != 1.0
            && let Some(p) = pos
        {
            let about = axis.t(p.x);
            let d = (dur / f64::from(zoom)).clamp(least, duration);
            t0 = about - (about - t0) * d / dur;
            dur = d;
        }
        let pan = scroll.x + scroll.y;
        if pan != 0.0 {
            t0 -= f64::from(pan) * dur / f64::from(axis.width.max(1.0));
        }
    }
    if back.dragged_by(egui::PointerButton::Primary) {
        t0 -= f64::from(back.drag_delta().x) * dur / f64::from(axis.width.max(1.0));
    }
    if back.clicked()
        && let Some(p) = back.interact_pointer_pos()
        && p.x >= axis.left
    {
        actions.push(Action::Seek(doc.snap(secs(axis.t(p.x).max(0.0)))));
    }
    let t0 = t0.clamp(0.0, (duration - dur).max(0.0));
    if (t0, dur) != (axis.t0, axis.dur) {
        actions.push(Action::Span(t0, dur));
    }
}

/// The projection band: each segment's two shapes, named.
fn bands(
    painter: &egui::Painter,
    plot: egui::Rect,
    axis: &Axis,
    doc: &Doc,
    anim: &ViewAnim,
    visuals: &egui::Visuals,
) {
    let keys = &anim.projection.keys;
    let font = egui::FontId::proportional(10.0);
    let edges: Vec<f64> = std::iter::once(0.0)
        .chain(keys.iter().map(|k| k.at.to_secs_f64()))
        .chain(std::iter::once(doc.duration().to_secs_f64()))
        .collect();
    for (i, w) in edges.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        let (xa, xb) = (axis.x(a).max(plot.left()), axis.x(b).min(plot.right()));
        if xb - xa < 24.0 {
            continue;
        }
        // Before the first key and after the last, the shape holds.
        let from = keys.get(i.saturating_sub(1)).map(|k| k.value);
        let to = keys.get(i).map(|k| k.value);
        let text = match (i, from, to) {
            (0, _, Some(s)) => shape_label(s),
            (_, Some(f), Some(t)) if f == t => shape_label(f),
            (_, Some(f), Some(t)) => format!("{} → {}", shape_label(f), shape_label(t)),
            (_, Some(f), None) => shape_label(f),
            _ => continue,
        };
        painter
            .with_clip_rect(egui::Rect::from_x_y_ranges(xa..=xb, plot.y_range()))
            .text(
                egui::pos2(xa + 4.0, plot.top() + 2.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                visuals.weak_text_color(),
            );
    }
}

#[expect(clippy::too_many_arguments, reason = "one row's curve")]
fn curve(
    painter: &egui::Painter,
    plot: egui::Rect,
    axis: &Axis,
    samples: &Samples,
    values: &[f64],
    range: (f64, f64),
    row: Row,
    visuals: &egui::Visuals,
) {
    let color = match row {
        Row::Yaw => egui::Color32::from_rgb(230, 120, 90),
        Row::Pitch => egui::Color32::from_rgb(120, 200, 110),
        Row::Roll => egui::Color32::from_rgb(110, 160, 240),
        Row::Fov => egui::Color32::from_rgb(220, 190, 90),
        Row::Projection => egui::Color32::from_rgb(190, 130, 220),
    };
    let grey = visuals.weak_text_color();
    let broken = matches!(row, Row::Yaw | Row::Roll);
    let painter = painter.with_clip_rect(plot.expand(1.0));
    let point = |i: usize| egui::pos2(axis.x(samples.t[i]), y_of(plot, range, values[i]));
    // Runs of one kind: whole, or steep (broken and greyed).
    let mut run: Vec<egui::Pos2> = Vec::new();
    let mut steep_run = false;
    let flush = |run: &mut Vec<egui::Pos2>, steep: bool| {
        if run.len() >= 2 {
            if steep {
                painter.extend(egui::Shape::dashed_line(
                    run,
                    egui::Stroke::new(1.0, grey),
                    3.0,
                    3.0,
                ));
            } else {
                painter.add(egui::Shape::line(
                    std::mem::take(run),
                    egui::Stroke::new(1.5, color),
                ));
            }
        }
        run.clear();
    };
    for i in 0..values.len() {
        let steep = broken && samples.steep[i];
        if steep != steep_run {
            flush(&mut run, steep_run);
            steep_run = steep;
        }
        // A jump of the unwrapping across a pole isn't drawn.
        if broken && i > 0 && !run.is_empty() && (values[i] - values[i - 1]).abs() > 90.0 {
            flush(&mut run, steep_run);
        }
        run.push(point(i));
    }
    flush(&mut run, steep_run);
}

#[expect(clippy::too_many_arguments, reason = "one row's keys")]
fn dots(
    ui: &mut egui::Ui,
    plot: egui::Rect,
    axis: &Axis,
    doc: &Doc,
    samples: &Samples,
    range: (f64, f64),
    row: Row,
    drag: &Option<CurveDrag>,
    (start, end): (f64, f64),
    actions: &mut Vec<Action>,
) {
    let visuals = ui.visuals().clone();
    for k in doc.lane_keys(row.lane()) {
        let t = k.at.to_secs_f64();
        let x = axis.x(t);
        if x < plot.left() - DOT || x > plot.right() + DOT {
            continue;
        }
        let Some((value, draggable)) = key_value(doc, &k, row, samples) else {
            continue;
        };
        let ours = drag.as_ref().filter(|d| d.row == row && d.key == k.id);
        let shown = ours.map_or(value, |d| d.value);
        let c = egui::pos2(
            x,
            if row == Row::Projection {
                plot.center().y
            } else {
                y_of(plot, range, shown).clamp(plot.top(), plot.bottom())
            },
        );
        let selected = doc.selection.contains(&k.id);
        let color = if selected {
            visuals.selection.stroke.color
        } else {
            visuals.text_color()
        };
        let inside = t >= start && t < end;
        let painter = ui.painter_at(plot.expand(DOT + 1.0));
        if inside && draggable {
            painter.circle_filled(c, DOT, color);
        } else {
            painter.circle_stroke(c, DOT, egui::Stroke::new(1.5, color));
        }
        let r = ui.interact(
            egui::Rect::from_center_size(c, egui::vec2(3.0 * DOT, 3.0 * DOT)),
            egui::Id::unique(("curve-key", row, k.id)),
            egui::Sense::click_and_drag(),
        );
        let name = format!("{} key at {}", row.label(), format_time(k.at));
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &name));
        let r = if draggable {
            r.on_hover_cursor(egui::CursorIcon::ResizeVertical)
        } else {
            r
        };
        let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        if r.clicked() {
            actions.push(Action::Select(k.id, toggle));
            actions.push(Action::Seek(k.at));
        }
        if draggable && r.drag_started() {
            let y0 = r.interact_pointer_pos().map_or(c.y, |p| p.y);
            actions.push(Action::Select(k.id, false));
            actions.push(Action::Drag(Some(CurveDrag {
                row,
                key: k.id,
                start: value,
                y0,
                range,
                value,
                spins: Vec::new(),
            })));
        }
        if let Some(d) = ours
            && r.dragged()
            && let Some(p) = r.interact_pointer_pos()
        {
            let per_point = (d.range.1 - d.range.0) / f64::from(plot.height().max(1.0));
            let v = d.start + f64::from(d.y0 - p.y) * per_point;
            let v = match row {
                Row::Pitch => v.clamp(-90.0, 90.0),
                Row::Fov => v.clamp(MIN_HFOV_DEG, MAX_FOV_DEG),
                _ => v,
            };
            if let Some((preview, spins)) = drag_preview(doc, d, v) {
                let mut d = d.clone();
                d.value = v;
                d.spins = spins;
                actions.push(Action::Drag(Some(d)));
                actions.push(Action::Preview(Some(preview)));
            }
        }
        if let Some(d) = ours
            && r.drag_stopped()
        {
            let edits = value_of(doc, d.row, d.key, d.value)
                .filter(|_| d.value != d.start && !crate::app::escaped(ui))
                .map(|v| doc.set_value(d.key, v, &d.spins))
                .unwrap_or_default();
            actions.push(Action::Drag(None));
            actions.push(Action::Preview(None));
            actions.push(Action::Edit(edit_label(d.row), edits));
        }
        r.context_menu(|ui| {
            for (name, ease) in EASES {
                if ui.selectable_label(k.ease == ease, name).clicked() {
                    let keys = if selected {
                        doc.selection.clone()
                    } else {
                        BTreeSet::from([k.id])
                    };
                    actions.push(Action::Edit("Set ease", doc.set_ease(&keys, ease)));
                    ui.close();
                }
            }
        });
    }
}

fn edit_label(row: Row) -> &'static str {
    match row {
        Row::Yaw => "Move yaw",
        Row::Pitch => "Move pitch",
        Row::Roll => "Move roll",
        Row::Fov => "Move FOV",
        Row::Projection => "Move key",
    }
}

/// A key's value on `row`, where its dot sits (yaw and roll on the
/// unwrapped curve), and whether a drag takes it: an aim key's angles
/// come from its target, and near a pole yaw and roll mean little.
fn key_value(doc: &Doc, k: &KeyMark, row: Row, samples: &Samples) -> Option<(f64, bool)> {
    let t = k.at.to_secs_f64();
    let near = |values: &[f64]| {
        let i = samples.t.partition_point(|&s| s < t);
        [i.checked_sub(1), Some(i)]
            .into_iter()
            .flatten()
            .filter_map(|i| Some((samples.t.get(i)?, values.get(i)?)))
            .min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()))
            .map(|(_, v)| *v)
    };
    match row {
        Row::Yaw | Row::Pitch | Row::Roll => {
            let key = doc.orientation_key(k.id)?;
            let i = row.angle().unwrap_or(0);
            let (y, view) = match key.value {
                Orient::View(o) => (o.ypr(), true),
                Orient::Aim { .. } => (
                    Ypr::from_quat(doc.anim().orientation_at(t, doc.project.targets())),
                    false,
                ),
            };
            let own = [y.yaw, y.pitch, y.roll][i].to_degrees();
            let steep = y.pitch.to_degrees().abs() > STEEP_DEG;
            let value = match row {
                Row::Pitch => own,
                Row::Yaw => near(&samples.yaw).map_or(own, |c| c + wrap_deg(own - c)),
                _ => near(&samples.roll).map_or(own, |c| c + wrap_deg(own - c)),
            };
            Some((value, view && (row == Row::Pitch || !steep)))
        }
        Row::Fov => doc.fov_key(k.id).map(|key| (key.value, true)),
        Row::Projection => Some((0.0, false)),
    }
}

/// The value a drag to `v` gives the key.
fn value_of(doc: &Doc, row: Row, key: KeyId, v: f64) -> Option<AnyValue> {
    match row {
        Row::Yaw | Row::Pitch | Row::Roll => {
            let Orient::View(o) = doc.orientation_key(key)?.value else {
                return None;
            };
            let o = doc::with_angle(o, row.angle()?, v)?;
            Some(AnyValue::Orientation(Orient::View(o)))
        }
        Row::Fov => Some(AnyValue::Fov(v.clamp(MIN_HFOV_DEG, MAX_FOV_DEG))),
        Row::Projection => None,
    }
}

/// The view with the dragged key at `v`, and the spins that keep each
/// segment turning the way it did the frame before (section 6.4).
fn drag_preview(doc: &Doc, d: &CurveDrag, v: f64) -> Option<(ViewAnim, Vec<(KeyId, i8)>)> {
    let value = value_of(doc, d.row, d.key, v)?;
    let targets = doc.project.targets();
    let before = doc.anim().clone();
    let mut spins = d.spins.clone();
    let next = doc.with_value(d.key, &value, &spins);
    for (id, spin) in follow_spin(&before, &next, targets) {
        match spins.iter_mut().find(|(k, _)| *k == id) {
            Some(s) => s.1 = spin,
            None => spins.push((id, spin)),
        }
    }
    Some((doc.with_value(d.key, &value, &spins), spins))
}

fn apply(s: &mut AppState, actions: Vec<Action>) {
    for a in actions {
        match a {
            Action::Seek(t) => s.seek(t, SeekMode::Exact),
            Action::Select(id, toggle) => {
                if let Some(doc) = &mut s.reframe.doc {
                    if !toggle {
                        doc.selection.clear();
                        doc.selection.insert(id);
                    } else if !doc.selection.remove(&id) {
                        doc.selection.insert(id);
                    }
                }
            }
            Action::Drag(d) => s.reframe.curves.drag = d,
            Action::Preview(p) => {
                if let Some(doc) = &mut s.reframe.doc {
                    doc.preview = p;
                }
            }
            Action::Edit(label, edits) => {
                s.edit(label, &edits);
            }
            Action::Span(t0, dur) => s.reframe.curves.span = Some((t0, dur)),
        }
    }
}

/// `s` seconds, to the microsecond.
fn secs(s: f64) -> Time {
    Time::seconds(
        Rational::new((s.max(0.0) * 1_000_000.0).round() as i64, 1_000_000)
            .unwrap_or(Rational::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::{Ease, Key, KeyTrack, Orientation};
    use spherecut_sphere::perspective::Projection;

    use super::*;

    fn key(at: f64, yaw: f64, pitch: f64, spin: i8) -> Key<Orient> {
        Key {
            id: KeyId(at as u64 + 1),
            at: secs(at),
            value: Orient::View(Orientation::from_ypr(Ypr::from_degrees(yaw, pitch, 0.0)).unwrap()),
            ease: Ease::LINEAR,
            spin,
        }
    }

    fn anim(keys: Vec<Key<Orient>>) -> ViewAnim {
        ViewAnim {
            mode: OrientMode::Level,
            orientation: KeyTrack { keys },
            fov: KeyTrack::one(KeyId(100), 90.0, Ease::SMOOTH),
            projection: KeyTrack::one(
                KeyId(101),
                LensShape::Pure(Projection::Rectilinear),
                Ease::STOP,
            ),
        }
    }

    fn times(n: usize, end: f64) -> Vec<f64> {
        (0..=n).map(|i| end * i as f64 / n as f64).collect()
    }

    #[test]
    fn yaw_rises_past_180_without_a_jump() {
        // 150° to −150° the short way is +60°, through 180.
        let a = anim(vec![key(0.0, 150.0, 0.0, 0), key(2.0, -150.0, 0.0, 0)]);
        let s = sample(&a, &[], &times(40, 2.0));
        assert!((s.yaw[0] - 150.0).abs() < 1e-6);
        assert!((s.yaw[40] - 210.0).abs() < 1e-6, "{}", s.yaw[40]);
        assert!(s.yaw.windows(2).all(|w| (w[1] - w[0]).abs() < 10.0));
    }

    #[test]
    fn a_spin_shows_as_its_turns_even_when_zoomed_in() {
        // One extra turn: 0° to 10° is 370°.
        let a = anim(vec![key(0.0, 0.0, 0.0, 1), key(4.0, 10.0, 0.0, 0)]);
        let s = sample(&a, &[], &times(200, 4.0));
        assert!((s.yaw[200] - 370.0).abs() < 1e-6, "{}", s.yaw[200]);
        // Sampled from the middle on only, the curve is where it is in
        // the whole: about 185° at 2 s.
        let mid = sample(&a, &[], &[2.0, 2.5, 3.0]);
        assert!((mid.yaw[0] - 185.0).abs() < 1e-6, "{}", mid.yaw[0]);
    }

    #[test]
    fn a_steep_view_is_marked() {
        let a = anim(vec![key(0.0, 0.0, 0.0, 0), key(1.0, 0.0, 89.0, 0)]);
        let s = sample(&a, &[], &times(10, 1.0));
        assert!(!s.steep[0]);
        assert!(s.steep[10]);
    }

    #[test]
    fn the_morph_goes_from_0_to_1_across_its_segment() {
        let mut a = anim(vec![key(0.0, 0.0, 0.0, 0)]);
        a.projection = KeyTrack {
            keys: vec![
                Key {
                    id: KeyId(1),
                    at: Time::ZERO,
                    value: LensShape::Pure(Projection::Rectilinear),
                    ease: Ease::STOP,
                    spin: 0,
                },
                Key {
                    id: KeyId(2),
                    at: Time::from_secs(1),
                    value: LensShape::Pure(Projection::Fisheye),
                    ease: Ease::STOP,
                    spin: 0,
                },
            ],
        };
        let s = sample(&a, &[], &[0.0, 0.5, 1.0, 1.5]);
        assert!(s.morph[0].abs() < 1e-9);
        assert!(
            (s.morph[1] - 0.5).abs() < 1e-9,
            "smoothstep's middle: {}",
            s.morph[1]
        );
        assert!(s.morph[3].abs() < 1e-9, "past the last key it holds");
    }

    #[test]
    fn wrap_keeps_180_and_turns_minus_180() {
        assert_eq!(wrap_deg(180.0), 180.0);
        assert_eq!(wrap_deg(-180.0), 180.0);
        assert!((wrap_deg(190.0) + 170.0).abs() < 1e-12);
        assert!((wrap_deg(-540.0) - 180.0).abs() < 1e-12);
    }
}
