//! The project model (M2 plan sections 3 to 5, DECISIONS D47 and D48).
//!
//! A [`Project`] holds media, one sequence with one track and one clip
//! (M2's limits, [`limits`]), and look-at targets. It changes only through
//! [`Project::apply`], which takes a list of [`Edit`]s and applies all of
//! them or none, so the GUI, the CLI and A2's agents change it the same
//! way; [`History`] keeps the undo steps, and [`file`] saves and loads it.
//!
//! Every clip, key, marker, target, media entry, sequence and track has an
//! id from the project's one counter, never reused, so an id names one
//! thing for the project's life and edits name ids, not positions. An id
//! of 0 in an edit means "give this a new id" ([`Project::apply`]).
//!
//! Times are exact ([`Time`]). A clip's keys and markers are in its
//! source's own time (0 at the source's first frame), so a trim never
//! moves them off the moment they were set on.

pub mod anim;
pub mod edit;
pub mod file;
pub mod folder;
pub mod history;
mod validate;

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use spherecut_sphere::perspective::{LensShape, Projection};

use crate::time::{FrameRate, Time};

pub use anim::{
    Aim, AimDir, Ease, Key, KeyTrack, MAX_AIM_PITCH_DEG, Orient, OrientMode, Orientation,
    OrientationError, Side, Target, View, ViewAnim, follow_spin,
};
pub use edit::{AnyKey, AnyKeys, AnyValue, Applied, Edit, EditError, PathPoint};
pub use folder::{
    By, FolderError, Holder, Identity, Kept, Loaded, ProjectFolder, ProjectLock, Saved,
};
pub use history::{Gesture, History, Origin, Step};
pub use validate::{Code, Problem};

/// The limits [`Project::validate`] holds a project to.
pub mod limits {
    /// Keys per track, and per target's path.
    pub const MAX_KEYS: usize = 10_000;
    /// Markers per clip.
    pub const MAX_MARKERS: usize = 1_000;
    /// Look-at targets per project.
    pub const MAX_TARGETS: usize = 100;
    /// Characters in a name.
    pub const MAX_NAME_CHARS: usize = 200;
    /// Extra whole turns on one segment of an orientation track.
    pub const MAX_SPIN: i8 = 8;
    /// The fastest sequence rate, frames per second.
    pub const MAX_RATE: i64 = 240;
    /// The largest id: JSON readers hold integers exactly up to 2^53.
    pub const MAX_ID: u64 = 1 << 53;
    /// A flat output's short side, in pixels (M2 plan section 8.1).
    pub const FLAT_SHORT_SIDES: [u32; 3] = [1080, 1440, 2160];
    /// An equirect output's width, in pixels; its height is half.
    pub const EQUIRECT_WIDTHS: [u32; 3] = [3840, 5760, 7680];
    /// M2 edits one clip: one sequence, one video track, one clip at 0.
    pub const SEQUENCES: usize = 1;
}

macro_rules! ids {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// In an edit, "give this a new id".
            pub const NEW: $name = $name(0);

            pub fn is_new(self) -> bool {
                self.0 == 0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    )*};
}

ids!(
    /// A media entry's id.
    MediaId,
    /// A sequence's id.
    SequenceId,
    /// A track's id.
    TrackId,
    /// A clip's id.
    ClipId,
    /// A key's id, on a clip's view track or a target's path.
    KeyId,
    /// A marker's id.
    MarkerId,
    /// A look-at target's id.
    TargetId,
);

/// A project. Its parts are read through the accessors and changed only
/// by [`Project::apply`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Raised by every applied list of edits, undo and redo included, and
    /// never lowered; A2's `if_revision` compares it.
    revision: u64,
    /// The next id to hand out. Ids are never reused, so undo never
    /// lowers it.
    next_id: u64,
    /// Sorted by id.
    media: Vec<Media>,
    /// M2: exactly one.
    sequences: Vec<Sequence>,
    /// Look-at targets, project-wide, sorted by id.
    targets: Vec<Target>,
}

/// A media file the project uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    #[serde(default)]
    pub id: MediaId,
    /// Absolute in memory; relative to the project file's folder when
    /// saved (M2 plan section 5.2).
    pub path: PathBuf,
    /// What the file was when it was added, to say when it has changed.
    pub fingerprint: Fingerprint,
    pub video: MediaVideo,
}

