//! The edit reference (a2-plan 4.2): every `Edit` op with what it does
//! and one example, as `project`'s `edits` part gives them and the tests
//! apply them.
//!
//! The examples are written as an agent writes them (4.1: A1's times, ids
//! left out of new things) and name the ids of [`project`], the untitled
//! project of a 10 s clip with a few things added, so that each applies.

use serde_json::{Value, json};
use spherecut_core::project::{
    Edit, Fingerprint, History, INITIAL_VIEW, Media, MediaId, MediaVideo, Origin, Project,
};
use spherecut_core::{FrameRate, Time};

/// One op: its name, what it does, and an example.
#[derive(Debug, Clone, Copy)]
pub struct Op {
    pub op: &'static str,
    pub does: &'static str,
    pub example: &'static str,
}

/// What the ids in the examples are.
pub const IDS: &str = "The examples name media 1, sequence 2, track 3, clip 4, its fov key 6, \
                       an orientation key 8 at 5 s, a marker 9 and a target 10.";

/// How times are written, word for word everywhere a time is described.
pub const TIMES: &str = "A time (at, start, end) is the clip's file time, as look's: \"12.5\", \
                         \"1:02.5\", \"30000/1001\", \"120f\" (frames at the media's rate; a \
                         sequence's rate for add_clip's at), [num, den], or {\"exact\": \"25/2\"} \
                         as project gives them. A bare number is refused.";

/// Every op, in `Edit`'s order.
pub const OPS: &[Op] = &[
    Op {
        op: "add_media",
        does: "Add a video to the project, by path (probed beneath the roots). M2's one clip \
               uses only the first.",
        example: r#"{"op": "add_media", "path": "clips/b.mp4"}"#,
    },
    Op {
        op: "remove_media",
        does: "Remove a video no clip uses.",
        example: r#"{"op": "remove_media", "media": 1}"#,
    },
    Op {
        op: "add_clip",
        does: "Add a clip to a track. Refused (limit) until M3: a sequence has one clip.",
        example: r#"{"op": "add_clip", "track": 3, "clip": {"media": 1, "source": {"start": "0", "end": "5"}, "at": "10", "markers": [], "view": {"mode": "level", "orientation": [{"at": "0", "value": {"view": {"ypr_deg": [0, 0, 0]}}}], "fov": [{"at": "0", "value": 100}], "projection": [{"at": "0", "value": "rectilinear"}]}}}"#,
    },
    Op {
        op: "remove_clip",
        does: "Remove a clip with its keys and markers. Refused (limit) until M3.",
        example: r#"{"op": "remove_clip", "clip": 4}"#,
    },
    Op {
        op: "set_trim",
        does: "The clip's in and out in its file; either may be left out.",
        example: r#"{"op": "set_trim", "clip": 4, "start": "45f", "end": "9.5"}"#,
    },
    Op {
        op: "set_output",
        does: "What the sequence renders: flat (aspect 16:9, 9:16, 1:1 or 4:5; short_side \
               1080, 1440 or 2160) or equirect (width 3840, 5760 or 7680, turned to \
               orientation).",
        example: r#"{"op": "set_output", "sequence": 2, "output": {"kind": "flat", "aspect": "9:16", "short_side": 1440}}"#,
    },
    Op {
        op: "set_rate",
        does: "The sequence's frame rate.",
        example: r#"{"op": "set_rate", "sequence": 2, "rate": "60"}"#,
    },
    Op {
        op: "set_export",
        does: "Codec (h264, hevc, av1), quality (high, medium, small), encoder (auto, vaapi, \
               software), audio (copy, aac, none) and motion blur.",
        example: r#"{"op": "set_export", "sequence": 2, "export": {"codec": "hevc", "quality": "high", "encoder": "auto", "audio": "copy", "motion_blur": true}}"#,
    },
    Op {
        op: "add_key",
        does: "A key on one of the clip's three view tracks (orientation, fov in degrees, \
               projection), at a time without one. Ease into and out of it: smooth, linear \
               or stop.",
        example: r#"{"op": "add_key", "clip": 4, "key": {"orientation": {"at": "2.5", "value": {"view": {"ypr_deg": [90, -10, 0]}}, "ease": {"into": "smooth", "out": "linear"}}}}"#,
    },
    Op {
        op: "set_key",
        does: "Change a key's time, value, ease or spin (extra whole turns to the next key); \
               what is left out stays.",
        example: r#"{"op": "set_key", "key": 8, "at": [11, 2], "value": {"orientation": {"aim": {"target": 10, "roll_deg": 0}}}}"#,
    },
    Op {
        op: "remove_key",
        does: "Remove a key; a track keeps at least one.",
        example: r#"{"op": "remove_key", "key": 8}"#,
    },
    Op {
        op: "set_keys",
        does: "Replace all of one track's keys; a key keeps its id when given one of the \
               track's.",
        example: r#"{"op": "set_keys", "clip": 4, "keys": {"fov": [{"id": 6, "at": "0", "value": 100}, {"at": "4", "value": 70}]}}"#,
    },
    Op {
        op: "set_orient_mode",
        does: "How orientation keys join: level (yaw, pitch and roll as numbers, so level \
               moves stay level) or free (along the sphere).",
        example: r#"{"op": "set_orient_mode", "clip": 4, "mode": "free"}"#,
    },
    Op {
        op: "set_view_path",
        does: "Many orientation keys at once, one a point, with a fov key where a point has \
               one: following something. With replace, the keys from the first point to the \
               last go first.",
        example: r#"{"op": "set_view_path", "clip": 4, "points": [{"at": "6", "orientation": {"ypr_deg": [10, 0, 0]}, "fov": 90}, {"at": "6.5", "orientation": {"ypr_deg": [20, -2, 0]}}], "replace": true}"#,
    },
    Op {
        op: "add_marker",
        does: "A named moment.",
        example: r#"{"op": "add_marker", "clip": 4, "marker": {"at": "2", "name": "the jump"}}"#,
    },
    Op {
        op: "set_marker",
        does: "Move or rename a marker.",
        example: r#"{"op": "set_marker", "marker": 9, "at": "3.5", "name": "landing"}"#,
    },
    Op {
        op: "remove_marker",
        does: "Remove a marker.",
        example: r#"{"op": "remove_marker", "marker": 9}"#,
    },
    Op {
        op: "add_target",
        does: "Something to look at, in one video, as yaw and pitch over its time: one key \
               for a fixed point, more for a moving one. Orientation keys aim at it.",
        example: r#"{"op": "add_target", "target": {"name": "boat", "media": 1, "path": [{"at": "0", "value": {"yaw_deg": 45, "pitch_deg": -5}}]}}"#,
    },
    Op {
        op: "set_target",
        does: "Rename a target or replace its path.",
        example: r#"{"op": "set_target", "target": 10, "path": [{"at": "0", "value": {"yaw_deg": 30, "pitch_deg": -10}}, {"at": "4", "value": {"yaw_deg": 60, "pitch_deg": -8}}]}"#,
    },
    Op {
        op: "remove_target",
        does: "Remove a target no key aims at.",
        example: r#"{"op": "remove_target", "target": 10}"#,
    },
    Op {
        op: "rename",
        does: "Rename the sequence, a marker or a target.",
        example: r#"{"op": "rename", "id": 2, "name": "Cliff jump"}"#,
    },
];

