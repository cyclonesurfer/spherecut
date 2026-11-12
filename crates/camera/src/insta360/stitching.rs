//! A file's calibration strings as sphere's types (M3 plan 5.2 and 5.3):
//! the stored numbers as a [`Calibration`], and with the table's
//! [`Known`] entry, all that `Rig::build` needs from a file but the tune.

use spherecut_sphere::rig::{self, CalibTerms, Calibration, LensCalib};

use super::calib::{self, Calib, Focal, Generation, Token};
use super::known::{self, Known};
use super::metadata::{Metadata, Window};
use crate::{CameraError, Part};

/// What a file gives a stitch: its stored calibration and the table's
/// entry for its camera, whose geometry reads the generation field 136
/// names.
#[derive(Debug, Clone, PartialEq)]
pub struct Stitching {
    pub calibration: Calibration,
    pub known: Known,
}

/// sphere's [`Calibration`] of a file's parsed V3 and V6 strings and its
/// field 27 window, either string or both: src is the canvas's height,
/// the side of each block's square; dst is the window's, or src when the
/// file has no window; each block's numbers are taken as stored (Mei's
/// ξ and focal lengths, the centre, the angles, the translation, 0 where
/// a block stores none, and the terms in the string's order), but block
/// 1's cx, which is made local to its own square (cx − src). Field 27's
/// offset isn't read: plan 3.2's U13 leaves it out until M3.6 rules on
/// it.
///
/// Plan 5.2's `to_calibration(&Calib)` can't fill a `Calibration`, which
/// holds src, field 27's dst and both generations, so it takes all
/// three. Strings that parse but can't be a calibration are
/// [`CameraError::NotCalibration`], never a guess: neither string
/// (record 1); a string of another generation in a generation's place, a
/// block with a radius for its focal, a term count that isn't the
/// generation's, a canvas that isn't two squares side by side, or a V6
/// canvas other than V3's (the string's field, 54 or 111); a window that
/// isn't square, or whose src isn't the canvas's square (field 27).
pub fn to_calibration(
    v3: Option<&Calib>,
    v6: Option<&Calib>,
    window: Option<&Window>,
) -> Result<Calibration, CameraError> {
    let first = match (v3, v6) {
        (Some(c), _) => (c, rig::Generation::V3),
        (None, Some(c)) => (c, rig::Generation::V6),
        (None, None) => {
            return Err(not_calibration(
                Part::Record(1),
                "it holds neither a V3 nor a V6 calibration string",
            ));
        }
    };
    let src = side(first.0, first.1)?;
    let v3 = v3
        .map(|c| blocks(c, rig::Generation::V3, src))
        .transpose()?;
    let v6 = v6
        .map(|c| blocks(c, rig::Generation::V6, src))
        .transpose()?;
    let dst = match window {
        Some(w) => window_side(w, src)?,
        None => src,
    };
    Ok(Calibration { src, dst, v3, v6 })
}

/// What `meta` gives a stitch, the one entry probe and `agent::still`
/// share: the working V3 and V6 strings parsed and through
/// [`to_calibration`] with field 27's window, and the table's entry for
/// the model and firmware ([`super::known::lookup`]) with its geometry's
/// generation the one field 136 names, 3 for V3 and 4 for V6, which the
/// calibration must hold.
///
/// `None` when the metadata holds neither a V3 nor a V6 string. An error
/// when one doesn't parse, when they can't be a calibration, and when
/// field 136 can't say which the rig reads ([`CameraError::NotCalibration`]
/// in field 136): absent, as no X5 file is, since the strings alone
/// don't say which the camera uses; naming V1 or V2, whose radius focal
/// a rig doesn't read; naming none of the four (5, V8, among them); or
/// naming a generation whose string the file doesn't hold.
pub fn stitching(meta: &Metadata) -> Result<Option<Stitching>, CameraError> {
    let parsed = |g: Generation| {
        meta.calib
            .get(g)
            .working
            .as_deref()
            .map(|s| calib::parse(g, s))
            .transpose()
    };
    let v3 = parsed(Generation::V3)?;
    let v6 = parsed(Generation::V6)?;
    if v3.is_none() && v6.is_none() {
        return Ok(None);
    }
    let calibration = to_calibration(v3.as_ref(), v6.as_ref(), meta.window.as_ref())?;
    let in_use = in_use(meta.offset_version)?;
    let (held, why) = match in_use {
        rig::Generation::V3 => (
            calibration.v3.is_some(),
            "it names V3, whose string the file doesn't hold",
        ),
        rig::Generation::V6 => (
            calibration.v6.is_some(),
            "it names V6, whose string the file doesn't hold",
        ),
    };
    if !held {
        return Err(not_calibration(Part::Field(136), why));
    }
    let mut known = known::lookup(
        meta.model.as_deref().unwrap_or_default(),
        meta.firmware.as_deref().unwrap_or_default(),
    );
    known.geometry.generation = in_use;
    Ok(Some(Stitching { calibration, known }))
}

