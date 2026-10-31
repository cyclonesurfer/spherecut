//! The timeline strip (M2 plan section 10.1; amendment 2 items 39 and
//! 53): under the viewer in place of M1's scrub bar while a project is
//! open. From the top: the markers, the scrub bar with the trim's handles
//! and the source past them dimmed, and a lane of keys for each of the
//! view's three tracks. A drag draws where it goes and edits on release,
//! one undo step.

use std::collections::BTreeSet;
use std::time::Instant;

use spherecut_core::Time;
use spherecut_core::project::{Ease, Edit, KeyId, MarkerId};
use spherecut_media::backend::SeekMode;
use spherecut_media::doctor::Severity;

use crate::app::AppState;
use crate::doc::{Doc, Lane};
use crate::notices::{Notice, NoticeKind};
use crate::transport;

/// The ease presets a key's right-click menu gives.
pub const EASES: [(&str, Ease); 6] = [
    ("Smooth", Ease::SMOOTH),
    ("Stop", Ease::STOP),
    ("Linear", Ease::LINEAR),
    ("Arrive", Ease::ARRIVE),
    ("Leave", Ease::LEAVE),
    ("Hold", Ease::HOLD),
];

/// How near, in points, `Shift` snaps a drag to a marker or the playhead.
pub const SNAP_POINTS: f32 = 8.0;
/// The lanes' names, left of them.
const LABEL_WIDTH: f32 = 84.0;
const ROW: f32 = 16.0;
/// Half a key's diamond, corner to centre, in points.
const DIAMOND: f32 = 5.0;

/// A drag of the strip, where it has got to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    In(Time),
    Out(Time),
    Marker {
        id: MarkerId,
        at: Time,
    },
    /// The selected keys, by as much as the grabbed key went from `from`
    /// to `to`.
    Keys {
        from: Time,
        to: Time,
    },
}

/// The strip's own state.
#[derive(Debug, Default)]
pub struct Strip {
    drag: Option<Drag>,
    /// The marker being renamed, and its name so far.
    rename: Option<(MarkerId, String)>,
}

/// What the strip's input asks for, taken once it has drawn.
enum Action {
    Seek(Time),
    Select(KeyId, bool),
    Deselect,
    Drag(Option<Drag>),
    Rename(Option<(MarkerId, String)>),
    Edit(&'static str, Vec<Edit>),
    Refused(&'static str, &'static str),
}

/// Source time along the strip's track.
#[derive(Debug, Clone, Copy)]
struct Track {
    left: f32,
    width: f32,
    duration: Time,
}

impl Track {
    fn x(&self, t: Time) -> f32 {
        let secs = self.duration.to_secs_f64().max(1e-9);
        self.left + (t.to_secs_f64() / secs).clamp(0.0, 1.0) as f32 * self.width
    }

    fn time(&self, x: f32) -> Time {
        let f = f64::from(((x - self.left) / self.width.max(1.0)).clamp(0.0, 1.0));
        transport::time_of_fraction(f, self.duration)
    }
}

/// `t` on a frame, or with `shift` on the marker or playhead within
/// [`SNAP_POINTS`] of it.
fn snap(doc: &Doc, track: &Track, t: Time, shift: bool, playhead: Time) -> Time {
    if shift {
        let x = track.x(t);
        let near = doc
            .clip()
            .markers
            .iter()
            .map(|m| m.at)
            .chain([playhead])
            .map(|c| ((track.x(c) - x).abs(), c))
            .filter(|(d, _)| *d <= SNAP_POINTS)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, c)) = near {
            return c;
        }
    }
    doc.snap(t)
}

