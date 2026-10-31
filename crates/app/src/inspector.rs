//! The Inspector (M2 plan section 10.1): the view at the playhead in
//! numbers, editable on a key; the selected keys' time, ease and spin,
//! and what an orientation key aims at; the trim; the sequence's output,
//! a 360 output's front, and the rate; and the look-at targets.
//!
//! A number being changed shows in the viewer as it changes, through the
//! project's preview (amendment 2 item 39), and becomes one undo step
//! when its field lets go: the drag ends, or the typing does.

use spherecut_core::project::{
    AnyValue, Edit, KeyId, Orient, Orientation, Output, Target, TargetId,
};
use spherecut_core::{FrameRate, Rounding, Time};
use spherecut_export::Preset;
use spherecut_media::backend::SeekMode;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{LensShape, MIN_HFOV_DEG, Morph, Projection};

use crate::app::AppState;
use crate::doc::{self, Doc, Lane, MAX_FOV_DEG};
use crate::reframe::Look;
use crate::timeline::EASES;
use crate::transport::format_time;

/// The rates the sequence can be set to, besides its video's own.
pub const RATES: [FrameRate; 8] = [
    FrameRate::FPS_23_976,
    FrameRate::FPS_24,
    FrameRate::FPS_25,
    FrameRate::FPS_29_97,
    FrameRate::FPS_30,
    FrameRate::FPS_50,
    FrameRate::FPS_59_94,
    FrameRate::FPS_60,
];

const ANGLES: [&str; 3] = ["Yaw", "Pitch", "Roll"];

/// A number field of the Inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// One of an orientation key's yaw, pitch and roll (0, 1, 2).
    Angle(KeyId, usize),
    /// An aim key's roll.
    AimRoll(KeyId),
    Fov(KeyId),
    Morph(KeyId),
    /// A key's frame.
    Frame(KeyId),
    Spin(KeyId),
    /// One of a 360 output's front's yaw, pitch and roll.
    Front(usize),
}

/// A number being changed, and its value so far.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Editing {
    pub field: Field,
    pub value: f64,
}

/// The Inspector's own state.
#[derive(Debug, Default)]
pub struct Inspector {
    pub editing: Option<Editing>,
    /// A target's name being changed.
    rename: Option<(TargetId, String)>,
    /// What a projection key that isn't a morph morphs to, once its
    /// morph is set above 0.
    morph_to: Option<Projection>,
}

enum Action {
    Edit(&'static str, Vec<Edit>),
    Editing(Option<Editing>),
    Preview(Option<spherecut_core::project::ViewAnim>),
    Front(Option<Orientation>),
    Rename(Option<(TargetId, String)>),
    MorphTo(Projection),
    Seek(Time),
}

/// A field's value as an undo step: its label and edits.
type Commit = dyn Fn(&Doc, f64) -> (&'static str, Vec<Edit>);

/// What the fields draw and do, gathered while the project is borrowed.
struct Form<'a> {
    doc: &'a Doc,
    editing: Option<Editing>,
    /// The field being changed was drawn this frame.
    drawn: bool,
    actions: Vec<Action>,
}

impl Form<'_> {
    /// A number field labelled `label`, showing `value` (or the change
    /// under way), enabled or not. A change shows at once through
    /// `preview` and is kept by `commit` when the field lets go.
    #[expect(
        clippy::too_many_arguments,
        reason = "one field, its range and what it does"
    )]
    fn number(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        field: Field,
        value: f64,
        enabled: bool,
        range: std::ops::RangeInclusive<f64>,
        suffix: &str,
        commit: &Commit,
        preview: &dyn Fn(&Doc, f64) -> Option<Action>,
    ) {
        let l = ui.label(label);
        let ours = self.editing.filter(|e| e.field == field);
        let mut v = ours.map_or(value, |e| e.value);
        let decimals = if matches!(field, Field::Frame(_) | Field::Spin(_)) {
            0
        } else {
            3
        };
        let speed = match field {
            Field::Morph(_) => 0.005,
            Field::Frame(_) | Field::Spin(_) => 0.1,
            _ => 0.25,
        };
        let r = ui
            .add_enabled(
                enabled,
                egui::DragValue::new(&mut v)
                    .range(range)
                    .speed(speed)
                    .suffix(suffix)
                    .max_decimals(decimals),
            )
            .labelled_by(l.id);
        ui.end_row();
        if ours.is_some() {
            self.drawn = true;
        }
        let active = r.dragged() || r.has_focus();
        if r.changed() {
            self.actions
                .push(Action::Editing(Some(Editing { field, value: v })));
            self.drawn = true;
            if let Some(a) = preview(self.doc, v) {
                self.actions.push(a);
            }
        } else if let Some(e) = ours
            && !active
        {
            self.commit(e, commit);
        }
    }

    fn commit(&mut self, e: Editing, commit: &Commit) {
        let (label, edits) = commit(self.doc, e.value);
        self.actions.push(Action::Editing(None));
        self.actions.push(Action::Preview(None));
        self.actions.push(Action::Front(None));
        self.actions.push(Action::Edit(label, edits));
    }
}

