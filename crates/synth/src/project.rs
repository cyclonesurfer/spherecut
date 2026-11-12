//! The export and viewer fixture (M3 plan 11.5, rungs 4 and 5 of 4.2):
//! rung 3's file made a project as the app makes one of a camera file,
//! then stitched by hand, with an Auto twin.
//!
//! The project is `media::project::project_media` of the file with
//! `camera_source`'s source, through `Project::untitled`; then a
//! `set_source` that takes the binding rung 3 planted (block b on track
//! b, `x5_like_planted`'s, where the X5's row has block b on track 1 − b),
//! as a user who has checked it would; then one `set_stitch` with a tune
//! that differs from the default wherever a render can show it: q =
//! 1/1.5 m⁻¹, lens B yawed 0.3° and a 3° feather, with Fixed gains of 0.9
//! on lens B, or in the twin a hand-written Auto track. The numbers are
//! handed out beside the projects, so that a test builds its reference
//! rig from them through `Rig::build`, never from the project: a reader
//! shared by the export and the reference can't hide a tune it drops.

use std::path::Path;

use spherecut_core::project::{
    Analysed, AnalysisInput, DualFisheye, Edit, Gains, History, INITIAL_VIEW, Origin, Project,
    Sampling, Source, gains_basis,
};
use spherecut_media::backend::{FfmpegBackend, MediaBackend, VideoRequest};
use spherecut_media::probe::probe;
use spherecut_media::project::{camera_source, project_media};
use spherecut_sphere::rig::{Binding, Geometry, Status, Tune};

use crate::insv::{InsvFiles, InsvSpec, write_insv};
use crate::lens::X5_LIKE_SIDE;
use crate::media::SynthError;

/// The label of the fixture's `set_source`.
pub const PLANTED_BINDING: &str = "Planted binding";
/// The label of its `set_stitch`.
pub const STITCH: &str = "Stitch";

/// 11.5's fixture: rung 3's file, its two projects, and the numbers
/// their `set_stitch` holds, apart from them.
#[derive(Debug, Clone)]
pub struct StitchFixture {
    /// Rung 3's file.
    pub files: InsvFiles,
    /// What [`write_insv`] wrote it from: [`InsvSpec::rung3`].
    pub spec: InsvSpec,
    /// The project with Fixed gains.
    pub fixed: Project,
    /// Its two steps, [`PLANTED_BINDING`] and [`STITCH`].
    pub fixed_history: History,
    /// The Auto twin: the same tune, with [`StitchFixture::auto_gains`].
    pub auto: Project,
    /// Its two steps, as [`StitchFixture::fixed_history`]'s.
    pub auto_history: History,
    /// The tune both `set_stitch`es set: q = 1/1.5 m⁻¹, lens B (block 1)
    /// yawed 0.3° and a 3° feather, the rest the default's.
    pub tune: Tune,
    /// The fixed project's gains: 0.9 on each of lens B's channels, 1 on
    /// lens A's.
    pub fixed_gains: Gains,
    /// The twin's: an Auto track a frame, at the file's rate, whose log
    /// ratio ramps from 0 on the first frame to 0.2 on the last on each
    /// channel, measured under [`StitchFixture::tune`] from the master's
    /// every frame by no estimator (0: written by hand).
    pub auto_gains: Gains,
    /// The track whose lens looks forward: 0, as rung 3 was composed.
    pub front_track: u8,
    /// Each track's side: `[768, 768]`.
    pub sides: [u32; 2],
}

/// 11.5's tune: q = 1/1.5 m⁻¹, lens B (block 1) yawed 0.3° and a 3°
/// feather, the rest [`Tune::default`]'s.
pub fn fixture_tune() -> Tune {
    let mut tune = Tune {
        feather_deg: 3.0,
        q_per_m: 1.0 / 1.5,
        ..Tune::default()
    };
    tune.lenses[1].ypr_deg = [0.3, 0.0, 0.0];
    tune
}

