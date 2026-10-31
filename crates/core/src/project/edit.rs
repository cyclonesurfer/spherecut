//! Edits (M2 plan section 4, DECISIONS D48): the one way a project
//! changes.
//!
//! An [`Edit`] is a value naming ids, never positions, serialised with an
//! `"op"` tag (`{"op": "set_trim", "clip": 4, "end": {...}}`) for A2's
//! `edit` tool and for undo history. [`Project::apply`] applies a list of
//! them in order, all or none, and returns them as applied (new ids
//! filled in) with the edits that undo them.

use serde::{Deserialize, Serialize};
use spherecut_sphere::perspective::LensShape;
use thiserror::Error;

use super::anim::{AimDir, Ease, Key, KeyTrack, Orient, OrientMode, Orientation, Target};
use super::validate::{Code, Problem, check_fov};
use super::{
    Clip, ClipId, ExportSettings, KeyId, Marker, MarkerId, Media, MediaId, Output, Project,
    SequenceId, TargetId, TrackId,
};
use crate::time::{FrameRate, Time};

/// One change to a project. Optional fields left out are left as they
/// are. An id of 0 (or left out) in something added means "give it a new
/// id".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// Add a media entry (made from a probe of the file, by the caller:
    /// `apply` does no I/O).
    AddMedia {
        media: Media,
    },
    /// Remove a media entry no clip uses.
    RemoveMedia {
        media: MediaId,
    },
    /// Add a clip to a track. M2: refused, the track has its one clip.
    AddClip {
        track: TrackId,
        clip: Clip,
    },
    /// Remove a clip with its keys and markers. M2: refused, it is the
    /// only clip.
    RemoveClip {
        clip: ClipId,
    },
    /// The source's in and out.
    SetTrim {
        clip: ClipId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<Time>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<Time>,
    },
    SetOutput {
        sequence: SequenceId,
        output: Output,
    },
    SetRate {
        sequence: SequenceId,
        rate: FrameRate,
    },
    SetExport {
        sequence: SequenceId,
        export: ExportSettings,
    },
    /// A key on one of a clip's view tracks, at a time without one.
    AddKey {
        clip: ClipId,
        key: AnyKey,
    },
    /// Change a key in place: its time (not onto another key's), value
    /// (of its track's kind), ease or spin.
    SetKey {
        key: KeyId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<Time>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<AnyValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ease: Option<Ease>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spin: Option<i8>,
    },
    /// Remove a key; a track keeps at least one.
    RemoveKey {
        key: KeyId,
    },
    /// Replace all of one track's keys. A key keeps its id when it has
    /// one of that track's ids; give 0 for a new key.
    SetKeys {
        clip: ClipId,
        keys: AnyKeys,
    },
    SetOrientMode {
        clip: ClipId,
        mode: OrientMode,
    },
    /// Many orientation (and field of view) keys at once, one per point
    /// (PLAN A2's view path). With `replace`, the keys from the first
    /// point's time to the last's are removed first; without it, a point
    /// at a key's time is refused.
    SetViewPath {
        clip: ClipId,
        points: Vec<PathPoint>,
        #[serde(default)]
        replace: bool,
    },
    AddMarker {
        clip: ClipId,
        marker: Marker,
    },
    SetMarker {
        marker: MarkerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<Time>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    RemoveMarker {
        marker: MarkerId,
    },
    AddTarget {
        target: Target,
    },
    SetTarget {
        target: TargetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<KeyTrack<AimDir>>,
    },
    /// Remove a target no key aims at.
    RemoveTarget {
        target: TargetId,
    },
    /// Rename the sequence, marker or target with this id.
    Rename {
        id: u64,
        name: String,
    },
}

/// A key for one of a view's three tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnyKey {
    Orientation(Key<Orient>),
    Fov(Key<f64>),
    Projection(Key<LensShape>),
}

/// A value for a key of one of a view's three tracks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnyValue {
    Orientation(Orient),
    Fov(f64),
    Projection(LensShape),
}

/// All of one track's keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnyKeys {
    Orientation(Vec<Key<Orient>>),
    Fov(Vec<Key<f64>>),
    Projection(Vec<Key<LensShape>>),
}

