//! The built-in geometry table (M3 plan 5.3): for each camera model, the
//! conventions its files leave open, as sphere's [`Geometry`] with its
//! status and evidence, and what else a file's own bytes don't say.
//!
//! Before M3.6 the X5's entry is a placeholder: the identity
//! [`EulerConv`](spherecut_sphere::rig::EulerConv) with the −90° roll
//! offset, "placeholder until the fit". M3.6 replaces it with what its
//! fit on clips 8 to 10 finds (8.6). A firmware update or a later body
//! needs a row here, not code.

use spherecut_sphere::rig::Geometry;

/// What the table knows of a camera, as [`lookup`] gives it for one
/// file's model and firmware.
///
/// [`Geometry`] owns its evidence, a `String` that `lookup` words for the
/// file it was asked about, so a `Known` is made for each lookup rather
/// than held in a static.
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    /// The model the entry is for, as field 2 names it: `"Insta360 X5"`.
    /// For a model with no entry, the model asked about.
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

/// The table's entry for `model` on `firmware`, as field 2 and field 3
/// name them. A firmware the entry's `measured_on` doesn't list gets the
/// entry as provisional, with evidence that says so; a model with no
/// entry gets the X5's conventions, provisional, with the notice
/// "geometry assumed from the X5".
#[expect(unused_variables, reason = "M3.5 unit (b) writes the body")]
pub fn lookup(model: &str, firmware: &str) -> Known {
    todo!("M3.5 unit (b)")
}
