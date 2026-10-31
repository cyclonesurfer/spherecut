//! The open project (M2 plan section 10; amendment 2 items 20, 39 and
//! 53): the project and its undo history, the file it came from or was
//! saved to, the keys selected, a drag's preview, and the edits the
//! reframer's gestures make of them. No egui here: the timeline strip,
//! the curves, the Inspector and the viewer call it, and its tests need
//! no window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use glam::DQuat;
use spherecut_core::project::{
    AimDir, AnyKey, AnyKeys, AnyValue, Clip, Ease, Edit, EditError, ExportSettings, History,
    InitialView, Key, KeyId, KeyTrack, Marker, MarkerId, Media, Orient, Orientation, Origin,
    Output, Problem, Project, Target, TargetId, View, ViewAnim,
};
use spherecut_core::{FrameRate, Rational, Rounding, Time};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, LensShape, MIN_HFOV_DEG, Projection};

/// The widest field of view a key holds, in degrees (D50: the renderer
/// caps each projection at its own).
pub const MAX_FOV_DEG: f64 = 360.0;

/// A view's three key tracks, as the strip's lanes show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    Orientation,
    Fov,
    Projection,
}

impl Lane {
    pub const ALL: [Lane; 3] = [Lane::Orientation, Lane::Fov, Lane::Projection];

    pub fn label(self) -> &'static str {
        match self {
            Lane::Orientation => "Orientation",
            Lane::Fov => "FOV",
            Lane::Projection => "Projection",
        }
    }

    /// A new key's ease, as the untitled project's keys have it.
    pub fn default_ease(self) -> Ease {
        match self {
            Lane::Orientation | Lane::Fov => Ease::SMOOTH,
            Lane::Projection => Ease::STOP,
        }
    }
}

/// A key as the strip draws it: its lane, id, time and ease.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyMark {
    pub lane: Lane,
    pub id: KeyId,
    pub at: Time,
    pub ease: Ease,
    /// An orientation key that aims at a target (M2.7's).
    pub aim: bool,
}

/// What the viewer shows, as the three tracks hold it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewValues {
    pub orientation: DQuat,
    /// Horizontal, of the output frame, in degrees.
    pub fov_deg: f64,
    pub shape: LensShape,
}

impl ViewValues {
    pub fn of(orientation: DQuat, lens: &Lens) -> ViewValues {
        ViewValues {
            orientation,
            fov_deg: lens.hfov.to_degrees(),
            shape: lens.shape(),
        }
    }

    /// The values the three lanes take apart, past rounding.
    pub fn changed(&self, other: &ViewValues) -> Vec<Lane> {
        let mut lanes = Vec::new();
        if spherecut_sphere::rotation_angle(self.orientation, other.orientation) > 1e-7 {
            lanes.push(Lane::Orientation);
        }
        if (self.fov_deg - other.fov_deg).abs() > 1e-6 {
            lanes.push(Lane::Fov);
        }
        let (a, b) = (self.shape.perimeter(), other.shape.perimeter());
        if spherecut_sphere::spline::wrap_period(a - b, LensShape::PERIMETER).abs() > 1e-9 {
            lanes.push(Lane::Projection);
        }
        lanes
    }

    fn value(&self, lane: Lane) -> Result<AnyValue, Problem> {
        Ok(match lane {
            Lane::Orientation => {
                let o = Orientation::new(self.orientation).map_err(|e| {
                    Problem::new(spherecut_core::project::Code::Invalid, e.to_string())
                })?;
                AnyValue::Orientation(Orient::View(o))
            }
            Lane::Fov => AnyValue::Fov(self.fov_deg.clamp(MIN_HFOV_DEG, MAX_FOV_DEG)),
            Lane::Projection => AnyValue::Projection(self.shape),
        })
    }
}

/// The viewer's own default view, forward and level through a 100°
/// rectilinear lens (an untitled project starts with the view on screen).
pub const INITIAL_VIEW: InitialView = InitialView {
    orientation: Orientation::IDENTITY,
    fov_deg: 100.0,
    projection: Projection::Rectilinear,
};