/// What identifies a media file's contents cheaply (M2 plan section 5.3):
/// its size and a hash of its first MiB, beside the video's own numbers
/// in [`MediaVideo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    /// The file's size in bytes.
    pub bytes: u64,
    /// FNV-1a 64 of the first [`file::HEAD_BYTES`] (or the whole file
    /// when shorter), as 16 hex digits.
    pub head: String,
}

/// The video stream a clip shows, as probed when the media was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaVideo {
    /// The stream's index in the file.
    pub stream: u32,
    pub width: u32,
    pub height: u32,
    /// The stream's frame rate (its average for a variable-rate stream).
    pub rate: FrameRate,
    /// From the first frame's start to the last frame's end, in clip time
    /// (0 at the first presented frame).
    pub duration: Time,
}

/// A sequence: what is exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    #[serde(default)]
    pub id: SequenceId,
    pub name: String,
    /// The export's frame rate: the clip's by default.
    pub rate: FrameRate,
    pub output: Output,
    /// The export choices last made for this sequence.
    pub export: ExportSettings,
    /// M2: one video track.
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    #[serde(default)]
    pub id: TrackId,
    pub kind: TrackKind,
    /// Sorted by `at`, then id. M2: one clip.
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
}

/// A clip: a trimmed piece of a media file, and how the camera looks at
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    #[serde(default)]
    pub id: ClipId,
    pub media: MediaId,
    /// The trim: the source's [start, end), in its own time.
    pub source: Range,
    /// Where the clip starts in the sequence. M2: always 0.
    pub at: Time,
    /// In the source's time, sorted by time, then id.
    pub markers: Vec<Marker>,
    pub view: ViewAnim,
}

/// A span of time, [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: Time,
    pub end: Time,
}

/// A named moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    #[serde(default)]
    pub id: MarkerId,
    pub at: Time,
    #[serde(default)]
    pub name: String,
}

/// What a sequence exports.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Output {
    /// A virtual camera's picture, `short_side` pixels on its short side
    /// (1080, 1440 or 2160).
    Flat { aspect: Aspect, short_side: u32 },
    /// The whole sphere, `width` × `width / 2`, turned so that
    /// `orientation` is its centre (D33's `world = q · view`).
    Equirect {
        width: u32,
        orientation: Orientation,
    },
}

/// A flat output's shape, width to height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aspect {
    #[serde(rename = "16:9")]
    Wide,
    #[serde(rename = "9:16")]
    Portrait,
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "4:5")]
    Tall,
}

impl Aspect {
    pub const ALL: [Aspect; 4] = [Aspect::Wide, Aspect::Portrait, Aspect::Square, Aspect::Tall];

    /// Width and height in whole numbers: 16:9 is (16, 9).
    pub fn ratio(self) -> (u32, u32) {
        match self {
            Aspect::Wide => (16, 9),
            Aspect::Portrait => (9, 16),
            Aspect::Square => (1, 1),
            Aspect::Tall => (4, 5),
        }
    }
}

impl Output {
    /// The default for a new sequence: flat 16:9 at 1080.
    pub const DEFAULT: Output = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };

    /// The picture's size in pixels, width then height.
    pub fn size(&self) -> (u32, u32) {
        match *self {
            Output::Flat { aspect, short_side } => {
                let (w, h) = aspect.ratio();
                let s = u64::from(short_side);
                // Exact for every listed size; validate refuses others.
                let long = |a: u32, b: u32| (s * u64::from(a) / u64::from(b)) as u32;
                if w >= h {
                    (long(w, h), short_side)
                } else {
                    (short_side, long(h, w))
                }
            }
            Output::Equirect { width, .. } => (width, width / 2),
        }
    }
}

/// The export choices kept with a sequence (M2 plan section 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSettings {
    pub codec: Codec,
    pub quality: Quality,
    pub encoder: EncoderChoice,
    pub audio: AudioChoice,
    pub motion_blur: bool,
}

