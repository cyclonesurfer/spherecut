//! What makes a project whole (M2 plan section 3.1): [`Project::validate`]
//! runs after every edit and on load, so no edit and no file can leave a
//! project that the rest of Spherecut would have to second-guess.

use std::collections::HashSet;
use std::fmt;

use spherecut_sphere::perspective::{LensShape, MIN_HFOV_DEG, Projection};
use thiserror::Error;

use super::anim::{Key, KeyTrack, Orient};
use super::limits::*;
use super::{Clip, Output, Project, Range, TrackKind};
use crate::time::Time;

/// Why an edit or a file was refused, as a stable code (A2's `edit` tool
/// returns it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// An id that names nothing.
    NotFound,
    /// Something still used (a media file by a clip, a target by a key).
    InUse,
    /// A key at a time that already has one.
    Exists,
    /// The last key of a track.
    LastKey,
    /// A time, size or value outside what is allowed.
    OutOfRange,
    /// Anything else malformed.
    Invalid,
    /// Past one of M2's limits ([`super::limits`]).
    Limit,
    /// An id given where the project gives them.
    IdGiven,
}

impl Code {
    pub const ALL: [Code; 8] = [
        Code::NotFound,
        Code::InUse,
        Code::Exists,
        Code::LastKey,
        Code::OutOfRange,
        Code::Invalid,
        Code::Limit,
        Code::IdGiven,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::NotFound => "not_found",
            Code::InUse => "in_use",
            Code::Exists => "exists",
            Code::LastKey => "last_key",
            Code::OutOfRange => "out_of_range",
            Code::Invalid => "invalid",
            Code::Limit => "limit",
            Code::IdGiven => "id_given",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refusal: its code and a sentence saying what is wrong.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct Problem {
    pub code: Code,
    pub message: String,
}

impl Problem {
    pub fn new(code: Code, message: impl Into<String>) -> Problem {
        Problem {
            code,
            message: message.into(),
        }
    }
}

fn fail<T>(code: Code, message: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::new(code, message))
}

/// A name's rules: at most [`MAX_NAME_CHARS`] characters, no control
/// characters, and not empty unless `may_be_empty`.
pub(crate) fn check_name(what: &str, name: &str, may_be_empty: bool) -> Result<(), Problem> {
    if name.is_empty() && !may_be_empty {
        return fail(Code::Invalid, format!("{what} needs a name"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return fail(
            Code::Invalid,
            format!("{what}'s name is longer than {MAX_NAME_CHARS} characters"),
        );
    }
    if name.chars().any(char::is_control) {
        return fail(
            Code::Invalid,
            format!("{what}'s name has a control character (a newline or tab, say)"),
        );
    }
    Ok(())
}

/// The fields of view a key may hold, in degrees: the widest is
/// fisheye's.
pub fn fov_range_deg() -> (f64, f64) {
    (MIN_HFOV_DEG, Projection::Fisheye.max_hfov().to_degrees())
}

pub(crate) fn check_fov(fov: f64) -> Result<(), Problem> {
    let (lo, hi) = fov_range_deg();
    if !(fov.is_finite() && (lo..=hi).contains(&fov)) {
        return fail(
            Code::OutOfRange,
            format!("a field of view of {fov}° is outside {lo}° to {hi}°"),
        );
    }
    Ok(())
}

impl Project {
    /// Check every rule a project keeps (M2 plan section 3.1). [`apply`]
    /// runs it after each edit and [`file::load`] after reading, so a
    /// project that exists has passed it.
    ///
    /// [`apply`]: Project::apply
    /// [`file::load`]: super::file::load
    pub fn validate(&self) -> Result<(), Problem> {
        self.check_ids()?;
        if self.sequences.len() != SEQUENCES {
            return fail(
                Code::Limit,
                format!(
                    "this version edits one sequence, and the project has {}",
                    self.sequences.len()
                ),
            );
        }
        for m in &self.media {
            if !m.path.is_absolute() {
                return fail(
                    Code::Invalid,
                    format!("media {}'s path {} is not absolute", m.id, m.path.display()),
                );
            }
            let v = &m.video;
            if v.width == 0 || v.height == 0 || v.duration <= Time::ZERO {
                return fail(
                    Code::Invalid,
                    format!("media {} has no picture or no duration", m.id),
                );
            }
        }
        if self.targets.len() > MAX_TARGETS {
            return fail(
                Code::Limit,
                format!("more than {MAX_TARGETS} look-at targets"),
            );
        }
        if !self.targets.is_sorted_by_key(|t| t.id) {
            return fail(Code::Invalid, "targets are not in id order");
        }
        if !self.media.is_sorted_by_key(|m| m.id) {
            return fail(Code::Invalid, "media are not in id order");
        }
        for t in &self.targets {
            check_name("a target", &t.name, false)?;
            let Some(m) = self.media_by_id(t.media) else {
                return fail(
                    Code::NotFound,
                    format!("target {} is in media {}, which isn't there", t.id, t.media),
                );
            };
            check_keys("target", &t.path, Time::ZERO, Some(m.video.duration))?;
            for k in &t.path.keys {
                let a = k.value;
                if !(a.yaw_deg.is_finite() && a.pitch_deg.is_finite()) {
                    return fail(
                        Code::Invalid,
                        format!("target {}'s key {} isn't finite", t.id, k.id),
                    );
                }
                if a.pitch_deg.abs() > 90.0 {
                    return fail(
                        Code::OutOfRange,
                        format!(
                            "target {}'s key {} has pitch {}° past ±90°",
                            t.id, k.id, a.pitch_deg
                        ),
                    );
                }
            }
        }
        for s in &self.sequences {
            check_name("the sequence", &s.name, false)?;
            let rate = s.rate.as_rational();
            if rate > crate::time::Rational::from_integer(MAX_RATE) {
                return fail(
                    Code::OutOfRange,
                    format!("a sequence rate of {rate} fps is past {MAX_RATE}"),
                );
            }
            match s.output {
                Output::Flat { short_side, .. } if !FLAT_SHORT_SIDES.contains(&short_side) => {
                    return fail(
                        Code::OutOfRange,
                        format!(
                            "a flat output's short side is one of {FLAT_SHORT_SIDES:?}, not {short_side}"
                        ),
                    );
                }
                Output::Equirect { width, .. } if !EQUIRECT_WIDTHS.contains(&width) => {
                    return fail(
                        Code::OutOfRange,
                        format!(
                            "an equirect output's width is one of {EQUIRECT_WIDTHS:?}, not {width}"
                        ),
                    );
                }
                _ => {}
            }
            if s.tracks.len() != 1 || s.tracks[0].kind != TrackKind::Video {
                return fail(Code::Limit, "this version edits one video track");
            }
            let track = &s.tracks[0];
            if track.clips.len() != 1 {
                return fail(
                    Code::Limit,
                    format!(
                        "this version edits one clip, and the track has {}",
                        track.clips.len()
                    ),
                );
            }
            for c in &track.clips {
                self.check_clip(c)?;
            }
        }
        Ok(())
    }

    /// Ids: none is 0, none is used twice, all are below the counter,
    /// and the counter stays where JSON holds it exactly.
    fn check_ids(&self) -> Result<(), Problem> {
        if self.next_id == 0 || self.next_id > MAX_ID {
            return fail(
                Code::Invalid,
                format!("the id counter {} is out of range", self.next_id),
            );
        }
        let mut seen = HashSet::new();
        let mut add = |what: &str, id: u64| {
            if id == 0 {
                return fail(Code::Invalid, format!("{what} has id 0, which is no id"));
            }
            if id >= self.next_id {
                return fail(
                    Code::Invalid,
                    format!("{what} {id} is past the id counter {}", self.next_id),
                );
            }
            if !seen.insert(id) {
                return fail(Code::Invalid, format!("id {id} is used twice"));
            }
            Ok(())
        };
        for m in &self.media {
            add("a media entry", m.id.0)?;
        }
        for t in &self.targets {
            add("a target", t.id.0)?;
            for k in &t.path.keys {
                add("a key", k.id.0)?;
            }
        }
        for s in &self.sequences {
            add("a sequence", s.id.0)?;
            for t in &s.tracks {
                add("a track", t.id.0)?;
                for c in &t.clips {
                    add("a clip", c.id.0)?;
                    for m in &c.markers {
                        add("a marker", m.id.0)?;
                    }
                    let v = &c.view;
                    for id in v.orientation.keys.iter().map(|k| k.id) {
                        add("a key", id.0)?;
                    }
                    for id in v.fov.keys.iter().map(|k| k.id) {
                        add("a key", id.0)?;
                    }
                    for id in v.projection.keys.iter().map(|k| k.id) {
                        add("a key", id.0)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_clip(&self, c: &Clip) -> Result<(), Problem> {
        let Some(media) = self.media_by_id(c.media) else {
            return fail(
                Code::NotFound,
                format!("clip {} uses media {}, which isn't there", c.id, c.media),
            );
        };
        let length = media.video.duration;
        if c.at != Time::ZERO {
            return fail(Code::Limit, "this version starts the clip at 0");
        }
        let Range { start, end } = c.source;
        if start < Time::ZERO || end > length || start >= end {
            return fail(
                Code::OutOfRange,
                format!(
                    "the trim {:.3} s to {:.3} s is not inside the source (0 to {:.3} s) with its start before its end",
                    start.to_secs_f64(),
                    end.to_secs_f64(),
                    length.to_secs_f64()
                ),
            );
        }
        if c.markers.len() > MAX_MARKERS {
            return fail(Code::Limit, format!("more than {MAX_MARKERS} markers"));
        }
        if !c.markers.is_sorted_by_key(|m| (m.at, m.id)) {
            return fail(Code::Invalid, "markers are not in time order");
        }
        for m in &c.markers {
            check_name("a marker", &m.name, true)?;
            if m.at < Time::ZERO || m.at > length {
                return fail(
                    Code::OutOfRange,
                    format!(
                        "marker {} at {:.3} s is outside the source",
                        m.id,
                        m.at.to_secs_f64()
                    ),
                );
            }
        }
        let v = &c.view;
        check_keys("orientation", &v.orientation, Time::ZERO, Some(length))?;
        check_keys("fov", &v.fov, Time::ZERO, Some(length))?;
        check_keys("projection", &v.projection, Time::ZERO, Some(length))?;
        for k in &v.orientation.keys {
            if k.spin.abs() > MAX_SPIN {
                return fail(
                    Code::OutOfRange,
                    format!("key {} spins {} turns, more than {MAX_SPIN}", k.id, k.spin),
                );
            }
            if let Orient::Aim { target, roll_deg } = k.value {
                let Some(t) = self.target(target) else {
                    return fail(
                        Code::NotFound,
                        format!("key {} aims at target {target}, which isn't there", k.id),
                    );
                };
                if t.media != c.media {
                    return fail(
                        Code::Invalid,
                        format!(
                            "key {} aims at target {target}, which is in media {}, not the clip's {}",
                            k.id, t.media, c.media
                        ),
                    );
                }
                if !(roll_deg.is_finite() && roll_deg.abs() <= 180.0) {
                    return fail(
                        Code::OutOfRange,
                        format!("key {}'s roll {roll_deg}° is outside ±180°", k.id),
                    );
                }
            }
        }
        for k in &v.fov.keys {
            check_fov(k.value)
                .map_err(|p| Problem::new(p.code, format!("key {}: {}", k.id, p.message)))?;
            check_no_spin(k)?;
        }
        for k in &v.projection.keys {
            check_no_spin(k)?;
            if let LensShape::Morph(m) = k.value {
                if m.from == m.to {
                    return fail(
                        Code::Invalid,
                        format!("key {} morphs from a projection to itself", k.id),
                    );
                }
                if !(0.0..=1.0).contains(&m.morph) {
                    return fail(
                        Code::OutOfRange,
                        format!("key {}'s morph {} is outside 0 to 1", k.id, m.morph),
                    );
                }
            }
        }
        Ok(())
    }
}

fn check_no_spin<V>(k: &Key<V>) -> Result<(), Problem> {
    if k.spin != 0 {
        return fail(
            Code::Invalid,
            format!("key {} spins, and only orientation keys do", k.id),
        );
    }
    Ok(())
}

/// A track: one key at least, at most [`MAX_KEYS`], in strictly
/// increasing time within [`from`, `to`].
fn check_keys<V>(
    what: &str,
    track: &KeyTrack<V>,
    from: Time,
    to: Option<Time>,
) -> Result<(), Problem> {
    let keys = &track.keys;
    if keys.is_empty() {
        return fail(Code::LastKey, format!("the {what} track has no key"));
    }
    if keys.len() > MAX_KEYS {
        return fail(
            Code::Limit,
            format!("the {what} track has more than {MAX_KEYS} keys"),
        );
    }
    if let Some(pair) = keys.windows(2).find(|w| w[0].at >= w[1].at) {
        return fail(
            Code::Exists,
            format!(
                "the {what} track's keys {} and {} are not in increasing time",
                pair[0].id, pair[1].id
            ),
        );
    }
    for k in [&keys[0], &keys[keys.len() - 1]] {
        if k.at < from || to.is_some_and(|to| k.at > to) {
            return fail(
                Code::OutOfRange,
                format!(
                    "key {} at {:.3} s is outside the source",
                    k.id,
                    k.at.to_secs_f64()
                ),
            );
        }
    }
    Ok(())
}