/// The open project.
#[derive(Debug)]
pub struct Doc {
    pub project: Project,
    pub history: History,
    /// The file it was loaded from or last saved to; none while untitled.
    pub path: Option<PathBuf>,
    /// The revision that file holds.
    saved: Option<u64>,
    /// The keys selected, one or more, in one lane or across lanes.
    pub selection: BTreeSet<KeyId>,
    /// The clip's view with a drag's change in it, shown until the drag
    /// ends (amendment 2 item 39): the project changes, through `apply`
    /// and its checks, only then. Any edit, undo or redo drops it.
    pub preview: Option<ViewAnim>,
    /// A 360 output's front being changed, shown until the change ends.
    pub front_preview: Option<Orientation>,
}

impl Doc {
    /// A project loaded from `path`, or made untitled (no path).
    pub fn new(project: Project, path: Option<PathBuf>) -> Doc {
        let saved = path.as_ref().map(|_| project.revision());
        Doc {
            project,
            history: History::new(),
            path,
            saved,
            selection: BTreeSet::new(),
            preview: None,
            front_preview: None,
        }
    }

    /// The untitled project opening `media` makes.
    pub fn untitled(media: Media, view: InitialView) -> Result<Doc, Problem> {
        Ok(Doc::new(Project::untitled(media, view)?, None))
    }

    /// Changed since it was saved or made.
    pub fn dirty(&self) -> bool {
        match self.saved {
            Some(r) => r != self.project.revision(),
            None => self.project.revision() != 0,
        }
    }

    /// Saved to `path` at `revision`.
    pub fn saved_to(&mut self, path: PathBuf, revision: u64) {
        self.path = Some(path);
        self.saved = Some(revision);
    }