/// A point of a view path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathPoint {
    pub at: Time,
    pub orientation: Orientation,
    /// Degrees; left out, the field of view keys are left alone here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov: Option<f64>,
    /// The ids the point's keys were given, filled in by `apply` so that
    /// redo makes the same ids.
    #[serde(default, skip_serializing_if = "KeyId::is_new_ref")]
    pub orientation_key: KeyId,
    #[serde(default, skip_serializing_if = "KeyId::is_new_ref")]
    pub fov_key: KeyId,
}

impl KeyId {
    fn is_new_ref(&self) -> bool {
        self.is_new()
    }
}

/// Edits as applied, with their inverse.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The edits with every new id filled in: what redo applies.
    pub edits: Vec<Edit>,
    /// What undoes them, in the order to apply it.
    pub inverse: Vec<Edit>,
}

/// A refused list of edits: which edit, and why. The project is as it
/// was.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("edit {index} ({op}): {problem}")]
pub struct EditError {
    pub index: usize,
    pub op: &'static str,
    pub problem: Problem,
}

impl EditError {
    pub fn code(&self) -> Code {
        self.problem.code
    }
}

/// Whose ids an edit may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ids {
    /// From outside (the GUI, an agent): a new thing's id is 0.
    Fresh,
    /// Undo and redo: things come back with the ids they had.
    Restore,
}

impl Edit {
    /// The `op` name.
    pub fn op(&self) -> &'static str {
        match self {
            Edit::AddMedia { .. } => "add_media",
            Edit::RemoveMedia { .. } => "remove_media",
            Edit::AddClip { .. } => "add_clip",
            Edit::RemoveClip { .. } => "remove_clip",
            Edit::SetTrim { .. } => "set_trim",
            Edit::SetOutput { .. } => "set_output",
            Edit::SetRate { .. } => "set_rate",
            Edit::SetExport { .. } => "set_export",
            Edit::AddKey { .. } => "add_key",
            Edit::SetKey { .. } => "set_key",
            Edit::RemoveKey { .. } => "remove_key",
            Edit::SetKeys { .. } => "set_keys",
            Edit::SetOrientMode { .. } => "set_orient_mode",
            Edit::SetViewPath { .. } => "set_view_path",
            Edit::AddMarker { .. } => "add_marker",
            Edit::SetMarker { .. } => "set_marker",
            Edit::RemoveMarker { .. } => "remove_marker",
            Edit::AddTarget { .. } => "add_target",
            Edit::SetTarget { .. } => "set_target",
            Edit::RemoveTarget { .. } => "remove_target",
            Edit::Rename { .. } => "rename",
        }
    }

    /// Every `op` name, in the enum's order.
    pub const OPS: [&'static str; 21] = [
        "add_media",
        "remove_media",
        "add_clip",
        "remove_clip",
        "set_trim",
        "set_output",
        "set_rate",
        "set_export",
        "add_key",
        "set_key",
        "remove_key",
        "set_keys",
        "set_orient_mode",
        "set_view_path",
        "add_marker",
        "set_marker",
        "remove_marker",
        "add_target",
        "set_target",
        "remove_target",
        "rename",
    ];
}

fn fail<T>(code: Code, message: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::new(code, message))
}

/// Which of a view's tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Orientation,
    Fov,
    Projection,
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Orientation => "orientation",
            Which::Fov => "fov",
            Which::Projection => "projection",
        }
    }
}

impl AnyValue {
    fn into_orientation(self) -> Option<Orient> {
        match self {
            AnyValue::Orientation(o) => Some(o),
            _ => None,
        }
    }

    fn into_fov(self) -> Option<f64> {
        match self {
            AnyValue::Fov(f) => Some(f),
            _ => None,
        }
    }

    fn into_projection(self) -> Option<LensShape> {
        match self {
            AnyValue::Projection(p) => Some(p),
            _ => None,
        }
    }

    fn which(&self) -> Which {
        match self {
            AnyValue::Orientation(_) => Which::Orientation,
            AnyValue::Fov(_) => Which::Fov,
            AnyValue::Projection(_) => Which::Projection,
        }
    }
}

impl Project {
    /// Apply `edits` in order, all or none (M2 plan section 4.2). On
    /// success the revision is one higher and [`Applied`] holds the edits
    /// as applied and their inverse; on error the project is unchanged
    /// and the error names the edit and why. Each edit is checked, and
    /// the project validated, before the next is applied. No I/O.
    pub fn apply(&mut self, edits: &[Edit]) -> Result<Applied, EditError> {
        self.apply_with(edits, Ids::Fresh)
    }