/// The Inspector pane.
pub fn inspector_ui(s: &mut AppState, ui: &mut egui::Ui) {
    let t = s.frame_time();
    let centre = s.view.q;
    let look = s.reframe.look;
    let flat = s.reframe.flat_output().is_some();
    let ins = &s.reframe.inspector;
    let (editing, rename, morph_to) = (ins.editing, ins.rename.clone(), ins.morph_to);
    let mut actions = Vec::new();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            let Some(doc) = s.reframe.doc.as_ref() else {
                ui.weak("No project: open a video or a project from the Files pane");
                return;
            };
            let mut form = Form {
                doc,
                editing,
                drawn: false,
                actions: Vec::new(),
            };
            ui.strong(format!("View at {}", format_time(t)));
            if flat {
                view_section(ui, &mut form, t, look, morph_to);
            } else {
                ui.weak("A 360 output: the whole sphere is exported, turned to its front (below)");
            }
            ui.separator();
            keys_section(ui, &mut form);
            ui.separator();
            trim_section(ui, doc);
            ui.separator();
            sequence_section(ui, &mut form);
            ui.separator();
            targets_section(ui, &mut form, t, centre, rename);
            // A field that went away mid-change (the playhead left its
            // key, the selection changed) is let go, its key unchanged.
            if form.editing.is_some() && !form.drawn {
                form.actions.push(Action::Editing(None));
                form.actions.push(Action::Preview(None));
                form.actions.push(Action::Front(None));
            }
            actions = form.actions;
        });
    apply(s, actions);
}