    /// The name the window and the Save window say: the file's, or the
    /// sequence's while untitled.
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{} (untitled)", self.project.the_sequence().name))
    }

    pub fn clip(&self) -> &Clip {
        self.project.the_clip()
    }

    /// The media file the clip shows.
    pub fn media(&self) -> Option<&Media> {
        self.project.media_by_id(self.clip().media)
    }

    /// The sequence's frame rate: what keys and trims snap to.
    pub fn rate(&self) -> FrameRate {
        self.project.the_sequence().rate
    }

    /// The source's length.
    pub fn duration(&self) -> Time {
        self.media().map_or(Time::ZERO, |m| m.video.duration)
    }

    /// Half a frame: how near a key is to be the playhead's.
    pub fn half_frame(&self) -> Time {
        let d = self.rate().frame_duration();
        Time::seconds(
            d.as_rational()
                .checked_div(Rational::from_integer(2))
                .unwrap_or(Rational::ZERO),
        )
    }

    /// `t` on the nearest frame's start, within the source.
    pub fn snap(&self, t: Time) -> Time {
        let rate = self.rate();
        let snapped = rate
            .time_to_frame(t, Rounding::Nearest)
            .and_then(|n| rate.frame_to_time(n))
            .unwrap_or(t);
        snapped.clamp(Time::ZERO, self.duration())
    }

    /// One undo step of `edits`; the selection keeps only keys still
    /// there. On error nothing changes.
    pub fn record(&mut self, label: &str, edits: &[Edit]) -> Result<(), EditError> {
        if edits.is_empty() {
            return Ok(());
        }
        self.history
            .record(&mut self.project, label, Origin::User, edits)?;
        self.end_preview();
        self.prune_selection();
        Ok(())
    }

    /// The drag's previews dropped: the project's own view is shown.
    pub fn end_preview(&mut self) {
        self.preview = None;
        self.front_preview = None;
    }

    /// Undo, giving the step's label.
    pub fn undo(&mut self) -> Result<Option<String>, EditError> {
        let label = self
            .history
            .undo(&mut self.project)?
            .map(|s| s.label.clone());
        self.end_preview();
        self.prune_selection();
        Ok(label)
    }

    /// Redo, giving the step's label.
    pub fn redo(&mut self) -> Result<Option<String>, EditError> {
        let label = self
            .history
            .redo(&mut self.project)?
            .map(|s| s.label.clone());
        self.end_preview();
        self.prune_selection();
        Ok(label)
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo_step().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.history.redo_step().map(|s| s.label.as_str())
    }

    fn prune_selection(&mut self) {
        let there: BTreeSet<KeyId> = self.keys().iter().map(|k| k.id).collect();
        self.selection.retain(|k| there.contains(k));
    }

    /// Every key of the clip's view, lane by lane, each in time order.
    pub fn keys(&self) -> Vec<KeyMark> {
        let v = &self.clip().view;
        let mut out = Vec::new();
        for k in &v.orientation.keys {
            out.push(KeyMark {
                lane: Lane::Orientation,
                id: k.id,
                at: k.at,
                ease: k.ease,
                aim: matches!(k.value, Orient::Aim { .. }),
            });
        }
        out.extend(v.fov.keys.iter().map(|k| mark(Lane::Fov, k)));
        out.extend(v.projection.keys.iter().map(|k| mark(Lane::Projection, k)));
        out
    }

    /// `lane`'s keys.
    pub fn lane_keys(&self, lane: Lane) -> Vec<KeyMark> {
        self.keys().into_iter().filter(|k| k.lane == lane).collect()
    }

    /// The key on `lane` within half a frame of `t`, the nearest.
    pub fn key_at(&self, lane: Lane, t: Time) -> Option<KeyMark> {
        let half = self.half_frame().to_secs_f64();
        let t = t.to_secs_f64();
        self.lane_keys(lane)
            .into_iter()
            .map(|k| ((k.at.to_secs_f64() - t).abs(), k))
            .filter(|(d, _)| *d <= half + 1e-12)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, k)| k)
    }

    /// The key a change of `lane`'s value at `t` edits (section 10.2):
    /// the one at the playhead, or the lane's only key. None makes the
    /// change a look-around.
    pub fn editable(&self, lane: Lane, t: Time) -> Option<KeyMark> {
        let at = self.key_at(lane, t);
        if at.is_some() {
            return at;
        }
        match self.lane_keys(lane).as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Whether any lane has a key at the playhead.
    pub fn on_key(&self, t: Time) -> bool {
        Lane::ALL.iter().any(|&l| self.key_at(l, t).is_some())
    }

    /// The clip's view as shown: a drag's preview, else the project's.
    pub fn anim(&self) -> &ViewAnim {
        self.preview.as_ref().unwrap_or(&self.clip().view)
    }

    /// The keyed view at `t`, source time, as shown.
    pub fn view_at(&self, t: Time) -> View {
        self.anim().at(t.to_secs_f64(), self.project.targets())
    }

    /// The sequence's output as shown: a 360 output with its front
    /// being changed has the change's.
    pub fn output(&self) -> Output {
        match (self.project.the_sequence().output, self.front_preview) {
            (Output::Equirect { width, .. }, Some(orientation)) => {
                Output::Equirect { width, orientation }
            }
            (out, _) => out,
        }
    }

    /// The key with `id` on whichever lane has it.
    pub fn key(&self, id: KeyId) -> Option<KeyMark> {
        self.keys().into_iter().find(|k| k.id == id)
    }

    pub fn orientation_key(&self, id: KeyId) -> Option<&Key<Orient>> {
        self.clip().view.orientation.key(id)
    }

    pub fn fov_key(&self, id: KeyId) -> Option<&Key<f64>> {
        self.clip().view.fov.key(id)
    }

    pub fn projection_key(&self, id: KeyId) -> Option<&Key<LensShape>> {
        self.clip().view.projection.key(id)
    }

    /// The clip's view with `key`'s value set to `value`, and the
    /// orientation keys in `spins` with those spins: a drag's preview.
    /// A value of the wrong lane changes nothing.
    pub fn with_value(&self, key: KeyId, value: &AnyValue, spins: &[(KeyId, i8)]) -> ViewAnim {
        let mut a = self.clip().view.clone();
        match *value {
            AnyValue::Orientation(o) => {
                if let Some(k) = a.orientation.keys.iter_mut().find(|k| k.id == key) {
                    k.value = o;
                }
            }
            AnyValue::Fov(f) => {
                if let Some(k) = a.fov.keys.iter_mut().find(|k| k.id == key) {
                    k.value = f;
                }
            }
            AnyValue::Projection(p) => {
                if let Some(k) = a.projection.keys.iter_mut().find(|k| k.id == key) {
                    k.value = p;
                }
            }
        }
        for &(id, spin) in spins {
            if let Some(k) = a.orientation.keys.iter_mut().find(|k| k.id == id) {
                k.spin = spin;
            }
        }
        a
    }

    /// `key`'s value set to `value`, and the spins in `spins` set where
    /// they differ from the project's (section 6.4's sticky spin): one
    /// step's edits.
    pub fn set_value(&self, key: KeyId, value: AnyValue, spins: &[(KeyId, i8)]) -> Vec<Edit> {
        let mut edits = vec![Edit::SetKey {
            key,
            at: None,
            value: Some(value),
            ease: None,
            spin: None,
        }];
        for &(id, spin) in spins {
            let now = self.orientation_key(id).map(|k| k.spin);
            if now.is_some_and(|s| s != spin) {
                match edits
                    .iter_mut()
                    .find(|e| matches!(e, Edit::SetKey { key, .. } if *key == id))
                {
                    Some(Edit::SetKey { spin: s, .. }) => *s = Some(spin),
                    _ => edits.push(Edit::SetKey {
                        key: id,
                        at: None,
                        value: None,
                        ease: None,
                        spin: Some(spin),
                    }),
                }
            }
        }
        edits
    }

    /// `key`'s spin: extra whole turns from it to the next key.
    pub fn set_spin(&self, key: KeyId, spin: i8) -> Vec<Edit> {
        match self.orientation_key(key) {
            Some(k) if k.spin != spin => vec![Edit::SetKey {
                key,
                at: None,
                value: None,
                ease: None,
                spin: Some(spin),
            }],
            _ => Vec::new(),
        }
    }

    /// The selected key moved to `at` (snapped to a frame by the caller).
    pub fn set_time(&self, key: KeyId, at: Time) -> Vec<Edit> {
        match self.key(key) {
            Some(k) if k.at != at => vec![Edit::SetKey {
                key,
                at: Some(at),
                value: None,
                ease: None,
                spin: None,
            }],
            _ => Vec::new(),
        }
    }

    /// An orientation key aimed at `target`, keeping its roll, or with
    /// none turned back into the direction it looks in at its time.
    pub fn aim(&self, key: KeyId, target: Option<TargetId>) -> Vec<Edit> {
        let Some(k) = self.orientation_key(key) else {
            return Vec::new();
        };
        let value = match (target, k.value) {
            (Some(t), Orient::Aim { target, .. }) if t == target => return Vec::new(),
            (Some(target), Orient::Aim { roll_deg, .. }) => Orient::Aim { target, roll_deg },
            (Some(target), Orient::View(o)) => Orient::Aim {
                target,
                roll_deg: o.ypr().roll.to_degrees(),
            },
            (None, Orient::View(_)) => return Vec::new(),
            (None, Orient::Aim { .. }) => {
                let q = self
                    .clip()
                    .view
                    .orientation_at(k.at.to_secs_f64(), self.project.targets());
                match Orientation::new(q) {
                    Ok(o) => Orient::View(o),
                    Err(_) => return Vec::new(),
                }
            }
        };
        vec![Edit::SetKey {
            key,
            at: None,
            value: Some(AnyValue::Orientation(value)),
            ease: None,
            spin: None,
        }]
    }

    /// A target at `dir`, fixed, in the clip's media, named by its
    /// number.
    pub fn add_target(&self, t: Time, dir: AimDir) -> Vec<Edit> {
        let n = self.project.targets().len() + 1;
        vec![Edit::AddTarget {
            target: Target {
                id: TargetId::NEW,
                name: format!("Target {n}"),
                media: self.clip().media,
                path: KeyTrack {
                    keys: vec![new_key(t, dir, Ease::SMOOTH)],
                },
            },
        }]
    }

    /// `target` at `dir` at `t`: its path's key within half a frame of
    /// `t` moved there, else a new key, so a target that moves is keyed
    /// as the view is.
    pub fn key_target(&self, target: TargetId, t: Time, dir: AimDir) -> Vec<Edit> {
        let Some(tg) = self.project.target(target) else {
            return Vec::new();
        };
        let mut path = tg.path.clone();
        let half = self.half_frame().to_secs_f64();
        let near = path
            .keys
            .iter_mut()
            .find(|k| (k.at.to_secs_f64() - t.to_secs_f64()).abs() <= half + 1e-12);
        match near {
            Some(k) => k.value = dir,
            None => {
                let i = path.keys.partition_point(|k| k.at < t);
                path.keys.insert(i, new_key(t, dir, Ease::SMOOTH));
            }
        }
        vec![Edit::SetTarget {
            target,
            name: None,
            path: Some(path),
        }]
    }

    pub fn rename_target(&self, target: TargetId, name: &str) -> Vec<Edit> {
        match self.project.target(target) {
            Some(t) if t.name != name => vec![Edit::Rename {
                id: target.0,
                name: name.to_owned(),
            }],
            _ => Vec::new(),
        }
    }

    /// The sequence's output set to `output`. Between flat and 360 the
    /// export's codec follows, when it was the old kind's default.
    pub fn set_output(&self, output: Output) -> Vec<Edit> {
        let seq = self.project.the_sequence();
        if seq.output == output {
            return Vec::new();
        }
        let sequence = seq.id;
        let mut edits = vec![Edit::SetOutput { sequence, output }];
        let kind = |o: &Output| matches!(o, Output::Flat { .. });
        let was = ExportSettings::default_for(&seq.output);
        if kind(&seq.output) != kind(&output) && seq.export.codec == was.codec {
            edits.push(Edit::SetExport {
                sequence,
                export: ExportSettings {
                    codec: ExportSettings::default_for(&output).codec,
                    ..seq.export
                },
            });
        }
        edits
    }

    /// A 360 output's front turned to `orientation`.
    pub fn set_front(&self, orientation: Orientation) -> Vec<Edit> {
        match self.project.the_sequence().output {
            Output::Equirect { width, .. } => {
                self.set_output(Output::Equirect { width, orientation })
            }
            Output::Flat { .. } => Vec::new(),
        }
    }

    pub fn set_rate(&self, rate: FrameRate) -> Vec<Edit> {
        let seq = self.project.the_sequence();
        if seq.rate == rate {
            return Vec::new();
        }
        vec![Edit::SetRate {
            sequence: seq.id,
            rate,
        }]
    }

    /// The export's choices, kept with the sequence.
    pub fn set_export(&self, export: ExportSettings) -> Vec<Edit> {
        let seq = self.project.the_sequence();
        if seq.export == export {
            return Vec::new();
        }
        vec![Edit::SetExport {
            sequence: seq.id,
            export,
        }]
    }

    /// The key times before and after `t`, past half a frame, for
    /// `Alt+[` and `Alt+]`.
    pub fn key_before(&self, t: Time) -> Option<Time> {
        let half = self.half_frame();
        let limit = t.checked_sub(half).ok()?;
        self.keys()
            .into_iter()
            .map(|k| k.at)
            .filter(|&a| a < limit)
            .max()
    }

    pub fn key_after(&self, t: Time) -> Option<Time> {
        let half = self.half_frame();
        let limit = t.checked_add(half).ok()?;
        self.keys()
            .into_iter()
            .map(|k| k.at)
            .filter(|&a| a > limit)
            .min()
    }

    /// A change of the view to `now` at the playhead `t`, as edits of
    /// the keys it touches (section 10.2): `Some` of the edits (none when
    /// nothing changed) when every lane that changed has a key to edit,
    /// `None` when one hasn't and the change is a look-around. An aim
    /// key (M2.7's targets) isn't edited by turning the view.
    pub fn view_edits(&self, t: Time, now: &ViewValues) -> Option<Vec<Edit>> {
        let was = self.view_at(t);
        let changed = ViewValues::of(was.orientation, &was.lens).changed(now);
        let mut edits = Vec::new();
        for lane in changed {
            let k = self.editable(lane, t)?;
            if k.aim {
                return None;
            }
            edits.push(Edit::SetKey {
                key: k.id,
                at: None,
                value: Some(now.value(lane).ok()?),
                ease: None,
                spin: None,
            });
        }
        Some(edits)
    }

    /// `Alt+K`: the view `now` keyed at the playhead `t`, one key on each
    /// lane, the key there changed where there is one (amendment 2 item
    /// 20), a new one at `t` elsewhere.
    pub fn key_view(&self, t: Time, now: &ViewValues) -> Result<Vec<Edit>, Problem> {
        let clip = self.clip().id;
        let mut edits = Vec::new();
        for lane in Lane::ALL {
            let value = now.value(lane)?;
            if let Some(k) = self.key_at(lane, t) {
                edits.push(Edit::SetKey {
                    key: k.id,
                    at: None,
                    value: Some(value),
                    ease: None,
                    spin: None,
                });
                continue;
            }
            let ease = lane.default_ease();
            let key = match value {
                AnyValue::Orientation(v) => AnyKey::Orientation(new_key(t, v, ease)),
                AnyValue::Fov(v) => AnyKey::Fov(new_key(t, v, ease)),
                AnyValue::Projection(v) => AnyKey::Projection(new_key(t, v, ease)),
            };
            edits.push(Edit::AddKey { clip, key });
        }
        Ok(edits)
    }

    /// `I`: the trim's start at `t`.
    pub fn set_in(&self, t: Time) -> Vec<Edit> {
        vec![Edit::SetTrim {
            clip: self.clip().id,
            start: Some(t),
            end: None,
        }]
    }

    /// `O`: the trim's end at `end`, the end of the frame on screen, so
    /// that frame is the last in.
    pub fn set_out(&self, end: Time) -> Vec<Edit> {
        vec![Edit::SetTrim {
            clip: self.clip().id,
            start: None,
            end: Some(end.min(self.duration())),
        }]
    }

    /// `M`: a marker at `t`, named by its number.
    pub fn add_marker(&self, t: Time) -> Vec<Edit> {
        let n = self.clip().markers.len() + 1;
        vec![Edit::AddMarker {
            clip: self.clip().id,
            marker: Marker {
                id: MarkerId::NEW,
                at: t,
                name: format!("Marker {n}"),
            },
        }]
    }

    pub fn move_marker(&self, id: MarkerId, at: Time) -> Vec<Edit> {
        match self.clip().marker(id) {
            Some(m) if m.at != at => vec![Edit::SetMarker {
                marker: id,
                at: Some(at),
                name: None,
            }],
            _ => Vec::new(),
        }
    }

    pub fn rename_marker(&self, id: MarkerId, name: &str) -> Vec<Edit> {
        match self.clip().marker(id) {
            Some(m) if m.name != name => vec![Edit::SetMarker {
                marker: id,
                at: None,
                name: Some(name.to_owned()),
            }],
            _ => Vec::new(),
        }
    }

    /// The selected keys moved by `by`, each lane's track replaced whole
    /// so that keys passing each other stay in order. None when a key
    /// would leave the source or land on another's time.
    pub fn move_keys(&self, keys: &BTreeSet<KeyId>, by: Time) -> Option<Vec<Edit>> {
        if by == Time::ZERO || keys.is_empty() {
            return Some(Vec::new());
        }
        let clip = self.clip().id;
        let end = self.duration();
        let v = &self.clip().view;
        let mut edits = Vec::new();
        if let Some(o) = moved(&v.orientation, keys, by, end) {
            edits.push(Edit::SetKeys {
                clip,
                keys: AnyKeys::Orientation(o?),
            });
        }
        if let Some(f) = moved(&v.fov, keys, by, end) {
            edits.push(Edit::SetKeys {
                clip,
                keys: AnyKeys::Fov(f?),
            });
        }
        if let Some(p) = moved(&v.projection, keys, by, end) {
            edits.push(Edit::SetKeys {
                clip,
                keys: AnyKeys::Projection(p?),
            });
        }
        Some(edits)
    }

    /// `Delete`: the selected keys removed, but for each lane's first
    /// when all of a lane's keys are selected, since a lane keeps one.
    /// The edits and how many keys stayed for that reason.
    pub fn remove_keys(&self, keys: &BTreeSet<KeyId>) -> (Vec<Edit>, usize) {
        let mut edits = Vec::new();
        let mut kept = 0;
        for lane in Lane::ALL {
            let all = self.lane_keys(lane);
            let mut chosen: Vec<KeyId> = all
                .iter()
                .map(|k| k.id)
                .filter(|id| keys.contains(id))
                .collect();
            if !chosen.is_empty() && chosen.len() == all.len() {
                chosen.remove(0);
                kept += 1;
            }
            edits.extend(chosen.into_iter().map(|key| Edit::RemoveKey { key }));
        }
        (edits, kept)
    }

    /// The keys' eases set to `ease`.
    pub fn set_ease(&self, keys: &BTreeSet<KeyId>, ease: Ease) -> Vec<Edit> {
        self.keys()
            .into_iter()
            .filter(|k| keys.contains(&k.id) && k.ease != ease)
            .map(|k| Edit::SetKey {
                key: k.id,
                at: None,
                value: None,
                ease: Some(ease),
                spin: None,
            })
            .collect()
    }
}