    pub(crate) fn apply_with(&mut self, edits: &[Edit], ids: Ids) -> Result<Applied, EditError> {
        if edits.is_empty() {
            return Err(EditError {
                index: 0,
                op: "none",
                problem: Problem::new(Code::Invalid, "no edits to apply"),
            });
        }
        let mut work = self.clone();
        let mut applied = Vec::with_capacity(edits.len());
        let mut inverses = Vec::with_capacity(edits.len());
        for (index, edit) in edits.iter().enumerate() {
            let mut edit = edit.clone();
            let op = edit.op();
            let err = |problem| EditError { index, op, problem };
            let inverse = work.apply_one(&mut edit, ids).map_err(err)?;
            work.validate().map_err(err)?;
            applied.push(edit);
            inverses.push(inverse);
        }
        work.revision = self.revision + 1;
        *self = work;
        Ok(Applied {
            edits: applied,
            inverse: inverses.into_iter().rev().flatten().collect(),
        })
    }

    /// Give `id` a new number if it is 0; otherwise, a restored thing's
    /// id must be one the project gave.
    fn give(&mut self, id: &mut u64, ids: Ids, what: &str) -> Result<(), Problem> {
        if *id == 0 {
            *id = self.take_id();
            return Ok(());
        }
        match ids {
            Ids::Fresh => fail(
                Code::IdGiven,
                format!("the new {what} has id {id}; leave `id` out and the project gives one"),
            ),
            Ids::Restore if *id < self.next_id => Ok(()),
            Ids::Restore => fail(
                Code::Invalid,
                format!("the {what} {id} has an id this project never gave"),
            ),
        }
    }

    /// Give ids to `keys` that replace `old`'s: a key keeps an id `old`
    /// has; 0 gets a new one; anything else only on undo and redo.
    fn give_keys<V>(
        &mut self,
        keys: &mut [Key<V>],
        old: &[Key<V>],
        ids: Ids,
    ) -> Result<(), Problem> {
        for k in keys {
            if k.id.is_new() || ids == Ids::Restore || !old.iter().any(|o| o.id == k.id) {
                self.give(&mut k.id.0, ids, "key")?;
            }
        }
        Ok(())
    }

    fn give_clip(&mut self, clip: &mut Clip, ids: Ids) -> Result<(), Problem> {
        self.give(&mut clip.id.0, ids, "clip")?;
        for m in &mut clip.markers {
            self.give(&mut m.id.0, ids, "marker")?;
        }
        let v = &mut clip.view;
        for k in &mut v.orientation.keys {
            self.give(&mut k.id.0, ids, "key")?;
        }
        for k in &mut v.fov.keys {
            self.give(&mut k.id.0, ids, "key")?;
        }
        for k in &mut v.projection.keys {
            self.give(&mut k.id.0, ids, "key")?;
        }
        Ok(())
    }

    fn clip_or_fail(&mut self, id: ClipId) -> Result<&mut Clip, Problem> {
        match self.clip_mut(id) {
            Some(c) => Ok(c),
            None => fail(Code::NotFound, format!("there is no clip {id}")),
        }
    }

    fn sequence_or_fail(&mut self, id: SequenceId) -> Result<&mut super::Sequence, Problem> {
        match self.sequence_mut(id) {
            Some(s) => Ok(s),
            None => fail(Code::NotFound, format!("there is no sequence {id}")),
        }
    }

    /// The clip and track holding key `id`.
    fn find_key(&self, id: KeyId) -> Option<(ClipId, Which)> {
        self.clips().find_map(|c| {
            let v = &c.view;
            if v.orientation.key(id).is_some() {
                Some((c.id, Which::Orientation))
            } else if v.fov.key(id).is_some() {
                Some((c.id, Which::Fov))
            } else if v.projection.key(id).is_some() {
                Some((c.id, Which::Projection))
            } else {
                None
            }
        })
    }

    fn find_marker(&self, id: MarkerId) -> Option<ClipId> {
        self.clips().find(|c| c.marker(id).is_some()).map(|c| c.id)
    }