fn view_section(
    ui: &mut egui::Ui,
    form: &mut Form<'_>,
    t: Time,
    look: Look,
    morph_to: Option<Projection>,
) {
    let doc = form.doc;
    if look == Look::Around {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "The viewer shows a look-around, not these keys: Alt+K keys it",
        );
    }
    let view = doc.view_at(t);
    egui::Grid::new("inspector-view")
        .num_columns(2)
        .show(ui, |ui| {
            // Orientation: the key's own yaw, pitch and roll (section 6.4),
            // or the view's between keys.
            let key = doc.editable(Lane::Orientation, t);
            match key.and_then(|k| doc.orientation_key(k.id)) {
                Some(k) if matches!(k.value, Orient::Aim { .. }) => {
                    let Orient::Aim { target, roll_deg } = k.value else {
                        return;
                    };
                    let id = k.id;
                    ui.label("Aims at");
                    ui.label(
                        doc.project
                            .target(target)
                            .map_or("a target that isn't there", |t| t.name.as_str()),
                    );
                    ui.end_row();
                    form.number(
                        ui,
                        "Roll",
                        Field::AimRoll(id),
                        roll_deg,
                        true,
                        -180.0..=180.0,
                        "°",
                        &move |d, v| ("Set roll", aim_roll(d, id, target, v)),
                        &move |d, v| {
                            let value = AnyValue::Orientation(Orient::Aim {
                                target,
                                roll_deg: v,
                            });
                            Some(Action::Preview(Some(d.with_value(id, &value, &[]))))
                        },
                    );
                }
                Some(k) => {
                    let Orient::View(o) = k.value else {
                        return;
                    };
                    let id = k.id;
                    let y = o.ypr();
                    for (i, (name, a)) in
                        ANGLES.into_iter().zip([y.yaw, y.pitch, y.roll]).enumerate()
                    {
                        let range = if i == 1 { -90.0..=90.0 } else { -180.0..=180.0 };
                        form.number(
                            ui,
                            name,
                            Field::Angle(id, i),
                            a.to_degrees(),
                            true,
                            range,
                            "°",
                            &move |d, v| (angle_label(i), angle_edit(d, id, i, v)),
                            &move |d, v| {
                                let o = d.orientation_key(id).and_then(|k| match k.value {
                                    Orient::View(o) => doc::with_angle(o, i, v),
                                    Orient::Aim { .. } => None,
                                })?;
                                let value = AnyValue::Orientation(Orient::View(o));
                                Some(Action::Preview(Some(d.with_value(id, &value, &[]))))
                            },
                        );
                    }
                }
                None => {
                    let y = Ypr::from_quat(view.orientation);
                    for (name, a) in ANGLES.into_iter().zip([y.yaw, y.pitch, y.roll]) {
                        ui.label(name);
                        ui.label(format!("{:.3}°", a.to_degrees()));
                        ui.end_row();
                    }
                }
            }
            match doc.editable(Lane::Fov, t).and_then(|k| doc.fov_key(k.id)) {
                Some(k) => {
                    let id = k.id;
                    form.number(
                        ui,
                        "FOV",
                        Field::Fov(id),
                        k.value,
                        true,
                        MIN_HFOV_DEG..=MAX_FOV_DEG,
                        "°",
                        &move |d, v| ("Set FOV", fov_edit(d, id, v)),
                        &move |d, v| {
                            let value = AnyValue::Fov(v.clamp(MIN_HFOV_DEG, MAX_FOV_DEG));
                            Some(Action::Preview(Some(d.with_value(id, &value, &[]))))
                        },
                    );
                }
                None => {
                    ui.label("FOV");
                    ui.label(format!("{:.3}°", view.lens.hfov.to_degrees()));
                    ui.end_row();
                }
            }
            projection_rows(ui, form, t, &view, morph_to);
        });
    if !Lane::ALL.iter().all(|&l| doc.editable(l, t).is_some()) {
        ui.weak("Not on a key: Alt+K keys the view here");
    }
}

fn projection_rows(
    ui: &mut egui::Ui,
    form: &mut Form<'_>,
    t: Time,
    view: &spherecut_core::project::View,
    morph_to: Option<Projection>,
) {
    let doc = form.doc;
    let key = doc
        .editable(Lane::Projection, t)
        .and_then(|k| doc.projection_key(k.id));
    let shape = key.map_or(view.lens.shape(), |k| k.value);
    let (from, to, morph) = match shape {
        LensShape::Pure(p) => (p, morph_to.filter(|&m| m != p).unwrap_or(next(p)), 0.0),
        LensShape::Morph(m) => (m.from, m.to, m.morph),
    };
    let enabled = key.is_some();
    let id = key.map(|k| k.id);
    let set = |from: Projection, to: Projection, morph: f64| {
        if morph <= 0.0 {
            LensShape::Pure(from)
        } else {
            LensShape::Morph(Morph { from, to, morph })
        }
    };
    let l = ui.label("Projection");
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt("inspector-projection")
            .selected_text(crate::view::projection_label(from))
            .show_ui(ui, |ui| {
                for p in PROJECTIONS {
                    if ui
                        .selectable_label(p == from, crate::view::projection_label(p))
                        .clicked()
                        && let Some(id) = id
                    {
                        let to = if to == p { next(p) } else { to };
                        let value = AnyValue::Projection(set(p, to, morph));
                        form.actions.push(Action::Edit(
                            "Set projection",
                            doc.set_value(id, value, &[]),
                        ));
                    }
                }
            })
            .response
            .labelled_by(l.id);
    });
    ui.end_row();
    let l = ui.label("Morph to");
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt("inspector-morph-to")
            .selected_text(crate::view::projection_label(to))
            .show_ui(ui, |ui| {
                for p in PROJECTIONS.into_iter().filter(|&p| p != from) {
                    if ui
                        .selectable_label(p == to, crate::view::projection_label(p))
                        .clicked()
                    {
                        form.actions.push(Action::MorphTo(p));
                        if morph > 0.0
                            && let Some(id) = id
                        {
                            let value = AnyValue::Projection(set(from, p, morph));
                            form.actions.push(Action::Edit(
                                "Set projection",
                                doc.set_value(id, value, &[]),
                            ));
                        }
                    }
                }
            })
            .response
            .labelled_by(l.id);
    });
    ui.end_row();
    let id = id.unwrap_or(KeyId::NEW);
    form.number(
        ui,
        "Morph",
        Field::Morph(id),
        morph,
        enabled,
        0.0..=1.0,
        "",
        &move |d, v| {
            let value = AnyValue::Projection(set(from, to, v.clamp(0.0, 1.0)));
            let same = d
                .projection_key(id)
                .is_some_and(|k| AnyValue::Projection(k.value) == value);
            let edits = if same {
                Vec::new()
            } else {
                d.set_value(id, value, &[])
            };
            ("Set morph", edits)
        },
        &move |d, v| {
            let value = AnyValue::Projection(set(from, to, v.clamp(0.0, 1.0)));
            Some(Action::Preview(Some(d.with_value(id, &value, &[]))))
        },
    );
}