fn mark<V>(lane: Lane, k: &Key<V>) -> KeyMark {
    KeyMark {
        lane,
        id: k.id,
        at: k.at,
        ease: k.ease,
        aim: false,
    }
}

/// `o` with one of its yaw, pitch and roll (0, 1, 2) set to `deg`, the
/// rest of its own decomposition kept (section 6.4): pitch within ±90°.
pub fn with_angle(o: Orientation, which: usize, deg: f64) -> Option<Orientation> {
    let mut y = o.ypr();
    let r = deg.to_radians();
    match which {
        0 => y.yaw = r,
        1 => y.pitch = r.clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2),
        _ => y.roll = r,
    }
    Orientation::from_ypr(Ypr::new(y.yaw, y.pitch, y.roll)).ok()
}

/// Where a view looking along `q` looks, as a target's direction: its
/// yaw and pitch in degrees (D33: forward is the centre's).
pub fn aim_dir(q: DQuat) -> AimDir {
    let y = Ypr::from_quat(q);
    AimDir {
        yaw_deg: y.yaw.to_degrees(),
        pitch_deg: y.pitch.to_degrees(),
    }
}

fn new_key<V>(at: Time, value: V, ease: Ease) -> Key<V> {
    Key {
        id: KeyId::NEW,
        at,
        value,
        ease,
        spin: 0,
    }
}