    /// Apply one edit, filling in its new ids, and return what undoes it.
    fn apply_one(&mut self, edit: &mut Edit, ids: Ids) -> Result<Vec<Edit>, Problem> {
        Ok(match edit {
            Edit::AddMedia { media } => {
                self.give(&mut media.id.0, ids, "media entry")?;
                let i = self.media.partition_point(|m| m.id < media.id);
                self.media.insert(i, media.clone());
                vec![Edit::RemoveMedia { media: media.id }]
            }
            Edit::RemoveMedia { media } => {
                let id = *media;
                let Some(i) = self.media.iter().position(|m| m.id == id) else {
                    return fail(Code::NotFound, format!("there is no media {id}"));
                };
                if let Some(c) = self.clips().find(|c| c.media == id) {
                    return fail(Code::InUse, format!("clip {} uses media {id}", c.id));
                }
                if let Some(t) = self.targets.iter().find(|t| t.media == id) {
                    return fail(Code::InUse, format!("target {} is in media {id}", t.id));
                }
                vec![Edit::AddMedia {
                    media: self.media.remove(i),
                }]
            }
            Edit::AddClip { track, clip } => {
                self.give_clip(clip, ids)?;
                let id = *track;
                let Some(t) = self
                    .sequences
                    .iter_mut()
                    .flat_map(|s| &mut s.tracks)
                    .find(|t| t.id == id)
                else {
                    return fail(Code::NotFound, format!("there is no track {id}"));
                };
                let i = t
                    .clips
                    .partition_point(|c| (c.at, c.id) < (clip.at, clip.id));
                t.clips.insert(i, clip.clone());
                vec![Edit::RemoveClip { clip: clip.id }]
            }
            Edit::RemoveClip { clip } => {
                let id = *clip;
                let found = self
                    .sequences
                    .iter_mut()
                    .flat_map(|s| &mut s.tracks)
                    .find_map(|t| {
                        let i = t.clips.iter().position(|c| c.id == id)?;
                        Some((t.id, t.clips.remove(i)))
                    });
                let Some((track, clip)) = found else {
                    return fail(Code::NotFound, format!("there is no clip {id}"));
                };
                vec![Edit::AddClip { track, clip }]
            }
            Edit::SetTrim { clip, start, end } => {
                if start.is_none() && end.is_none() {
                    return fail(Code::Invalid, "set_trim needs `start`, `end` or both");
                }
                let id = *clip;
                let c = self.clip_or_fail(id)?;
                let old = c.source;
                c.source.start = start.unwrap_or(old.start);
                c.source.end = end.unwrap_or(old.end);
                vec![Edit::SetTrim {
                    clip: id,
                    start: Some(old.start),
                    end: Some(old.end),
                }]
            }
            Edit::SetOutput { sequence, output } => {
                let id = *sequence;
                let s = self.sequence_or_fail(id)?;
                let old = std::mem::replace(&mut s.output, *output);
                vec![Edit::SetOutput {
                    sequence: id,
                    output: old,
                }]
            }
            Edit::SetRate { sequence, rate } => {
                let id = *sequence;
                let s = self.sequence_or_fail(id)?;
                let old = std::mem::replace(&mut s.rate, *rate);
                vec![Edit::SetRate {
                    sequence: id,
                    rate: old,
                }]
            }
            Edit::SetExport { sequence, export } => {
                let id = *sequence;
                let s = self.sequence_or_fail(id)?;
                let old = std::mem::replace(&mut s.export, *export);
                vec![Edit::SetExport {
                    sequence: id,
                    export: old,
                }]
            }
            Edit::AddKey { clip, key } => {
                let clip = *clip;
                let id = match key {
                    AnyKey::Orientation(k) => {
                        self.add_key(clip, k, ids, |c| &mut c.view.orientation)?
                    }
                    AnyKey::Fov(k) => self.add_key(clip, k, ids, |c| &mut c.view.fov)?,
                    AnyKey::Projection(k) => {
                        self.add_key(clip, k, ids, |c| &mut c.view.projection)?
                    }
                };
                vec![Edit::RemoveKey { key: id }]
            }
            Edit::SetKey {
                key,
                at,
                value,
                ease,
                spin,
            } => {
                if at.is_none() && value.is_none() && ease.is_none() && spin.is_none() {
                    return fail(
                        Code::Invalid,
                        "set_key needs `at`, `value`, `ease` or `spin`",
                    );
                }
                let id = *key;
                let Some((clip, which)) = self.find_key(id) else {
                    return fail(Code::NotFound, format!("there is no key {id} on a clip"));
                };
                if let Some(v) = value
                    && v.which() != which
                {
                    return fail(
                        Code::Invalid,
                        format!(
                            "key {id} is on the {} track, so its value can't be a {} value",
                            which.name(),
                            v.which().name()
                        ),
                    );
                }
                let c = self.clip_or_fail(clip)?;
                let (at, value, ease, spin) = (*at, *value, *ease, *spin);
                let old = match which {
                    Which::Orientation => {
                        let v = value.and_then(AnyValue::into_orientation);
                        let k = set_key(&mut c.view.orientation, id, at, v, ease, spin)?;
                        (k.at, AnyValue::Orientation(k.value), k.ease, k.spin)
                    }
                    Which::Fov => {
                        let v = value.and_then(AnyValue::into_fov);
                        let k = set_key(&mut c.view.fov, id, at, v, ease, spin)?;
                        (k.at, AnyValue::Fov(k.value), k.ease, k.spin)
                    }
                    Which::Projection => {
                        let v = value.and_then(AnyValue::into_projection);
                        let k = set_key(&mut c.view.projection, id, at, v, ease, spin)?;
                        (k.at, AnyValue::Projection(k.value), k.ease, k.spin)
                    }
                };
                vec![Edit::SetKey {
                    key: id,
                    at: Some(old.0),
                    value: Some(old.1),
                    ease: Some(old.2),
                    spin: Some(old.3),
                }]
            }
            Edit::RemoveKey { key } => {
                let id = *key;
                let Some((clip, which)) = self.find_key(id) else {
                    return fail(Code::NotFound, format!("there is no key {id} on a clip"));
                };
                let c = self.clip_or_fail(clip)?;
                let key = match which {
                    Which::Orientation => {
                        AnyKey::Orientation(remove_key(&mut c.view.orientation, id)?)
                    }
                    Which::Fov => AnyKey::Fov(remove_key(&mut c.view.fov, id)?),
                    Which::Projection => {
                        AnyKey::Projection(remove_key(&mut c.view.projection, id)?)
                    }
                };
                vec![Edit::AddKey { clip, key }]
            }
            Edit::SetKeys { clip, keys } => {
                let id = *clip;
                let old = match keys {
                    AnyKeys::Orientation(k) => {
                        AnyKeys::Orientation(
                            self.set_keys(id, k, ids, |c| &mut c.view.orientation)?,
                        )
                    }
                    AnyKeys::Fov(k) => {
                        AnyKeys::Fov(self.set_keys(id, k, ids, |c| &mut c.view.fov)?)
                    }
                    AnyKeys::Projection(k) => {
                        AnyKeys::Projection(self.set_keys(id, k, ids, |c| &mut c.view.projection)?)
                    }
                };
                vec![Edit::SetKeys {
                    clip: id,
                    keys: old,
                }]
            }
            Edit::SetOrientMode { clip, mode } => {
                let id = *clip;
                let c = self.clip_or_fail(id)?;
                let old = std::mem::replace(&mut c.view.mode, *mode);
                vec![Edit::SetOrientMode {
                    clip: id,
                    mode: old,
                }]
            }
            Edit::SetViewPath {
                clip,
                points,
                replace,
            } => self.set_view_path(*clip, points, *replace, ids)?,
            Edit::AddMarker { clip, marker } => {
                self.give(&mut marker.id.0, ids, "marker")?;
                let c = self.clip_or_fail(*clip)?;
                let i = c
                    .markers
                    .partition_point(|m| (m.at, m.id) < (marker.at, marker.id));
                c.markers.insert(i, marker.clone());
                vec![Edit::RemoveMarker { marker: marker.id }]
            }
            Edit::SetMarker { marker, at, name } => {
                if at.is_none() && name.is_none() {
                    return fail(Code::Invalid, "set_marker needs `at`, `name` or both");
                }
                let id = *marker;
                let Some(clip) = self.find_marker(id) else {
                    return fail(Code::NotFound, format!("there is no marker {id}"));
                };
                let c = self.clip_or_fail(clip)?;
                let i = c
                    .markers
                    .iter()
                    .position(|m| m.id == id)
                    .unwrap_or_default();
                let mut m = c.markers.remove(i);
                let old = m.clone();
                if let Some(at) = at {
                    m.at = *at;
                }
                if let Some(name) = name {
                    m.name.clone_from(name);
                }
                let i = c.markers.partition_point(|o| (o.at, o.id) < (m.at, m.id));
                c.markers.insert(i, m);
                vec![Edit::SetMarker {
                    marker: id,
                    at: Some(old.at),
                    name: Some(old.name),
                }]
            }
            Edit::RemoveMarker { marker } => {
                let id = *marker;
                let Some(clip) = self.find_marker(id) else {
                    return fail(Code::NotFound, format!("there is no marker {id}"));
                };
                let c = self.clip_or_fail(clip)?;
                let i = c
                    .markers
                    .iter()
                    .position(|m| m.id == id)
                    .unwrap_or_default();
                vec![Edit::AddMarker {
                    clip,
                    marker: c.markers.remove(i),
                }]
            }
            Edit::AddTarget { target } => {
                self.give(&mut target.id.0, ids, "target")?;
                for k in &mut target.path.keys {
                    self.give(&mut k.id.0, ids, "key")?;
                }
                let i = self.targets.partition_point(|t| t.id < target.id);
                self.targets.insert(i, target.clone());
                vec![Edit::RemoveTarget { target: target.id }]
            }
            Edit::SetTarget { target, name, path } => {
                if name.is_none() && path.is_none() {
                    return fail(Code::Invalid, "set_target needs `name`, `path` or both");
                }
                let id = *target;
                let Some(i) = self.targets.iter().position(|t| t.id == id) else {
                    return fail(Code::NotFound, format!("there is no target {id}"));
                };
                let old = self.targets[i].clone();
                if let Some(path) = path {
                    self.give_keys(&mut path.keys, &old.path.keys, ids)?;
                    self.targets[i].path = path.clone();
                }
                if let Some(name) = name {
                    self.targets[i].name.clone_from(name);
                }
                vec![Edit::SetTarget {
                    target: id,
                    name: Some(old.name),
                    path: Some(old.path),
                }]
            }
            Edit::RemoveTarget { target } => {
                let id = *target;
                let Some(i) = self.targets.iter().position(|t| t.id == id) else {
                    return fail(Code::NotFound, format!("there is no target {id}"));
                };
                let aiming = self.clips().find_map(|c| {
                    c.view
                        .orientation
                        .keys
                        .iter()
                        .find(|k| matches!(k.value, Orient::Aim { target, .. } if target == id))
                });
                if let Some(k) = aiming {
                    return fail(Code::InUse, format!("key {} aims at target {id}", k.id));
                }
                vec![Edit::AddTarget {
                    target: self.targets.remove(i),
                }]
            }
            Edit::Rename { id, name } => {
                let id = *id;
                let old = self.rename(id, name)?;
                vec![Edit::Rename { id, name: old }]
            }
        })
    }