const PROJECTIONS: [Projection; 3] = [
    Projection::Rectilinear,
    Projection::Fisheye,
    Projection::Stereographic,
];

fn next(p: Projection) -> Projection {
    match p {
        Projection::Rectilinear => Projection::Fisheye,
        Projection::Fisheye => Projection::Stereographic,
        Projection::Stereographic => Projection::Rectilinear,
    }
}

fn angle_label(i: usize) -> &'static str {
    ["Set yaw", "Set pitch", "Set roll"][i.min(2)]
}

fn angle_edit(d: &Doc, id: KeyId, i: usize, v: f64) -> Vec<Edit> {
    let Some(k) = d.orientation_key(id) else {
        return Vec::new();
    };
    let Orient::View(o) = k.value else {
        return Vec::new();
    };
    let Some(n) = doc::with_angle(o, i, v) else {
        return Vec::new();
    };
    if n == o {
        return Vec::new();
    }
    d.set_value(id, AnyValue::Orientation(Orient::View(n)), &[])
}

fn aim_roll(d: &Doc, id: KeyId, target: TargetId, v: f64) -> Vec<Edit> {
    let value = Orient::Aim {
        target,
        roll_deg: v,
    };
    match d.orientation_key(id) {
        Some(k) if k.value != value => d.set_value(id, AnyValue::Orientation(value), &[]),
        _ => Vec::new(),
    }
}

fn fov_edit(d: &Doc, id: KeyId, v: f64) -> Vec<Edit> {
    let v = v.clamp(MIN_HFOV_DEG, MAX_FOV_DEG);
    match d.fov_key(id) {
        Some(k) if k.value != v => d.set_value(id, AnyValue::Fov(v), &[]),
        _ => Vec::new(),
    }
}