impl ExportSettings {
    /// The defaults for an output: H.264 for flat, which everything
    /// plays; HEVC for 360, which hardware encodes above 4096 wide.
    pub fn default_for(output: &Output) -> ExportSettings {
        ExportSettings {
            codec: match output {
                Output::Flat { .. } => Codec::H264,
                Output::Equirect { .. } => Codec::Hevc,
            },
            quality: Quality::High,
            encoder: EncoderChoice::Auto,
            audio: AudioChoice::Copy,
            motion_blur: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    High,
    Medium,
    Small,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderChoice {
    /// The GPU's encoder where it has one for the codec, else software.
    Auto,
    Vaapi,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioChoice {
    /// The source's packets, trimmed exactly where the codec allows.
    Copy,
    /// Decoded and encoded again as AAC.
    Aac,
    None,
}

/// The view a new project starts with: what the viewer shows when the
/// clip is opened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InitialView {
    pub orientation: Orientation,
    /// Horizontal field of view, in degrees.
    pub fov_deg: f64,
    pub projection: Projection,
}

/// The view an untitled project starts with: the viewer's own default,
/// forward and level through a 100° rectilinear lens. The GUI and A2's
/// `project` create make the same project of a clip.
pub const INITIAL_VIEW: InitialView = InitialView {
    orientation: Orientation::IDENTITY,
    fov_deg: 100.0,
    projection: Projection::Rectilinear,
};

impl Project {
    /// The untitled project opening `media` makes (M2 plan section 3.2):
    /// one sequence at the clip's rate, flat 16:9 1080p, one track, one
    /// clip over the whole file, and `view` as the one key on each of the
    /// view's tracks, at the source's start. `media`'s id is given here.
    pub fn untitled(mut media: Media, view: InitialView) -> Result<Project, Problem> {
        // As a saved project loads it, so save then load gives it back.
        media.path = file::lexical(&media.path);
        let mut p = Project {
            revision: 0,
            next_id: 1,
            media: Vec::new(),
            sequences: Vec::new(),
            targets: Vec::new(),
        };
        let name = media
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Sequence".to_owned());
        // A file name may hold what a name may not.
        let name: String = name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(limits::MAX_NAME_CHARS)
            .collect();
        let name = if name.trim().is_empty() {
            "Sequence".to_owned()
        } else {
            name
        };
        let media_id = MediaId(p.take_id());
        let sequence = SequenceId(p.take_id());
        let track = TrackId(p.take_id());
        let clip = ClipId(p.take_id());
        let key = |p: &mut Project| KeyId(p.take_id());
        let anim = ViewAnim {
            mode: OrientMode::Level,
            orientation: KeyTrack::one(key(&mut p), Orient::View(view.orientation), Ease::SMOOTH),
            fov: KeyTrack::one(key(&mut p), view.fov_deg, Ease::SMOOTH),
            projection: KeyTrack::one(key(&mut p), LensShape::Pure(view.projection), Ease::STOP),
        };
        let rate = media.video.rate;
        let end = media.video.duration;
        p.media.push(Media {
            id: media_id,
            ..media
        });
        p.sequences.push(Sequence {
            id: sequence,
            name,
            rate,
            output: Output::DEFAULT,
            export: ExportSettings::default_for(&Output::DEFAULT),
            tracks: vec![Track {
                id: track,
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: clip,
                    media: media_id,
                    source: Range {
                        start: Time::ZERO,
                        end,
                    },
                    at: Time::ZERO,
                    markers: Vec::new(),
                    view: anim,
                }],
            }],
        });
        p.validate()?;
        Ok(p)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn media(&self) -> &[Media] {
        &self.media
    }

    pub fn sequences(&self) -> &[Sequence] {
        &self.sequences
    }

    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    pub fn media_by_id(&self, id: MediaId) -> Option<&Media> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn target(&self, id: TargetId) -> Option<&Target> {
        self.targets.iter().find(|t| t.id == id)
    }

    /// Every clip, in sequence and track order.
    pub fn clips(&self) -> impl Iterator<Item = &Clip> {
        self.sequences
            .iter()
            .flat_map(|s| &s.tracks)
            .flat_map(|t| &t.clips)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips().find(|c| c.id == id)
    }

    /// M2's one clip, which [`Project::validate`] makes sure is there.
    pub fn the_clip(&self) -> &Clip {
        &self.sequences[0].tracks[0].clips[0]
    }

    /// M2's one sequence.
    pub fn the_sequence(&self) -> &Sequence {
        &self.sequences[0]
    }

    /// Whether `other` holds the same things, whatever the two revisions
    /// and id counters: what undoing every step gives back.
    pub fn same_content(&self, other: &Project) -> bool {
        self.media == other.media
            && self.sequences == other.sequences
            && self.targets == other.targets
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.sequences
            .iter_mut()
            .flat_map(|s| &mut s.tracks)
            .flat_map(|t| &mut t.clips)
            .find(|c| c.id == id)
    }

    pub(crate) fn sequence_mut(&mut self, id: SequenceId) -> Option<&mut Sequence> {
        self.sequences.iter_mut().find(|s| s.id == id)
    }
}

impl Clip {
    /// The clip's length in the sequence.
    pub fn duration(&self) -> Time {
        // validate keeps start < end, both small enough to subtract.
        self.source
            .end
            .checked_sub(self.source.start)
            .unwrap_or(Time::ZERO)
    }

    pub fn marker(&self, id: MarkerId) -> Option<&Marker> {
        self.markers.iter().find(|m| m.id == id)
    }
}

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod tests {
    use super::*;
    use testutil::{media, view};

    #[test]
    fn output_sizes() {
        let flat = |aspect, short_side| Output::Flat { aspect, short_side }.size();
        assert_eq!(flat(Aspect::Wide, 1080), (1920, 1080));
        assert_eq!(flat(Aspect::Wide, 1440), (2560, 1440));
        assert_eq!(flat(Aspect::Wide, 2160), (3840, 2160));
        assert_eq!(flat(Aspect::Portrait, 1080), (1080, 1920));
        assert_eq!(flat(Aspect::Square, 1440), (1440, 1440));
        assert_eq!(flat(Aspect::Tall, 1080), (1080, 1350));
        assert_eq!(flat(Aspect::Tall, 2160), (2160, 2700));
        let eq = Output::Equirect {
            width: 7680,
            orientation: Orientation::IDENTITY,
        };
        assert_eq!(eq.size(), (7680, 3840));
        // Every listed size is a multiple of 4 wide and even tall, as the
        // YUV pass needs (M2 plan section 7.3).
        for aspect in Aspect::ALL {
            for s in limits::FLAT_SHORT_SIDES {
                let (w, h) = flat(aspect, s);
                assert!(w % 4 == 0 && h % 2 == 0, "{aspect:?} {s}: {w}x{h}");
            }
        }
    }

    #[test]
    fn untitled_project() {
        let p = Project::untitled(media("/clips/VID_1.mp4", 10), view()).unwrap();
        assert_eq!(p.revision(), 0);
        assert_eq!(p.next_id(), 8);
        let s = p.the_sequence();
        assert_eq!(s.name, "VID_1");
        assert_eq!(s.rate, FrameRate::FPS_30);
        assert_eq!(s.output, Output::DEFAULT);
        let c = p.the_clip();
        assert_eq!(c.media, p.media()[0].id);
        assert_eq!(c.source.start, Time::ZERO);
        assert_eq!(c.source.end, Time::from_secs(10));
        assert_eq!(c.view.orientation.keys.len(), 1);
        assert_eq!(c.view.projection.keys[0].ease, Ease::STOP);
        let mut ids: Vec<u64> = vec![p.media()[0].id.0, s.id.0, s.tracks[0].id.0, c.id.0];
        ids.extend(c.view.orientation.keys.iter().map(|k| k.id.0));
        ids.extend(c.view.fov.keys.iter().map(|k| k.id.0));
        ids.extend(c.view.projection.keys.iter().map(|k| k.id.0));
        assert_eq!(ids, (1..8).collect::<Vec<_>>());
    }

    #[test]
    fn untitled_refuses_a_view_it_cannot_hold() {
        let mut v = view();
        v.fov_deg = 2.0;
        let e = Project::untitled(media("/clips/a.mp4", 10), v).unwrap_err();
        assert_eq!(e.code, Code::OutOfRange);
        let e = Project::untitled(media("relative.mp4", 10), view()).unwrap_err();
        assert_eq!(e.code, Code::Invalid);
    }
}