    fn add_key<V: Clone>(
        &mut self,
        clip: ClipId,
        key: &mut Key<V>,
        ids: Ids,
        track: impl FnOnce(&mut Clip) -> &mut KeyTrack<V>,
    ) -> Result<KeyId, Problem> {
        self.give(&mut key.id.0, ids, "key")?;
        let c = self.clip_or_fail(clip)?;
        let t = track(c);
        if let Some(k) = t.keys.iter().find(|k| k.at == key.at) {
            return fail(
                Code::Exists,
                format!("key {} is already at {:.3} s", k.id, key.at.to_secs_f64()),
            );
        }
        t.insert(key.clone());
        Ok(key.id)
    }

    fn set_keys<V: Clone>(
        &mut self,
        clip: ClipId,
        keys: &mut [Key<V>],
        ids: Ids,
        track: impl Fn(&mut Clip) -> &mut KeyTrack<V>,
    ) -> Result<Vec<Key<V>>, Problem> {
        let old = track(self.clip_or_fail(clip)?).keys.clone();
        self.give_keys(keys, &old, ids)?;
        track(self.clip_or_fail(clip)?).keys = keys.to_vec();
        Ok(old)
    }

    fn set_view_path(
        &mut self,
        clip: ClipId,
        points: &mut [PathPoint],
        replace: bool,
        ids: Ids,
    ) -> Result<Vec<Edit>, Problem> {
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return fail(Code::Invalid, "a view path needs at least one point");
        };
        let (first, last) = (first.at, last.at);
        if points.windows(2).any(|w| w[0].at >= w[1].at) {
            return fail(
                Code::Invalid,
                "a view path's points must be in increasing time",
            );
        }
        for p in points.iter() {
            if let Some(f) = p.fov {
                check_fov(f)?;
            }
        }
        let with_fov = points.iter().any(|p| p.fov.is_some());
        for p in points.iter_mut() {
            self.give(&mut p.orientation_key.0, ids, "key")?;
            if p.fov.is_some() {
                self.give(&mut p.fov_key.0, ids, "key")?;
            }
        }
        let c = self.clip_or_fail(clip)?;
        let old_orientation = c.view.orientation.keys.clone();
        let old_fov = c.view.fov.keys.clone();
        let outside = |at: Time| at < first || at > last;
        if replace {
            c.view.orientation.keys.retain(|k| outside(k.at));
            if with_fov {
                c.view.fov.keys.retain(|k| outside(k.at));
            }
        }
        for p in points.iter() {
            let taken =
                |keys: &[Key<_>]| keys.iter().find(|k: &&Key<_>| k.at == p.at).map(|k| k.id);
            if let Some(k) = taken(&c.view.orientation.keys) {
                return fail(
                    Code::Exists,
                    format!("key {k} is already at {:.3} s", p.at.to_secs_f64()),
                );
            }
            c.view.orientation.insert(Key {
                id: p.orientation_key,
                at: p.at,
                value: Orient::View(p.orientation),
                ease: Ease::SMOOTH,
                spin: 0,
            });
            if let Some(fov) = p.fov {
                if let Some(k) = c.view.fov.keys.iter().find(|k| k.at == p.at) {
                    return fail(
                        Code::Exists,
                        format!("key {} is already at {:.3} s", k.id, p.at.to_secs_f64()),
                    );
                }
                c.view.fov.insert(Key {
                    id: p.fov_key,
                    at: p.at,
                    value: fov,
                    ease: Ease::SMOOTH,
                    spin: 0,
                });
            }
        }
        let mut inverse = vec![Edit::SetKeys {
            clip,
            keys: AnyKeys::Orientation(old_orientation),
        }];
        if with_fov {
            inverse.push(Edit::SetKeys {
                clip,
                keys: AnyKeys::Fov(old_fov),
            });
        }
        Ok(inverse)
    }

    /// Rename what `id` names, returning its old name.
    fn rename(&mut self, id: u64, name: &str) -> Result<String, Problem> {
        if let Some(s) = self.sequences.iter_mut().find(|s| s.id.0 == id) {
            return Ok(std::mem::replace(&mut s.name, name.to_owned()));
        }
        if let Some(t) = self.targets.iter_mut().find(|t| t.id.0 == id) {
            return Ok(std::mem::replace(&mut t.name, name.to_owned()));
        }
        if let Some(clip) = self.find_marker(MarkerId(id)) {
            let c = self.clip_or_fail(clip)?;
            if let Some(m) = c.markers.iter_mut().find(|m| m.id.0 == id) {
                return Ok(std::mem::replace(&mut m.name, name.to_owned()));
            }
        }
        fail(
            Code::NotFound,
            format!("there is no sequence, marker or target {id} to rename"),
        )
    }
}