fn keys_section(ui: &mut egui::Ui, form: &mut Form<'_>) {
    let doc = form.doc;
    let selected: Vec<_> = doc
        .keys()
        .into_iter()
        .filter(|k| doc.selection.contains(&k.id))
        .collect();
    match selected.as_slice() {
        [] => {
            ui.strong("Selected keys");
            ui.weak("None: click a key in the strip or the curves");
        }
        [k] => {
            let k = *k;
            ui.strong(format!("{} key at {}", k.lane.label(), format_time(k.at)));
            let rate = doc.rate();
            let frame = rate.time_to_frame(k.at, Rounding::Nearest).unwrap_or(0);
            let last = rate
                .time_to_frame(doc.duration(), Rounding::Floor)
                .unwrap_or(0)
                .max(0);
            let id = k.id;
            egui::Grid::new("inspector-key")
                .num_columns(2)
                .show(ui, |ui| {
                    form.number(
                        ui,
                        "Frame",
                        Field::Frame(id),
                        frame as f64,
                        true,
                        0.0..=last as f64,
                        "",
                        &move |d, v| {
                            let at = d
                                .rate()
                                .frame_to_time(v.round() as i64)
                                .unwrap_or(Time::ZERO);
                            ("Move key", d.set_time(id, at))
                        },
                        &|_, _| None,
                    );
                    let l = ui.label("Ease");
                    egui::ComboBox::from_id_salt("inspector-ease")
                        .selected_text(ease_name(k.ease))
                        .show_ui(ui, |ui| {
                            for (name, ease) in EASES {
                                if ui.selectable_label(k.ease == ease, name).clicked() {
                                    let keys = std::collections::BTreeSet::from([id]);
                                    form.actions
                                        .push(Action::Edit("Set ease", doc.set_ease(&keys, ease)));
                                }
                            }
                        })
                        .response
                        .labelled_by(l.id);
                    ui.end_row();
                    if let Some(o) = doc.orientation_key(id) {
                        form.number(
                            ui,
                            "Spin",
                            Field::Spin(id),
                            f64::from(o.spin),
                            true,
                            -8.0..=8.0,
                            " turns",
                            &move |d, v| ("Set spin", d.set_spin(id, v.round() as i8)),
                            &|_, _| None,
                        );
                        aim_row(ui, form, id, o.value);
                    }
                });
        }
        many => {
            ui.strong(format!("{} keys selected", many.len()));
            ui.horizontal(|ui| {
                ui.label("Ease");
                for (name, ease) in EASES {
                    if ui.small_button(name).clicked() {
                        form.actions
                            .push(Action::Edit("Set ease", doc.set_ease(&doc.selection, ease)));
                    }
                }
            });
        }
    }
}

fn ease_name(e: spherecut_core::project::Ease) -> &'static str {
    EASES
        .iter()
        .find(|(_, x)| *x == e)
        .map_or("Custom", |(n, _)| *n)
}

/// What an orientation key looks at: a direction, or a target.
fn aim_row(ui: &mut egui::Ui, form: &mut Form<'_>, id: KeyId, value: Orient) {
    let doc = form.doc;
    let now = match value {
        Orient::Aim { target, .. } => Some(target),
        Orient::View(_) => None,
    };
    let name = |t: Option<TargetId>| match t {
        None => "A direction".to_owned(),
        Some(t) => doc
            .project
            .target(t)
            .map_or_else(|| "A missing target".to_owned(), |t| t.name.clone()),
    };
    let l = ui.label("Looks at");
    egui::ComboBox::from_id_salt("inspector-aim")
        .selected_text(name(now))
        .show_ui(ui, |ui| {
            let choices = std::iter::once(None).chain(
                doc.project
                    .targets()
                    .iter()
                    .filter(|t| t.media == doc.clip().media)
                    .map(|t| Some(t.id)),
            );
            for c in choices {
                if ui.selectable_label(c == now, name(c)).clicked() {
                    form.actions
                        .push(Action::Edit("Aim the key", doc.aim(id, c)));
                }
            }
        })
        .response
        .labelled_by(l.id);
    ui.end_row();
}

fn trim_section(ui: &mut egui::Ui, doc: &Doc) {
    ui.strong("Trim");
    let r = doc.clip().source;
    let rate = doc.rate();
    let frames = |t: Time| rate.time_to_frame(t, Rounding::Nearest).unwrap_or(0);
    egui::Grid::new("inspector-trim")
        .num_columns(2)
        .show(ui, |ui| {
            for (name, at) in [("In", r.start), ("Out", r.end)] {
                ui.label(name);
                ui.label(format!("{} (frame {})", format_time(at), frames(at)));
                ui.end_row();
            }
            let length = r.end.checked_sub(r.start).unwrap_or(Time::ZERO);
            ui.label("Duration");
            ui.label(format!(
                "{} ({} frames)",
                format_time(length),
                frames(r.end) - frames(r.start)
            ));
            ui.end_row();
        });
}