/// `project`'s `edits` part.
pub fn part() -> Value {
    json!({
        "note": "Edits are applied in order, all or none, as one undo step. Ids are \
                 project's; give none (or 0) for something new, and the result gives the \
                 ids made. Orientations are {\"ypr_deg\": [yaw, pitch, roll]} (look's angles) \
                 or {\"quat\": [x, y, z, w]}.",
        "times": TIMES,
        "ids": IDS,
        "ops": OPS
            .iter()
            .map(|o| json!({
                "op": o.op,
                "does": o.does,
                "example": example(o),
            }))
            .collect::<Vec<_>>(),
    })
}

/// `o`'s example as JSON. The examples are constants, which the tests
/// parse, so a failure here is a bug.
pub fn example(o: &Op) -> Value {
    serde_json::from_str(o.example).unwrap_or_else(|e| {
        tracing::error!("mcp: {}'s example doesn't parse: {e}", o.op);
        Value::Null
    })
}

/// The project the examples' ids name: the untitled project of `media`
/// (10 s at 30 fps, if it is the [`media`] here) with an orientation key
/// at 5 s (8), a marker (9) and a target (10), made as the user would
/// make them, and the history that made them.
pub fn project(media: Media) -> Result<(Project, History), String> {
    let mut project = Project::untitled(media, INITIAL_VIEW).map_err(|p| p.to_string())?;
    let mut history = History::new();
    let made: Vec<Edit> = serde_json::from_value(json!([
        {"op": "add_key", "clip": 4, "key": {"orientation": {
            "at": {"exact": "5"}, "value": {"view": {"ypr_deg": [45, 0, 0]}}}}},
        {"op": "add_marker", "clip": 4, "marker": {"at": {"exact": "3"}, "name": "drop"}},
        {"op": "add_target", "target": {"name": "rider", "media": 1, "path": [
            {"at": {"exact": "0"}, "value": {"yaw_deg": 20, "pitch_deg": -5}}]}},
    ]))
    .map_err(|e| e.to_string())?;
    history
        .record(&mut project, "Set up", Origin::User, &made)
        .map_err(|e| e.to_string())?;
    Ok((project, history))
}

/// A media entry of `path`, 3840×1920 at 30 fps for 10 s, for tests and
/// for [`project`] without a file to probe.
pub fn media(path: &std::path::Path) -> Media {
    Media {
        id: MediaId::NEW,
        path: path.to_path_buf(),
        fingerprint: Fingerprint {
            bytes: 1,
            head: "0000000000000000".into(),
        },
        video: MediaVideo {
            stream: 0,
            width: 3840,
            height: 1920,
            rate: FrameRate::FPS_30,
            duration: Time::from_secs(10),
        },
    }
}