/// The strip, at the playhead `at`.
pub fn strip_ui(s: &mut AppState, ui: &mut egui::Ui, at: Time, now: Instant) {
    let Some(doc) = s.reframe.doc.as_ref() else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW * 5.0),
        egui::Sense::hover(),
    );
    let track = Track {
        left: rect.left() + LABEL_WIDTH,
        width: (rect.width() - LABEL_WIDTH - DIAMOND).max(1.0),
        duration: doc.duration().max(Time::ZERO),
    };
    let row = |i: usize| {
        egui::Rect::from_min_size(
            egui::pos2(rect.left() + LABEL_WIDTH, rect.top() + ROW * i as f32),
            egui::vec2(rect.width() - LABEL_WIDTH, ROW),
        )
    };
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    for (i, name) in ["Markers", "", "Orientation", "FOV", "Projection"]
        .into_iter()
        .enumerate()
    {
        painter.text(
            egui::pos2(rect.left() + 2.0, rect.top() + ROW * (i as f32 + 0.5)),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }
    let strip = &s.reframe.strip;
    let (drag, rename) = (strip.drag, strip.rename.clone());
    let shift = ui.input(|i| i.modifiers.shift);
    let mut actions = Vec::new();
    markers_row(
        ui,
        doc,
        row(0),
        &track,
        at,
        (drag, rename, shift),
        &mut actions,
    );
    let trim = doc.clip().source;
    let (start, end) = match drag {
        Some(Drag::In(t)) => (t, trim.end),
        Some(Drag::Out(t)) => (trim.start, t),
        _ => (trim.start, trim.end),
    };
    for (i, lane) in Lane::ALL.into_iter().enumerate() {
        lane_row(
            ui,
            doc,
            row(2 + i),
            lane,
            &track,
            at,
            (drag, shift, start, end),
            &mut actions,
        );
    }
    // The bar last of the rows, so its handles are over the lanes' and
    // markers' edges.
    let bar = row(1);
    transport::scrub_row(s, ui, bar, at, Some(track.duration), now);
    let Some(doc) = s.reframe.doc.as_ref() else {
        return;
    };
    let painter = ui.painter_at(rect);
    let dim = egui::Color32::from_black_alpha(150);
    let bar_left = egui::Rect::from_x_y_ranges(bar.left()..=track.x(start), bar.y_range());
    let bar_right = egui::Rect::from_x_y_ranges(track.x(end)..=bar.right(), bar.y_range());
    painter.rect_filled(bar_left, 0.0, dim);
    painter.rect_filled(bar_right, 0.0, dim);
    handles(
        ui,
        doc,
        bar,
        &track,
        at,
        (drag, shift, start, end),
        &mut actions,
    );
    let x = track.x(at);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.5, visuals.strong_text_color()),
    );
    apply(s, actions);
}

