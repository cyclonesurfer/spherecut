//! The checks of a finished export before it is put in place (M2 plan
//! section 9, amendment 2 item 53): read back through media's probe, the
//! file is whole, its moov comes before its media data, and it has the
//! frames, duration (within a frame), codec, size and rate asked for, the
//! audio planned, and for a 360 export the spherical metadata as it was
//! written: `st3d` and `sv3d`, the V1 `uuid`, and `SA3D` on ambiX audio.
//! A failed check fails the export, and the file is never put in place.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::project::Codec;
use spherecut_core::time::{FrameRate, Time};
use spherecut_media::Source;
use spherecut_media::probe::{self, StreamInfo, StreamKind};
use spherecut_media::spherical::{Projection, StereoMode};

use crate::error::{ExportError, Stage};

/// A check the finished file passed, and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// `probe`, `layout`, `codec`, `size`, `frames`, `duration`, `rate`,
    /// `st3d`, `sv3d`, `v1` or `spherical` (none, for a flat export),
    /// `audio`, `sa3d`.
    pub name: &'static str,
    pub found: String,
}

/// What the file should be.
#[derive(Debug, Clone)]
pub struct Expected {
    pub codec: Codec,
    pub size: (u32, u32),
    pub frames: u64,
    pub duration: Time,
    pub rate: FrameRate,
    /// Equirect, with spherical metadata.
    pub sphere: bool,
    pub audio: Option<ExpectedAudio>,
}

#[derive(Debug, Clone)]
pub struct ExpectedAudio {
    /// FFmpeg's name for the codec.
    pub codec: String,
    pub rate: u32,
    pub channels: u16,
    /// First-order ambiX, with an `SA3D` box.
    pub ambix: bool,
}

/// FFmpeg's name for `codec`, as probe reports it.
fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    }
}

/// The checks passed so far.
struct Checks(Vec<Checked>);

impl Checks {
    /// Pass `name` if `ok`, with what was `found`, or fail the export
    /// saying what was `wanted`.
    fn expect(
        &mut self,
        name: &'static str,
        ok: bool,
        found: String,
        wanted: impl FnOnce() -> String,
    ) -> Result<(), ExportError> {
        if !ok {
            return Err(failed(
                name,
                format!("the file has {found}, not {}", wanted()),
            ));
        }
        self.0.push(Checked { name, found });
        Ok(())
    }
}

fn failed(name: &str, message: String) -> ExportError {
    ExportError::failed(Stage::Check, None, format!("{name}: {message}"))
}

/// The one stream of `kind`, or a failed check `name`.
fn only<'a>(
    streams: &'a [StreamInfo],
    kind: StreamKind,
    name: &str,
) -> Result<&'a StreamInfo, ExportError> {
    let of: Vec<_> = streams.iter().filter(|s| s.kind == kind).collect();
    match of.as_slice() {
        [s] => Ok(s),
        _ => Err(failed(
            name,
            format!("the file has {} such streams, not one", of.len()),
        )),
    }
}