/// The generation field 136 (`offset_version`) names, as a rig reads it.
fn in_use(offset_version: Option<u64>) -> Result<rig::Generation, CameraError> {
    let why = match offset_version.map(Generation::of_offset_version) {
        Some(Some(Generation::V3)) => return Ok(rig::Generation::V3),
        Some(Some(Generation::V6)) => return Ok(rig::Generation::V6),
        Some(Some(Generation::V1 | Generation::V2)) => {
            "it names V1 or V2, whose radius focal a rig doesn't read"
        }
        Some(None) => "it names none of the four generations",
        None => "it is missing, so the generation the camera uses isn't known",
    };
    Err(not_calibration(Part::Field(136), why))
}

/// The field of `generation`'s working string.
fn field(generation: rig::Generation) -> Part {
    Part::Field(match generation {
        rig::Generation::V3 => 54,
        rig::Generation::V6 => 111,
    })
}

/// The side of `c`'s blocks' squares, its canvas's height, if the canvas
/// is two squares side by side.
fn side(c: &Calib, generation: rig::Generation) -> Result<u32, CameraError> {
    let (w, h) = c.canvas;
    if h == 0 || u64::from(w) != 2 * u64::from(h) {
        return Err(not_calibration(
            field(generation),
            "its canvas isn't two squares side by side",
        ));
    }
    Ok(h)
}

/// `c`'s two blocks as sphere's, if it is a string of `generation`'s on
/// squares of `src`.
fn blocks(c: &Calib, generation: rig::Generation, src: u32) -> Result<[LensCalib; 2], CameraError> {
    let part = field(generation);
    let of = match generation {
        rig::Generation::V3 => Generation::V3,
        rig::Generation::V6 => Generation::V6,
    };
    if c.generation != of {
        return Err(not_calibration(
            part,
            "it holds a string of another generation",
        ));
    }
    if side(c, generation)? != src {
        return Err(not_calibration(part, "its canvas isn't the V3 string's"));
    }
    let [l0, l1] = &c.lenses;
    let block = |b: usize, l: &calib::LensTokens| -> Result<LensCalib, CameraError> {
        let Focal::Mei { xi, fx, fy } = &l.focal else {
            let why = if b == 0 {
                "block 0 gives a radius, not Mei's ξ and focal lengths"
            } else {
                "block 1 gives a radius, not Mei's ξ and focal lengths"
            };
            return Err(not_calibration(part, why));
        };
        let count = || {
            let why = if b == 0 {
                "block 0's term count isn't its generation's"
            } else {
                "block 1's term count isn't its generation's"
            };
            not_calibration(part, why)
        };
        let values: Vec<f64> = l.coefficients.iter().map(Token::value).collect();
        let terms = match generation {
            rig::Generation::V3 => {
                let [k1, k2, k3, p1, p2] =
                    <[f64; 5]>::try_from(values.as_slice()).map_err(|_| count())?;
                CalibTerms::V3 {
                    k: [k1, k2, k3],
                    p: [p1, p2],
                }
            }
            rig::Generation::V6 => CalibTerms::V6 {
                d: <[f64; 13]>::try_from(values.as_slice()).map_err(|_| count())?,
            },
        };
        // Block 1's centre counts from the canvas's left; its square
        // starts src further on (5.2).
        let left = if b == 0 { 0.0 } else { f64::from(src) };
        Ok(LensCalib {
            xi: xi.value(),
            fx: fx.value(),
            fy: fy.value(),
            cx: l.cx.value() - left,
            cy: l.cy.value(),
            ypr_deg: l.ypr.each_ref().map(Token::value),
            t_m: l
                .t
                .as_ref()
                .map_or([0.0; 3], |t| t.each_ref().map(Token::value)),
            terms,
        })
    };
    Ok([block(0, l0)?, block(1, l1)?])
}

/// dst, field 27's window's side, if the window is square and its src
/// the calibration's square of `src`.
fn window_side(w: &Window, src: u32) -> Result<u32, CameraError> {
    let [dst, dst_h] = w.dst;
    if dst != dst_h {
        return Err(not_calibration(Part::Field(27), "its window isn't square"));
    }
    if w.src != [src; 2] {
        return Err(not_calibration(
            Part::Field(27),
            "its window's src isn't the calibration canvas's square",
        ));
    }
    Ok(dst)
}

fn not_calibration(part: Part, why: &'static str) -> CameraError {
    CameraError::NotCalibration { part, why }
}