/// Change key `id` of `track` in place, keeping the keys in time order,
/// and return it as it was.
fn set_key<V: Clone>(
    track: &mut KeyTrack<V>,
    id: KeyId,
    at: Option<Time>,
    value: Option<V>,
    ease: Option<Ease>,
    spin: Option<i8>,
) -> Result<Key<V>, Problem> {
    let Some(i) = track.keys.iter().position(|k| k.id == id) else {
        return fail(Code::NotFound, format!("there is no key {id}"));
    };
    if let Some(at) = at
        && let Some(k) = track.keys.iter().find(|k| k.at == at && k.id != id)
    {
        return fail(
            Code::Exists,
            format!("key {} is already at {:.3} s", k.id, at.to_secs_f64()),
        );
    }
    let mut k = track.keys.remove(i);
    let old = k.clone();
    if let Some(at) = at {
        k.at = at;
    }
    if let Some(v) = value {
        k.value = v;
    }
    if let Some(e) = ease {
        k.ease = e;
    }
    if let Some(s) = spin {
        k.spin = s;
    }
    track.insert(k);
    Ok(old)
}

fn remove_key<V>(track: &mut KeyTrack<V>, id: KeyId) -> Result<Key<V>, Problem> {
    if track.keys.len() <= 1 {
        return fail(
            Code::LastKey,
            format!("key {id} is its track's last; a track keeps one key"),
        );
    }
    let Some(i) = track.keys.iter().position(|k| k.id == id) else {
        return fail(Code::NotFound, format!("there is no key {id}"));
    };
    Ok(track.keys.remove(i))
}