/// An output's name in the Inspector and the export window: its shape
/// and size.
pub fn output_label(p: Preset) -> String {
    let (w, h) = p.size();
    match p {
        Preset::Flat { aspect, .. } => {
            let (a, b) = aspect.ratio();
            format!("{a}:{b}, {w} × {h}")
        }
        Preset::Sphere { .. } => format!("360, {w} × {h}"),
    }
}

/// The output a preset makes, a 360 one keeping the front it had.
pub fn preset_output(p: Preset, now: &Output) -> Output {
    match p {
        Preset::Flat { aspect, short_side } => Output::Flat { aspect, short_side },
        Preset::Sphere { width } => Output::Equirect {
            width,
            orientation: match now {
                Output::Equirect { orientation, .. } => *orientation,
                Output::Flat { .. } => Orientation::IDENTITY,
            },
        },
    }
}

/// The output's presets in a menu, Flat then 360; a pick is an edit.
pub fn output_menu(ui: &mut egui::Ui, doc: &Doc, id: &str) -> Option<Vec<Edit>> {
    let now = doc.project.the_sequence().output;
    let current = Preset::of(&now);
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current.map_or_else(|| "Not a preset".to_owned(), output_label))
        .show_ui(ui, |ui| {
            ui.weak("Flat");
            for p in Preset::all() {
                if matches!(p, Preset::Sphere { .. }) {
                    continue;
                }
                if ui
                    .selectable_label(current == Some(p), output_label(p))
                    .clicked()
                {
                    picked = Some(doc.set_output(preset_output(p, &now)));
                }
            }
            ui.separator();
            ui.weak("360");
            for p in Preset::all() {
                if matches!(p, Preset::Flat { .. }) {
                    continue;
                }
                if ui
                    .selectable_label(current == Some(p), output_label(p))
                    .clicked()
                {
                    picked = Some(doc.set_output(preset_output(p, &now)));
                }
            }
        });
    picked
}

fn sequence_section(ui: &mut egui::Ui, form: &mut Form<'_>) {
    let doc = form.doc;
    ui.strong("Sequence");
    egui::Grid::new("inspector-sequence")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Output");
            if let Some(edits) = output_menu(ui, doc, "inspector-output") {
                form.actions.push(Action::Edit("Set output", edits));
            }
            ui.end_row();
            if let Output::Equirect { orientation, .. } = doc.output() {
                let y = orientation.ypr();
                for (i, (name, a)) in ANGLES.into_iter().zip([y.yaw, y.pitch, y.roll]).enumerate() {
                    let range = if i == 1 { -90.0..=90.0 } else { -180.0..=180.0 };
                    form.number(
                        ui,
                        &format!("Front {}", name.to_lowercase()),
                        Field::Front(i),
                        a.to_degrees(),
                        true,
                        range,
                        "°",
                        &move |d, v| ("Turn the front", front_edit(d, i, v)),
                        &move |d, v| {
                            let Output::Equirect { orientation, .. } =
                                d.project.the_sequence().output
                            else {
                                return None;
                            };
                            doc::with_angle(orientation, i, v).map(|o| Action::Front(Some(o)))
                        },
                    );
                }
            }
            let rate = doc.rate();
            let own = doc.media().map(|m| m.video.rate);
            let l = ui.label("Rate");
            egui::ComboBox::from_id_salt("inspector-rate")
                .selected_text(rate_label(rate, own))
                .show_ui(ui, |ui| {
                    let mut rates: Vec<FrameRate> = own.into_iter().chain(RATES).collect();
                    rates.dedup();
                    let mut seen = Vec::new();
                    for r in rates {
                        if seen.contains(&r) {
                            continue;
                        }
                        seen.push(r);
                        if ui.selectable_label(r == rate, rate_label(r, own)).clicked() {
                            form.actions.push(Action::Edit("Set rate", doc.set_rate(r)));
                        }
                    }
                })
                .response
                .labelled_by(l.id);
            ui.end_row();
        });
}