fn markers_row(
    ui: &mut egui::Ui,
    doc: &Doc,
    row: egui::Rect,
    track: &Track,
    at: Time,
    (drag, rename, shift): (Option<Drag>, Option<(MarkerId, String)>, bool),
    actions: &mut Vec<Action>,
) {
    let response = ui.interact(row, egui::Id::unique("strip-markers"), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Row, true, "Markers"));
    let color = ui.visuals().text_color();
    let font = egui::FontId::proportional(11.0);
    for m in &doc.clip().markers {
        let t = match drag {
            Some(Drag::Marker { id, at }) if id == m.id => at,
            _ => m.at,
        };
        let x = track.x(t);
        let top = row.top() + 2.0;
        let painter = ui.painter_at(row);
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x, top),
                egui::pos2(x + 8.0, top + 4.0),
                egui::pos2(x, top + 8.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
        painter.line_segment(
            [egui::pos2(x, top), egui::pos2(x, row.bottom())],
            egui::Stroke::new(1.0, color),
        );
        let text = painter.text(
            egui::pos2(x + 10.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            &m.name,
            font.clone(),
            color,
        );
        let hit =
            egui::Rect::from_min_max(egui::pos2(x - 3.0, row.top()), text.right_bottom()).union(
                egui::Rect::from_min_size(egui::pos2(x - 3.0, row.top()), egui::vec2(12.0, ROW)),
            );
        let r = ui
            .interact(
                hit,
                egui::Id::unique(("strip-marker", m.id)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        let name = m.name.clone();
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if r.double_clicked() {
            actions.push(Action::Rename(Some((m.id, m.name.clone()))));
        } else if r.clicked() {
            actions.push(Action::Seek(m.at));
        }
        if r.drag_started() {
            actions.push(Action::Drag(Some(Drag::Marker { id: m.id, at: m.at })));
        }
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
        {
            let to = snap(doc, track, track.time(pos.x), shift, at);
            actions.push(Action::Drag(Some(Drag::Marker { id: m.id, at: to })));
        }
        if r.drag_stopped() {
            if let Some(Drag::Marker { id, at: to }) = drag
                && id == m.id
            {
                actions.push(Action::Edit("Move marker", doc.move_marker(id, to)));
            }
            actions.push(Action::Drag(None));
        }
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                actions.push(Action::Rename(Some((m.id, m.name.clone()))));
                ui.close();
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::Edit(
                    "Remove marker",
                    vec![Edit::RemoveMarker { marker: m.id }],
                ));
                ui.close();
            }
        });
    }
    let Some((id, mut name)) = rename else {
        return;
    };
    let Some(m) = doc.clip().marker(id) else {
        actions.push(Action::Rename(None));
        return;
    };
    let x = track.x(m.at) + 10.0;
    let field =
        egui::Rect::from_min_size(egui::pos2(x, row.top() - 2.0), egui::vec2(140.0, ROW + 4.0));
    let r = ui.put(
        field,
        egui::TextEdit::singleline(&mut name)
            .id(egui::Id::unique("strip-rename"))
            .desired_width(140.0),
    );
    if !r.has_focus() && !r.lost_focus() {
        r.request_focus();
    }
    if r.lost_focus() {
        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let name = name.trim();
        if enter && !escape && !name.is_empty() {
            actions.push(Action::Edit("Rename marker", doc.rename_marker(id, name)));
        }
        actions.push(Action::Rename(None));
    } else {
        actions.push(Action::Rename(Some((id, name))));
    }
}

