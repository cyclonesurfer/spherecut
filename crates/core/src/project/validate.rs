//! What makes a project whole (M2 plan section 3.1): [`Project::validate`]
//! runs after every edit and on load, so no edit and no file can leave a
//! project that the rest of Spherecut would have to second-guess.

use std::collections::HashSet;
use std::f64::consts::PI;
use std::fmt;

use glam::DVec3;
use spherecut_sphere::perspective::{LensShape, MIN_HFOV_DEG, Projection};
use spherecut_sphere::rig::{Generation, Rig, RigError};
use thiserror::Error;

use super::anim::{Key, KeyTrack, Orient};
use super::limits::*;
use super::source::{Cap, CapMode, DualFisheye, Gains, Source, ValidateTest};
use super::{Clip, Media, Output, Project, Range, TrackKind};
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
    /// Each media's source and caps keep M3 plan 11.2's, and each refusal
    /// names the media and the field: [`Code::OutOfRange`] for a bound,
    /// NaN and infinity past every one, and [`Code::Invalid`] otherwise.
    /// A dual-fisheye source's are checked in this order, the first
    /// refusal returned: its tracks differ, the first the media's video
    /// stream; its front track is 0 or 1; the geometry's generation has
    /// its set in the snapshot; the hash is the raw strings'; the square
    /// and the window have sides; each of that set's principal points
    /// within the span of its square the canvas map shows (0 to src, or
    /// the window under map B); each lens's yaw, pitch and roll within
    /// ±15°, centre within ±64 px, radius scale 0.9 to 1.1, θ_max 85° to
    /// 110°, `xi_delta` within ±0.15 and ξ plus it above −1; the feather
    /// 0° to 15° and q 0 to 3.34 m⁻¹; the gains; the caps; then, from one
    /// [`Rig::build`] at the calibration's square, θ_max below the model's
    /// limit, the feather at most max(0, min(θ_max, θ_frame) − 90°) for
    /// each lens, and the coverage rule: no direction left that neither
    /// lens's mask holds at the tune's q. No rule reads a file.
    ///
    /// [`apply`]: Project::apply
    /// [`file::load`]: super::file::load
    pub fn validate(&self) -> Result<(), Problem> {
        self.validate_leaving_out(ValidateTest::default())
    }

    /// [`Project::validate`] with the rules `test` turns off left out:
    /// only a test's control asks for any.
    #[doc(hidden)]
    pub fn validate_leaving_out(&self, test: ValidateTest) -> Result<(), Problem> {
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
            // A file stores paths lexically, so save then load gives them
            // back only if they are already.
            if super::file::lexical(&m.path) != m.path {
                return fail(
                    Code::Invalid,
                    format!(
                        "media {}'s path {} has \".\" or \"..\" in it (give it as {})",
                        m.id,
                        m.path.display(),
                        super::file::lexical(&m.path).display()
                    ),
                );
            }
            let v = &m.video;
            if v.width == 0 || v.height == 0 || v.duration <= Time::ZERO {
                return fail(
                    Code::Invalid,
                    format!("media {} has no picture or no duration", m.id),
                );
            }
            check_source(m, test)?;
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
                check_no_spin(k)?;
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
        // Each edit adds one; past this it would wrap.
        if self.revision > MAX_ID {
            return fail(
                Code::Invalid,
                format!("the revision {} is out of range", self.revision),
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

/// The bounds of a dual-fisheye source's numbers (M3 plan 11.2), in the
/// units its fields name. `sphere`'s `tune_default_lies_inside_validates_bounds`
/// copies them.
mod bounds {
    /// Each lens's yaw, pitch and roll, ± degrees.
    pub const YPR_DEG: f64 = 15.0;
    /// Each lens's centre shift, ± canvas pixels.
    pub const CENTRE_PX: f64 = 64.0;
    /// Each lens's radius scale.
    pub const RADIUS: (f64, f64) = (0.9, 1.1);
    /// Each lens's θ_max, degrees.
    pub const THETA_MAX_DEG: (f64, f64) = (85.0, 110.0);
    /// Each lens's ξ delta, ±: as far as the radius scale's 0.9 to 1.1
    /// moves θ_frame on X5-shaped sets, rounded outward (the range M3.9
    /// proposed for 11.2's list).
    pub const XI_DELTA: f64 = 0.15;
    /// The feather's half width, degrees, before each lens's own bound.
    pub const FEATHER_DEG: (f64, f64) = (0.0, 15.0);
    /// The stitch distance as q = 1/D, m⁻¹ (D ≥ 0.3 m).
    pub const Q_PER_M: (f64, f64) = (0.0, 3.34);
    /// A fixed gain.
    pub const GAIN: (f64, f64) = (0.25, 4.0);
    /// A cap's radius, degrees.
    pub const CAP_DEG: (f64, f64) = (5.0, 35.0);
    /// A cap's soft edge, degrees.
    pub const EDGE_DEG: (f64, f64) = (0.0, 5.0);
}

/// `v` within `lo` to `hi`, NaN and infinity outside: else an
/// out-of-range refusal naming media `id`'s `field` (a closure, so that
/// the name is only written for a refusal).
fn within(
    id: impl fmt::Display,
    field: impl FnOnce() -> String,
    v: f64,
    (lo, hi): (f64, f64),
    unit: &str,
) -> Result<(), Problem> {
    if !(lo..=hi).contains(&v) {
        return fail(
            Code::OutOfRange,
            format!(
                "media {id}'s {} of {v}{unit} is outside {lo}{unit} to {hi}{unit}",
                field()
            ),
        );
    }
    Ok(())
}

/// A media's source and caps (M3 plan 11.2): an EAC pair's two tracks
/// differ; a dual-fisheye source's structure, its tune's and gains'
/// bounds, then its caps, then the rules that build its model.
fn check_source(m: &Media, test: ValidateTest) -> Result<(), Problem> {
    match &m.source {
        Source::Equirect {} => check_caps(m),
        Source::EacPair(eac) => {
            check_tracks(m, eac.tracks)?;
            check_caps(m)
        }
        Source::DualFisheye(df) => {
            check_fisheye(m, df, test)?;
            check_caps(m)?;
            check_rig(m, df, test)
        }
    }
}

/// The two tracks are two streams: `Rig::build` pairs its blocks with
/// tracks 0 and 1 whatever streams they name, so nothing after `validate`
/// would see one stream named twice.
fn check_tracks(m: &Media, tracks: [u32; 2]) -> Result<(), Problem> {
    if tracks[0] == tracks[1] {
        return fail(
            Code::Invalid,
            format!(
                "media {}'s source.tracks name stream {} twice: each lens is a stream of its own",
                m.id, tracks[0]
            ),
        );
    }
    Ok(())
}

/// A dual-fisheye source's rules that build no model, in order: the
/// tracks (two streams, the first the media's video), the front track,
/// the geometry's set, the hash, the square's and the window's sides,
/// that set's principal points, each lens's tune (yaw, pitch and roll,
/// centre, radius scale, θ_max, ξ delta and the sum ξ + ξ delta), the
/// feather's and q's ranges, and the gains.
fn check_fisheye(m: &Media, df: &DualFisheye, test: ValidateTest) -> Result<(), Problem> {
    use bounds::*;
    let id = m.id;
    check_tracks(m, df.tracks)?;
    // Plan 11.1: a camera file's media is its first lens track, the
    // stream the clip's timing follows and the file was probed on (the
    // M3.9 review's R6), whichever client wrote the source.
    if df.tracks[0] != m.video.stream {
        return fail(
            Code::Invalid,
            format!(
                "media {id}'s source.tracks[0] is stream {}, and the media's video is stream {}: \
                 the first lens track is the stream the clip's timing follows",
                df.tracks[0], m.video.stream
            ),
        );
    }
    // `Rig::build`'s own rule (RigError::FrontTrack), held here so that
    // no project reaches it.
    if df.front_track > 1 {
        return fail(
            Code::OutOfRange,
            format!(
                "media {id}'s source.front_track {} is neither 0 nor 1",
                df.front_track
            ),
        );
    }
    let cal = &df.calibration;
    let (set, name) = match df.geometry.generation {
        Generation::V3 => (cal.v3, "v3"),
        Generation::V6 => (cal.v6, "v6"),
    };
    let Some(set) = set else {
        return fail(
            Code::Invalid,
            format!(
                "media {id}'s source.geometry.generation reads {:?}, and source.calibration.{name} is null",
                df.geometry.generation
            ),
        );
    };
    let hash = cal.raw.hash();
    if cal.hash != hash {
        return fail(
            Code::Invalid,
            format!(
                "media {id}'s source.calibration.hash {} isn't its raw strings' hash {hash}",
                cal.hash
            ),
        );
    }
    // Each block's principal point within the span of its square that
    // the canvas map shows (the M3.9 review's R2): outside, the lens's
    // axis is out of frame and it covers nothing of its slot. Block 1's cx
    // left canvas-absolute, as the raw string holds it, is the likeliest
    // slip. The tune's centre may still take it out by 64 px, where
    // θ_frame is 0 and the feather's and coverage rules refuse it.
    let rig_cal = cal.calibration();
    // `Rig::build`'s own rule (RigError::NotPositive), held here so that
    // the span below is read from a square and a window that have sides.
    for (field, side) in [("canvas[1]", rig_cal.src), ("window[0]", rig_cal.dst)] {
        if side == 0 {
            return fail(
                Code::Invalid,
                format!(
                    "media {id}'s source.calibration.{field} is 0, and a side of 0 makes no rig"
                ),
            );
        }
    }
    let span = df.geometry.canvas.slot_span(rig_cal.src, rig_cal.dst);
    for (b, block) in set.iter().enumerate() {
        for (axis, v) in [("cx", block.cx), ("cy", block.cy)] {
            within(
                id,
                || format!("source.calibration.{name}[{b}].{axis}"),
                v,
                span,
                " px",
            )?;
        }
    }
    let tune = &df.tune;
    for (b, lens) in tune.lenses.iter().enumerate() {
        let field = |f: &str| format!("source.tune.lenses[{b}].{f}");
        for (i, &a) in lens.ypr_deg.iter().enumerate() {
            within(
                id,
                || format!("{}[{i}]", field("ypr_deg")),
                a,
                (-YPR_DEG, YPR_DEG),
                "°",
            )?;
        }
        for (i, &c) in lens.centre_px.iter().enumerate() {
            within(
                id,
                || format!("{}[{i}]", field("centre_px")),
                c,
                (-CENTRE_PX, CENTRE_PX),
                " px",
            )?;
        }
        within(id, || field("radius_scale"), lens.radius_scale, RADIUS, "")?;
        within(
            id,
            || field("theta_max_deg"),
            lens.theta_max_deg,
            THETA_MAX_DEG,
            "°",
        )?;
        within(
            id,
            || field("xi_delta"),
            lens.xi_delta,
            (-XI_DELTA, XI_DELTA),
            "",
        )?;
        // F038: the unified model images nothing at ξ ≤ −1. A ξ that
        // isn't finite is the rig's to refuse, as the snapshot's other
        // numbers are.
        let xi = set[b].xi;
        let sum = xi + lens.xi_delta;
        if !test.no_xi_sum && sum.is_finite() && sum <= -1.0 {
            return fail(
                Code::OutOfRange,
                format!(
                    "media {id}'s {} of {} takes block {b}'s ξ of {xi} to {sum}: \
                     ξ + xi_delta must stay above −1, where the lens images nothing",
                    field("xi_delta"),
                    lens.xi_delta
                ),
            );
        }
    }
    within(
        id,
        || "source.tune.feather_deg".into(),
        tune.feather_deg,
        FEATHER_DEG,
        "°",
    )?;
    within(
        id,
        || "source.tune.q_per_m".into(),
        tune.q_per_m,
        Q_PER_M,
        " m⁻¹",
    )?;
    check_gains(m, &df.gains)
}

impl DualFisheye {
    /// The rules [`Project::validate`] keeps for this source as `media`'s,
    /// in its order, with no I/O and nothing else of the project: the
    /// structure and the tune's and gains' bounds, then the rules that
    /// build its model (θ_max below the limit, the feather short of each
    /// lens's frame, the coverage rule). `media` gives the id a refusal
    /// names and the video stream the first track must be; its own
    /// source isn't read, nor its caps, which are the media's. A client
    /// that makes a source, as media's `camera_source` does from a file's
    /// calibration at the default tune, checks it here before it goes
    /// into a project (the M3.9 review's R7).
    pub fn check(&self, media: &Media) -> Result<(), Problem> {
        let test = ValidateTest::default();
        check_fisheye(media, self, test)?;
        check_rig(media, self, test)
    }
}

/// Fixed gains within 0.25 to 4; an Auto track with a rate above 0, a
/// sample at least ([`Gains::at`] gives ones without one) and finite
/// ratios.
fn check_gains(m: &Media, gains: &Gains) -> Result<(), Problem> {
    let id = m.id;
    match gains {
        Gains::Off {} => {}
        Gains::Fixed { rgb } => {
            for (b, block) in rgb.iter().enumerate() {
                for (c, &g) in block.iter().enumerate() {
                    within(
                        id,
                        || format!("source.gains.rgb[{b}][{c}]"),
                        g,
                        bounds::GAIN,
                        "",
                    )?;
                }
            }
        }
        Gains::Auto {
            rate_hz, log_ratio, ..
        } => {
            if !(rate_hz.is_finite() && *rate_hz > 0.0) {
                return fail(
                    Code::OutOfRange,
                    format!(
                        "media {id}'s source.gains.rate_hz of {rate_hz} isn't a rate above 0 \
                         samples a second"
                    ),
                );
            }
            if log_ratio.is_empty() {
                return fail(
                    Code::Invalid,
                    format!("media {id}'s source.gains.log_ratio has no samples"),
                );
            }
            for (k, sample) in log_ratio.iter().enumerate() {
                if let Some(c) = sample.iter().position(|r| !r.is_finite()) {
                    return fail(
                        Code::OutOfRange,
                        format!(
                            "media {id}'s source.gains.log_ratio[{k}][{c}] of {} is not finite",
                            sample[c]
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Each cap 5° to 35° with an edge of 0° to 5°, and its other numbers
/// finite.
fn check_caps(m: &Media) -> Result<(), Problem> {
    let id = m.id;
    let caps: [(&str, Option<&Cap>); 2] = [
        ("nadir", m.caps.nadir.as_deref()),
        ("zenith", m.caps.zenith.as_deref()),
    ];
    for (pole, cap) in caps {
        let Some(cap) = cap else { continue };
        within(
            id,
            || format!("caps.{pole}.cap_deg"),
            cap.cap_deg,
            bounds::CAP_DEG,
            "°",
        )?;
        within(
            id,
            || format!("caps.{pole}.edge_deg"),
            cap.edge_deg,
            bounds::EDGE_DEG,
            "°",
        )?;
        let ring = match cap.mode {
            CapMode::Fill { ring_deg } => Some(ring_deg),
            CapMode::Patch { .. } => None,
        };
        let numbers = [
            ("mode.ring_deg", ring),
            ("rotation_deg", Some(cap.rotation_deg)),
        ];
        for (field, v) in numbers {
            if let Some(v) = v.filter(|v| !v.is_finite()) {
                return fail(
                    Code::Invalid,
                    format!("media {id}'s caps.{pole}.{field} of {v}° is not finite"),
                );
            }
        }
    }
    Ok(())
}

/// The rules that build the source's model, once: [`Rig::build`] at the
/// calibration's square (`src`, as `rig::theta_frame` takes it), which
/// refuses θ_max past the model's `limit_theta` and anything it can't
/// build from; then each lens's θ_frame against the feather; then the
/// coverage rule.
fn check_rig(m: &Media, df: &DualFisheye, test: ValidateTest) -> Result<(), Problem> {
    let id = m.id;
    let cal = df.calibration.calibration();
    let rig = match Rig::build(&cal, &df.geometry, &df.tune, df.front_track, [cal.src; 2]) {
        Ok(rig) => rig,
        Err(RigError::PastLimit {
            block,
            theta_max_deg,
            limit_deg,
        }) => {
            return fail(
                Code::OutOfRange,
                format!(
                    "media {id}'s source.tune.lenses[{block}].theta_max_deg of {theta_max_deg}° \
                     lies {LIMIT_WORDS} of {limit_deg:.3}°, where its r(θ) stops rising"
                ),
            );
        }
        Err(e) => {
            return fail(
                Code::Invalid,
                format!("media {id}'s source makes no rig: {e}"),
            );
        }
    };
    let tune = &df.tune;
    // The feather's band ends inside each lens's picture: h at most
    // min(θ_max, θ_frame) − 90°, and 0 where that is below 0, where a
    // feather of 0 switches at the band's centre and makes no hole of its
    // own (plan amendment 5). The band is centred 90° + δ_b out (F025,
    // Rig::band_centre), and the bound has no δ in it: where the band
    // would pass the mask, Rig::feather_at shrinks h.
    for (b, lens) in rig.lenses.iter().enumerate() {
        let theta_max = tune.lenses[b].theta_max_deg;
        let frame = lens.theta_frame.to_degrees();
        let bound = (theta_max.min(frame) - 90.0).max(0.0);
        if tune.feather_deg > bound {
            return fail(
                Code::OutOfRange,
                format!(
                    "media {id}'s source.tune.feather_deg of {}° would take the feather's band \
                     past where lens {b}'s picture ends: lens {b}'s θ_max is {theta_max}° and its \
                     θ_frame {frame:.3}°, so the feather is at most max(0, min(θ_max, θ_frame) − \
                     90°) = {bound}°",
                    tune.feather_deg
                ),
            );
        }
    }
    // The coverage rule. Lens b's mask holds every direction within a_b
    // of its axis: θ_b on the parallax-corrected ray normalize(d − q c_b)
    // is at most min(θ_max, θ_frame) there, as that ray turns from d by
    // asin(q |c_b|) at most. The two caps leave no direction out when
    // their half-angles sum to 360° less the angle between the axes: the
    // directions neither holds are two caps about the axes' opposites, of
    // 180° − a_b each, which meet when those sum past the angle between
    // the axes. The LOD's margin is left out, as the feather's is.
    let cap = [0, 1].map(|b| {
        let lens = &rig.lenses[b];
        let parallax = (rig.q * lens.centre_m.length()).min(1.0).asin();
        lens.theta_max.min(lens.theta_frame) - parallax
    });
    let axis = |b: usize| rig.lenses[b].lens_from_rig.inverse() * DVec3::Z;
    let between = if test.axes_opposite {
        PI
    } else {
        axis(0).angle_between(axis(1))
    };
    let need = 2.0 * PI - between;
    if cap[0] + cap[1] < need {
        let [a0, a1] = cap.map(f64::to_degrees);
        return fail(
            Code::OutOfRange,
            format!(
                "media {id}'s source.tune leaves directions {COVERAGE_WORDS} at q = {} m⁻¹: \
                 the lenses' masks hold caps of {a0:.3}° and {a1:.3}° about axes {:.3}° apart, \
                 and two caps cover the sphere only when they sum to 360° less that angle, \
                 {:.3}°; raise θ_max, or lower the radius scale, q or the turn between the lenses",
                tune.q_per_m,
                between.to_degrees(),
                need.to_degrees()
            ),
        );
    }
    Ok(())
}

/// The words the limit's refusal and the coverage rule's are found by.
const LIMIT_WORDS: &str = "past its lens's limit";
const COVERAGE_WORDS: &str = "covered by neither lens";

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
    // In seconds too: the spline divides by the gap between keys, and
    // two rational times may differ by less than an f64 holds.
    if let Some(pair) = keys
        .windows(2)
        .find(|w| w[0].at >= w[1].at || w[0].at.to_secs_f64() >= w[1].at.to_secs_f64())
    {
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