fn front_edit(d: &Doc, i: usize, v: f64) -> Vec<Edit> {
    let Output::Equirect { orientation, .. } = d.project.the_sequence().output else {
        return Vec::new();
    };
    match doc::with_angle(orientation, i, v) {
        Some(o) if o != orientation => d.set_front(o),
        _ => Vec::new(),
    }
}

/// A rate as people write it, the video's own marked.
pub fn rate_label(r: FrameRate, own: Option<FrameRate>) -> String {
    let fps = r.as_rational().to_f64();
    let text = if (fps - fps.round()).abs() < 1e-9 {
        format!("{fps:.0} fps")
    } else {
        format!("{fps:.3} fps")
    };
    if own == Some(r) {
        format!("{text} (the video's)")
    } else {
        text
    }
}

fn targets_section(
    ui: &mut egui::Ui,
    form: &mut Form<'_>,
    t: Time,
    centre: glam::DQuat,
    rename: Option<(TargetId, String)>,
) {
    let doc = form.doc;
    ui.strong("Targets");
    let targets: Vec<&Target> = doc
        .project
        .targets()
        .iter()
        .filter(|tg| tg.media == doc.clip().media)
        .collect();
    if targets.is_empty() {
        ui.weak("None: a target is something an orientation key can follow");
    }
    for tg in targets {
        ui.horizontal(|ui| {
            match &rename {
                Some((id, name)) if *id == tg.id => {
                    let mut name = name.clone();
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .id(egui::Id::unique(("inspector-target-name", tg.id)))
                            .desired_width(120.0),
                    );
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    if r.lost_focus() {
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let name = name.trim();
                        if enter && !name.is_empty() {
                            form.actions.push(Action::Edit(
                                "Rename target",
                                doc.rename_target(tg.id, name),
                            ));
                        }
                        form.actions.push(Action::Rename(None));
                    } else {
                        form.actions.push(Action::Rename(Some((tg.id, name))));
                    }
                }
                _ => {
                    let a = tg.at(t.to_secs_f64());
                    ui.label(&tg.name).on_hover_text(format!(
                        "at yaw {:.1}°, pitch {:.1}°; {} key(s)",
                        a.yaw.to_degrees(),
                        a.pitch.to_degrees(),
                        tg.path.keys.len()
                    ));
                    if ui.small_button("Rename").clicked() {
                        form.actions
                            .push(Action::Rename(Some((tg.id, tg.name.clone()))));
                    }
                }
            }
            if ui
                .small_button("Key here")
                .on_hover_text("The target where the view looks, at the playhead")
                .clicked()
            {
                form.actions.push(Action::Edit(
                    "Key the target",
                    doc.key_target(tg.id, t, doc::aim_dir(centre)),
                ));
            }
            if ui.small_button("Remove").clicked() {
                form.actions.push(Action::Edit(
                    "Remove target",
                    vec![Edit::RemoveTarget { target: tg.id }],
                ));
            }
        });
        if let Some(k) = tg.path.keys.first()
            && tg.path.keys.len() > 1
        {
            let first = k.at;
            ui.horizontal(|ui| {
                ui.weak(format!("moves: {} keys from", tg.path.keys.len()));
                if ui.small_button(format_time(first)).clicked() {
                    form.actions.push(Action::Seek(first));
                }
            });
        }
    }
    if ui.button("Add a target where the view looks").clicked() {
        form.actions.push(Action::Edit(
            "Add target",
            doc.add_target(t, doc::aim_dir(centre)),
        ));
    }
}

fn apply(s: &mut AppState, actions: Vec<Action>) {
    for a in actions {
        match a {
            Action::Edit(label, edits) => {
                s.edit(label, &edits);
            }
            Action::Editing(e) => s.reframe.inspector.editing = e,
            Action::Preview(p) => {
                if let Some(doc) = &mut s.reframe.doc {
                    doc.preview = p;
                }
            }
            Action::Front(f) => {
                if let Some(doc) = &mut s.reframe.doc {
                    doc.front_preview = f;
                }
            }
            Action::Rename(r) => s.reframe.inspector.rename = r,
            Action::MorphTo(p) => s.reframe.inspector.morph_to = Some(p),
            Action::Seek(t) => s.seek(t, SeekMode::Exact),
        }
    }
}