#[expect(clippy::too_many_arguments, reason = "one row of the strip")]
fn lane_row(
    ui: &mut egui::Ui,
    doc: &Doc,
    row: egui::Rect,
    lane: Lane,
    track: &Track,
    at: Time,
    (drag, shift, start, end): (Option<Drag>, bool, Time, Time),
    actions: &mut Vec<Action>,
) {
    let label = format!("{} keys", lane.label());
    let response = ui.interact(
        row,
        egui::Id::unique(("strip-lane", lane)),
        egui::Sense::click(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Row, true, &label));
    if response.clicked() {
        actions.push(Action::Deselect);
    }
    let visuals = ui.visuals();
    let (fill, picked) = (visuals.text_color(), visuals.selection.stroke.color);
    let by = match drag {
        Some(Drag::Keys { from, to }) => to.checked_sub(from).ok(),
        _ => None,
    };
    for k in doc.lane_keys(lane) {
        let selected = doc.selection.contains(&k.id);
        let t = by
            .filter(|_| selected)
            .and_then(|b| k.at.checked_add(b).ok())
            .unwrap_or(k.at);
        let c = egui::pos2(track.x(t), row.center().y);
        let points = vec![
            c + egui::vec2(0.0, -DIAMOND),
            c + egui::vec2(DIAMOND, 0.0),
            c + egui::vec2(0.0, DIAMOND),
            c + egui::vec2(-DIAMOND, 0.0),
        ];
        let color = if selected { picked } else { fill };
        // Keys past the trim shape the curve at its edges, but aren't
        // exported: drawn hollow.
        let inside = k.at >= start && k.at < end;
        let shape = if inside {
            egui::Shape::convex_polygon(points, color, egui::Stroke::NONE)
        } else {
            egui::Shape::closed_line(points, egui::Stroke::new(1.5, color))
        };
        ui.painter_at(row.expand(DIAMOND)).add(shape);
        let r = ui.interact(
            egui::Rect::from_center_size(c, egui::vec2(2.0 * DIAMOND + 4.0, ROW)),
            egui::Id::unique(("strip-key", k.id)),
            egui::Sense::click_and_drag(),
        );
        let name = format!("{} key at {}", lane.label(), transport::format_time(k.at));
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &name));
        let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        if r.clicked() {
            actions.push(Action::Select(k.id, toggle));
            actions.push(Action::Seek(k.at));
        }
        if r.drag_started() {
            if !selected {
                actions.push(Action::Select(k.id, false));
            }
            actions.push(Action::Drag(Some(Drag::Keys {
                from: k.at,
                to: k.at,
            })));
        }
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
            && let Some(Drag::Keys { from, .. }) = drag
        {
            let to = snap(doc, track, track.time(pos.x), shift, at);
            actions.push(Action::Drag(Some(Drag::Keys { from, to })));
        }
        if r.drag_stopped() {
            if let Some(Drag::Keys { from, to }) = drag
                && let Ok(by) = to.checked_sub(from)
            {
                match doc.move_keys(&doc.selection, by) {
                    Some(edits) => actions.push(Action::Edit("Move keys", edits)),
                    None => actions.push(Action::Refused(
                        "Move keys",
                        "a key would leave the video or land on another key's time",
                    )),
                }
            }
            actions.push(Action::Drag(None));
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

fn handles(
    ui: &mut egui::Ui,
    doc: &Doc,
    bar: egui::Rect,
    track: &Track,
    at: Time,
    (drag, shift, start, end): (Option<Drag>, bool, Time, Time),
    actions: &mut Vec<Action>,
) {
    let color = ui.visuals().selection.stroke.color;
    let frame = doc.rate().frame_duration();
    for (out, t, label) in [(false, start, "In handle"), (true, end, "Out handle")] {
        let x = track.x(t);
        let painter = ui.painter_at(bar.expand(2.0));
        let (inner, outer) = if out { (x - 4.0, x) } else { (x, x + 4.0) };
        painter.rect_filled(
            egui::Rect::from_x_y_ranges(inner.min(outer)..=inner.max(outer), bar.y_range()),
            1.0,
            color,
        );
        let r = ui
            .interact(
                egui::Rect::from_center_size(egui::pos2(x, bar.center().y), egui::vec2(10.0, ROW)),
                egui::Id::unique(("strip-handle", out)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, label));
        let make = |t: Time| if out { Drag::Out(t) } else { Drag::In(t) };
        if r.drag_started() {
            actions.push(Action::Drag(Some(make(t))));
        }
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
        {
            let to = snap(doc, track, track.time(pos.x), shift, at);
            // In stays a frame before out, and out a frame after in.
            let to = if out {
                to.max(start.checked_add(frame).unwrap_or(start))
            } else {
                to.min(end.checked_sub(frame).unwrap_or(end))
            };
            actions.push(Action::Drag(Some(make(to))));
        }
        if r.drag_stopped() {
            match drag {
                Some(Drag::In(t)) if !out => actions.push(Action::Edit("Move in", doc.set_in(t))),
                Some(Drag::Out(t)) if out => actions.push(Action::Edit("Move out", doc.set_out(t))),
                _ => {}
            }
            actions.push(Action::Drag(None));
        }
    }
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
            Action::Deselect => {
                if let Some(doc) = &mut s.reframe.doc {
                    doc.selection.clear();
                }
            }
            Action::Drag(d) => s.reframe.strip.drag = d,
            Action::Rename(r) => s.reframe.strip.rename = r,
            Action::Edit(label, edits) => {
                s.edit(label, &edits);
            }
            Action::Refused(label, why) => s.notices.push(
                Notice::new(
                    NoticeKind::Edit,
                    Severity::Warning,
                    format!("{label}: not done"),
                )
                .impact(why),
            ),
        }
    }
}