/// Check the file at `path` against `want`: the checks passed, or the
/// first that failed, as a [`Stage::Check`] error naming it. FFmpeg's
/// reads stop once `interrupt` is raised.
pub fn check(
    path: &Path,
    want: &Expected,
    interrupt: Arc<AtomicBool>,
) -> Result<Vec<Checked>, ExportError> {
    let report = probe::probe_cancellable(Source::path(path), interrupt)
        .map_err(|e| failed("probe", format!("the file doesn't read back: {e}")))?;
    if let Some(p) = report.failure() {
        return Err(failed("probe", p.message));
    }
    let mut c = Checks(vec![Checked {
        name: "probe",
        found: format!("{} bytes, whole", report.file_size),
    }]);

    let top: Vec<&str> = report
        .mp4
        .iter()
        .flat_map(|m| m.top_level.iter().map(|(t, _)| t.as_str()))
        .collect();
    let at = |t| top.iter().position(|&b| b == t);
    let moov_first = matches!((at("moov"), at("mdat")), (Some(m), Some(d)) if m < d);
    c.expect("layout", moov_first, top.join(", "), || {
        "the moov before the media data".into()
    })?;

    let v = only(&report.streams, StreamKind::Video, "video")?;
    let codec = v.codec.clone().unwrap_or_else(|| "no codec".into());
    let wanted = codec_name(want.codec);
    c.expect("codec", codec == wanted, codec, || wanted.into())?;
    let size = v.video.as_ref().map(|i| (i.width, i.height));
    let (w, h) = want.size;
    c.expect(
        "size",
        size == Some(want.size),
        size.map_or("no size".into(), |(sw, sh)| format!("{sw}x{sh}")),
        || format!("{w}x{h}"),
    )?;
    let frames = v.frames.and_then(|n| u64::try_from(n).ok());
    c.expect(
        "frames",
        frames == Some(want.frames),
        frames.map_or("no frame count".into(), |n| format!("{n} frames")),
        || format!("{} frames", want.frames),
    )?;
    let off = v.duration.map(|d| {
        let (a, b) = if d > want.duration {
            (d, want.duration)
        } else {
            (want.duration, d)
        };
        a.checked_sub(b)
    });
    let within = matches!(off, Some(Ok(o)) if o <= want.rate.frame_duration());
    c.expect(
        "duration",
        within,
        v.duration.map_or("no duration".into(), |d| {
            format!("{:.6} s", d.to_secs_f64())
        }),
        || format!("{:.6} s within a frame", want.duration.to_secs_f64()),
    )?;
    let rate = want.rate.as_rational();
    c.expect(
        "rate",
        v.avg_frame_rate == Some(rate),
        v.avg_frame_rate
            .map_or("no rate".into(), |r| format!("{r} fps")),
        || format!("{rate} fps"),
    )?;

    let sph = &v.spherical;
    if want.sphere {
        let v2 = sph.v2.as_ref();
        let stereo = v2.and_then(|s| s.stereo);
        c.expect(
            "st3d",
            stereo == Some(StereoMode::Mono),
            stereo.map_or("no st3d".into(), |s| s.name().into()),
            || "mono".into(),
        )?;
        let projection = v2.and_then(|s| s.projection.as_ref());
        let whole = matches!(
            projection,
            Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0
            })
        );
        let level =
            v2.is_some_and(|s| s.pose_yaw == 0.0 && s.pose_pitch == 0.0 && s.pose_roll == 0.0);
        c.expect(
            "sv3d",
            whole && level,
            match (projection, v2) {
                (Some(p), Some(s)) => format!(
                    "{p:?} at a pose of {}, {}, {}",
                    s.pose_yaw, s.pose_pitch, s.pose_roll
                ),
                _ => "no sv3d".into(),
            },
            || "the whole sphere, equirect, at no pose".into(),
        )?;
        let v1 = sph.v1.as_ref();
        let v1_ok = v1.is_some_and(|v| {
            v.spherical && v.stitched && v.projection_type.as_deref() == Some("equirectangular")
        });
        c.expect(
            "v1",
            v1_ok,
            v1.map_or("no V1 uuid".into(), |v| {
                format!(
                    "spherical {}, stitched {}, {}",
                    v.spherical,
                    v.stitched,
                    v.projection_type.as_deref().unwrap_or("no projection")
                )
            }),
            || "a stitched equirectangular sphere".into(),
        )?;
    } else {
        c.expect(
            "spherical",
            sph.is_empty(),
            if sph.is_empty() {
                "none".into()
            } else {
                "spherical metadata".into()
            },
            || "none on a flat export".into(),
        )?;
    }

    match &want.audio {
        None => {
            let n = report
                .streams
                .iter()
                .filter(|s| s.kind == StreamKind::Audio)
                .count();
            c.expect("audio", n == 0, format!("{n} audio streams"), || {
                "none".into()
            })?;
        }
        Some(a) => {
            let s = only(&report.streams, StreamKind::Audio, "audio")?;
            let codec = s.codec.as_deref().unwrap_or("no codec");
            let (rate, channels) = s
                .audio
                .as_ref()
                .map_or((0, 0), |i| (i.sample_rate, i.channels));
            c.expect(
                "audio",
                codec == a.codec && rate == a.rate && channels == u32::from(a.channels),
                format!("{codec}, {rate} Hz, {channels} channels"),
                || format!("{}, {} Hz, {} channels", a.codec, a.rate, a.channels),
            )?;
            if a.ambix {
                let sa = s.spherical.spatial_audio.as_ref();
                c.expect(
                    "sa3d",
                    sa.is_some_and(|sa| sa.is_ambix() && sa.ambisonic_order == 1),
                    sa.map_or("no SA3D".into(), |sa| {
                        format!("order {}, ambiX {}", sa.ambisonic_order, sa.is_ambix())
                    }),
                    || "first-order ambiX".into(),
                )?;
            }
        }
    }
    Ok(c.0)
}