/// Writes rung 3's file ([`InsvSpec::rung3`]) into `dir` and makes 11.5's
/// fixture of it: probed, track 0's stream opened for its timing, the
/// media `project_media` makes with `camera_source`'s source, an
/// untitled project of it at [`INITIAL_VIEW`], and in each of the two
/// projects a `set_source` labelled [`PLANTED_BINDING`] (the source with
/// its geometry's binding made Direct and its status User, and nothing
/// else changed) and a `set_stitch` labelled [`STITCH`] (the tune, and
/// the Fixed or the Auto gains), each by the user. An error when the file can't be written or read back as
/// a camera file, or when an edit is refused.
pub fn stitch_fixture(dir: &Path) -> Result<StitchFixture, SynthError> {
    let spec = InsvSpec::rung3();
    let files = write_insv(&spec, dir)?;
    let master = files.master.as_path();
    let read = |why: String| SynthError::Read {
        path: master.to_path_buf(),
        why,
    };
    let report = probe(master).map_err(|e| read(e.to_string()))?;
    let lenses = report
        .camera_file
        .lenses
        .as_ref()
        .ok_or_else(|| read("probe finds no lens tracks".into()))?;
    let request = VideoRequest {
        stream: Some(lenses.tracks[0]),
        ..VideoRequest::default()
    };
    let info = FfmpegBackend::new()
        .open_video(master, &request)
        .map_err(|e| read(e.to_string()))?
        .info()
        .clone();
    let src = spherecut_media::Source::path(master);
    let mut media = project_media(src, master, &info).map_err(read)?;
    let camera = camera_source(src, &report)
        .map_err(read)?
        .ok_or_else(|| read("camera_source finds no calibration a stitch reads".into()))?;
    let fingerprint = media.fingerprint.clone();
    media.source = Source::DualFisheye(Box::new(camera.clone()));
    let base = Project::untitled(media, INITIAL_VIEW).map_err(|e| read(e.to_string()))?;
    let id = base.the_clip().media;

    // Only the binding and the status: the evidence stays the table's.
    let planted = DualFisheye {
        geometry: Geometry {
            binding: Binding::Direct,
            status: Status::User,
            ..camera.geometry
        },
        ..camera
    };
    let tune = fixture_tune();
    let fixed_gains = Gains::Fixed {
        rgb: [[1.0; 3], [0.9; 3]],
    };
    let last = spec.frames.saturating_sub(1).max(1) as f32;
    let auto_gains = Gains::Auto {
        rate_hz: spec.rate.as_rational().to_f64(),
        log_ratio: (0..spec.frames)
            .map(|i| [0.2 * i as f32 / last; 3])
            .collect(),
        basis: gains_basis(&tune),
        input: AnalysisInput {
            file: Analysed::Master,
            fingerprint,
            frames: Sampling::Every { n: 1 },
        },
        estimator: 0,
    };
    let made = |gains: &Gains| -> Result<(Project, History), SynthError> {
        let mut p = base.clone();
        let mut h = History::new();
        let refused = |e: spherecut_core::project::EditError| read(e.to_string());
        h.record(
            &mut p,
            PLANTED_BINDING,
            Origin::User,
            &[Edit::SetSource {
                media: id,
                source: Source::DualFisheye(Box::new(planted.clone())),
            }],
        )
        .map_err(refused)?;
        h.record(
            &mut p,
            STITCH,
            Origin::User,
            &[Edit::SetStitch {
                media: id,
                tune: Some(tune),
                gains: Some(gains.clone()),
                profile: None,
            }],
        )
        .map_err(refused)?;
        Ok((p, h))
    };
    let (fixed, fixed_history) = made(&fixed_gains)?;
    let (auto, auto_history) = made(&auto_gains)?;
    Ok(StitchFixture {
        files,
        spec,
        fixed,
        fixed_history,
        auto,
        auto_history,
        tune,
        fixed_gains,
        auto_gains,
        front_track: 0,
        sides: [X5_LIKE_SIDE; 2],
    })
}
