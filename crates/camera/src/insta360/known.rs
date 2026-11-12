//! The built-in geometry table (M3 plan 5.3): for each camera model, the
//! conventions its files leave open, as sphere's [`Geometry`] with its
//! status and evidence, and what else a file's own bytes don't say.
//!
//! Before M3.6 the X5's entry is plan 3.2's Meanwhile column, its
//! extrinsics a placeholder: the identity [`EulerConv`] with the −90°
//! roll offset, "placeholder until the fit". M3.6 replaces it with what
//! its fit on clips 8 to 10 finds (8.6). A firmware update or a later
//! body needs a row here, not code.

use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::rig::{
    Binding, CanvasMap, EulerConv, Extrinsics, Generation, Geometry, QuarterTurn, Status, TurnAxis,
    TzSign,
};

/// What the table knows of a camera, as [`lookup`] gives it for one
/// file's model and firmware.
///
/// [`Geometry`] owns its evidence, a `String` that `lookup` words for the
/// file it was asked about, so a `Known` is made for each lookup rather
/// than held in a static.
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    /// The model the entry is for, by the table's name for it: `"X5"`,
    /// whose files' field 2 says `"Insta360 X5"`. A model with no entry
    /// gets the X5's, so this is `"X5"` then too, beside the notice.
    pub model: String,
    /// The firmware the entry's evidence covers (`["v1.1.22_build1"]` on
    /// the X5). A file on firmware it doesn't list gets the entry as
    /// provisional, with evidence that says so.
    pub measured_on: &'static [&'static str],
    /// The conventions, with their status and evidence. Its generation
    /// is the table's; [`super::stitching`] sets it from the file's field
    /// 136.
    pub geometry: Geometry,
    /// The track each half of the LRV shows, left half first: `[1, 0]` on
    /// the X5, whose left half is track 1 (plan 3.1).
    pub lrv_half_track: [u8; 2],
    /// The track of the lens on the screen's side, once a take settles it
    /// (take 15, M3.12); `None` until then.
    pub screen_side_track: Option<u8>,
    /// Whether the app stitches the model's files. False when M3.6's
    /// clips fail 8.6's gross rule, and the app then shows such files as
    /// lenses (13.2).
    pub stitchable: bool,
    /// What a user is told about the entry, such as "geometry assumed
    /// from the X5" for a model with no entry of its own; `None` when
    /// there is nothing to say.
    pub notice: Option<&'static str>,
}

/// One row of the table: a camera body, by the model its files' field
/// 2 names, with the conventions its files leave open and what its
/// evidence covers. A firmware update or a later body is a row.
struct Row {
    /// The table's name for the body.
    model: &'static str,
    /// Field 2 as the body's files hold it.
    field_2: &'static str,
    measured_on: &'static [&'static str],
    binding: Binding,
    canvas: CanvasMap,
    extrinsics: Extrinsics,
    turn: TurnAxis,
    tz: TzSign,
    upright: QuarterTurn,
    /// The generation the row reads when a file doesn't say;
    /// [`super::stitching`] reads field 136's.
    generation: Generation,
    v6: V6Reading,
    status: Status,
    evidence: &'static str,
    lrv_half_track: [u8; 2],
    screen_side_track: Option<u8>,
    stitchable: bool,
}

/// The X5, as plan 3.2's Meanwhile column has it until M3.6's fit:
/// block 0 on track 1 (U2: block 0 is the canvas's left, the LRV's left
/// half is track 1, record 22 agrees), canvas map C (U3), the placeholder
/// `EulerConv` (U4), tz as stored (U5), V6 read radial only (U6), V6 (U7:
/// field 136 = 4), upright. 3.2 gives no turn axis: Y, as synth's
/// `x5_like_planted` takes it.
const X5: Row = Row {
    model: "X5",
    field_2: "Insta360 X5",
    measured_on: &["v1.1.22_build1"],
    binding: Binding::Reversed,
    canvas: CanvasMap::C,
    extrinsics: Extrinsics::Stored(EulerConv::PLACEHOLDER),
    turn: TurnAxis::Y,
    tz: TzSign::AsStored,
    upright: QuarterTurn::Zero,
    generation: Generation::V6,
    v6: V6Reading::RADIAL_ONLY,
    status: Status::Provisional,
    evidence: "placeholder until the fit",
    lrv_half_track: [1, 0],
    screen_side_track: None,
    stitchable: true,
};

/// The table, one row a body.
const ROWS: &[Row] = &[X5];

/// The notice a model with no row of its own gets.
const ASSUMED: &str = "geometry assumed from the X5";

/// The table's entry for `model` on `firmware`, as field 2 and field 3
/// name them. A firmware the entry's `measured_on` doesn't list gets the
/// entry as provisional, with evidence that says so; a model with no
/// entry gets the X5's conventions, provisional, with the notice
/// "geometry assumed from the X5". Neither string is quoted back: the
/// evidence names only what the table holds.
pub fn lookup(model: &str, firmware: &str) -> Known {
    let found = ROWS.iter().find(|r| r.field_2 == model);
    let row = found.unwrap_or(&X5);
    let measured = row.measured_on.contains(&firmware);
    let (status, evidence, notice) = match (found, measured) {
        (Some(_), true) => (row.status, row.evidence.to_owned(), None),
        (Some(_), false) => (
            Status::Provisional,
            format!(
                "{}; measured on firmware {}, not this file's",
                row.evidence,
                row.measured_on.join(", ")
            ),
            None,
        ),
        (None, _) => (
            Status::Provisional,
            format!(
                "{}; the {}'s row, as no row is for this file's model",
                row.evidence, row.model
            ),
            Some(ASSUMED),
        ),
    };
    Known {
        model: row.model.to_owned(),
        measured_on: row.measured_on,
        geometry: Geometry {
            binding: row.binding,
            canvas: row.canvas,
            extrinsics: row.extrinsics,
            turn: row.turn,
            tz: row.tz,
            upright: row.upright,
            generation: row.generation,
            v6: row.v6,
            status,
            evidence,
        },
        lrv_half_track: row.lrv_half_track,
        screen_side_track: row.screen_side_track,
        stitchable: row.stitchable,
        notice,
    }
}