/// `track` with the keys in `ids` moved by `by`: none when none of its
/// keys move, and `Some(None)` when one would leave `[0, end]` or two
/// would share a time.
fn moved<V: Clone>(
    track: &KeyTrack<V>,
    ids: &BTreeSet<KeyId>,
    by: Time,
    end: Time,
) -> Option<Option<Vec<Key<V>>>> {
    if !track.keys.iter().any(|k| ids.contains(&k.id)) {
        return None;
    }
    let shift = |k: &Key<V>| -> Option<Key<V>> {
        let mut k = k.clone();
        if ids.contains(&k.id) {
            k.at = k.at.checked_add(by).ok()?;
            if k.at < Time::ZERO || k.at > end {
                return None;
            }
        }
        Some(k)
    };
    let keys: Option<Vec<Key<V>>> = track.keys.iter().map(shift).collect();
    Some(keys.and_then(|mut keys| {
        keys.sort_by_key(|k| k.at);
        (!keys.windows(2).any(|w| w[0].at == w[1].at)).then_some(keys)
    }))
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::{Fingerprint, MediaId, MediaVideo};
    use spherecut_sphere::orient::Ypr;

    use super::*;

    fn doc() -> Doc {
        let media = Media {
            id: MediaId::NEW,
            path: "/clips/a.mp4".into(),
            fingerprint: Fingerprint {
                bytes: 1,
                head: "0000000000000000".into(),
            },
            video: MediaVideo {
                stream: 0,
                width: 1024,
                height: 512,
                rate: FrameRate::FPS_30,
                duration: Time::from_secs(10),
            },
        };
        Doc::untitled(media, INITIAL_VIEW).unwrap()
    }

    fn at(frame: i64) -> Time {
        FrameRate::FPS_30.frame_to_time(frame).unwrap()
    }

    fn looking(yaw_deg: f64, fov_deg: f64) -> ViewValues {
        let q = Ypr::from_degrees(yaw_deg, 0.0, 0.0).to_quat();
        ViewValues::of(q, &Lens::new(Projection::Rectilinear, fov_deg.to_radians()))
    }

    #[test]
    fn a_turn_edits_the_only_key_then_keys_at_the_playhead() {
        let mut d = doc();
        // One key a lane: a turn anywhere edits it, a static reframe.
        let edits = d.view_edits(at(90), &looking(30.0, 100.0)).unwrap();
        assert_eq!(edits.len(), 1, "{edits:?}");
        d.record("Turn the view", &edits).unwrap();
        let y = d.view_at(at(200)).orientation;
        assert!((Ypr::from_quat(y).yaw.to_degrees() - 30.0).abs() < 1e-9);
        // Keyed at 3 s, a turn at 6 s has no key there or a lone one to
        // edit: a look-around.
        let k = d.key_view(at(90), &looking(60.0, 80.0)).unwrap();
        d.record("Key the view", &k).unwrap();
        assert_eq!(d.lane_keys(Lane::Orientation).len(), 2);
        assert!(d.view_edits(at(180), &looking(10.0, 90.0)).is_none());
        // Within half a frame of the key, the key is edited.
        let near = at(90)
            .checked_add(Time::seconds(Rational::new(1, 70).unwrap()))
            .unwrap();
        let edits = d.view_edits(near, &looking(61.0, 80.0)).unwrap();
        assert_eq!(edits.len(), 1, "only the orientation changed: {edits:?}");
        // The field of view alone, unchanged elsewhere, edits nothing.
        assert_eq!(d.view_edits(at(90), &looking(60.0, 80.0)), Some(Vec::new()));
    }

    #[test]
    fn keying_with_a_fov_key_at_the_playhead_changes_it_and_adds_the_others() {
        let mut d = doc();
        let clip = d.clip().id;
        d.record(
            "Add a key",
            &[Edit::AddKey {
                clip,
                key: AnyKey::Fov(new_key(at(30), 70.0, Ease::SMOOTH)),
            }],
        )
        .unwrap();
        let edits = d.key_view(at(30), &looking(45.0, 50.0)).unwrap();
        let ops: Vec<_> = edits.iter().map(Edit::op).collect();
        assert_eq!(ops, ["add_key", "set_key", "add_key"]);
        d.record("Key the view", &edits).unwrap();
        assert_eq!(d.lane_keys(Lane::Fov).len(), 2);
        assert_eq!(d.lane_keys(Lane::Orientation).len(), 2);
        assert!((d.view_at(at(30)).lens.hfov.to_degrees() - 50.0).abs() < 1e-9);
        // One step undoes all three.
        d.undo().unwrap();
        assert_eq!(d.lane_keys(Lane::Orientation).len(), 1);
        assert!((d.view_at(at(30)).lens.hfov.to_degrees() - 70.0).abs() < 1e-9);
    }

    #[test]
    fn moved_keys_keep_their_order_and_stay_in_the_source() {
        let mut d = doc();
        for f in [30, 60] {
            let k = d.key_view(at(f), &looking(f as f64, 90.0)).unwrap();
            d.record("Key the view", &k).unwrap();
        }
        let o = d.lane_keys(Lane::Orientation);
        let sel: BTreeSet<KeyId> = [o[1].id].into();
        // Past the key after it: the track is replaced, in order.
        let e = d.move_keys(&sel, at(45)).unwrap();
        d.record("Move key", &e).unwrap();
        let times: Vec<Time> = d
            .lane_keys(Lane::Orientation)
            .iter()
            .map(|k| k.at)
            .collect();
        assert_eq!(times, [at(0), at(60), at(75)]);
        // Onto another key's time, or out of the source: refused.
        assert!(d.move_keys(&sel, at(-15)).is_none());
        assert!(d.move_keys(&sel, Time::from_secs(10)).is_none());
    }

    #[test]
    fn delete_keeps_one_key_a_lane() {
        let mut d = doc();
        let all: BTreeSet<KeyId> = d.keys().iter().map(|k| k.id).collect();
        let (edits, kept) = d.remove_keys(&all);
        assert!(edits.is_empty());
        assert_eq!(kept, 3);
        let k = d.key_view(at(30), &looking(10.0, 90.0)).unwrap();
        d.record("Key the view", &k).unwrap();
        let all: BTreeSet<KeyId> = d.keys().iter().map(|k| k.id).collect();
        let (edits, kept) = d.remove_keys(&all);
        assert_eq!((edits.len(), kept), (3, 3));
        d.selection = all;
        d.record("Delete keys", &edits).unwrap();
        assert_eq!(d.keys().len(), 3);
        assert_eq!(d.selection.len(), 3, "the kept keys stay selected");
    }

    #[test]
    fn dirty_follows_the_saved_revision() {
        let mut d = doc();
        assert!(!d.dirty());
        d.record("Add marker", &d.add_marker(at(10))).unwrap();
        assert!(d.dirty());
        let r = d.project.revision();
        d.saved_to("/p/a.spherecut".into(), r);
        assert!(!d.dirty());
        assert_eq!(d.name(), "a.spherecut");
        d.undo().unwrap();
        assert!(d.dirty());
    }

    #[test]
    fn key_jumps_pass_the_key_at_the_playhead() {
        let mut d = doc();
        for f in [30, 60] {
            let k = d.key_view(at(f), &looking(f as f64, 90.0)).unwrap();
            d.record("Key the view", &k).unwrap();
        }
        assert_eq!(d.key_after(at(0)), Some(at(30)));
        assert_eq!(d.key_after(at(30)), Some(at(60)));
        assert_eq!(d.key_before(at(60)), Some(at(30)));
        assert_eq!(d.key_before(at(0)), None);
        assert_eq!(d.key_after(at(60)), None);
        assert_eq!(
            d.snap(Time::seconds(Rational::new(101, 100).unwrap())),
            at(30)
        );
    }
}
