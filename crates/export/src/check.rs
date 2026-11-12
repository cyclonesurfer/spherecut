//! The checks of a finished export before it is put in place (M2 plan
//! section 9, amendment 2 item 53): read back through media's probe, the
//! file is whole, its moov comes before its media data, and it has the
//! frames, duration (within a frame), codec, size and rate asked for, the
//! audio planned, and for a 360 export the spherical metadata as it was
//! written: `st3d` and `sv3d`, the V1 `uuid`, and `SA3D` on ambiX audio.
//! Probe reads an MP4's spherical boxes with our own box reader (D4), so
//! the file is then read a second time by FFmpeg's own demuxer
//! ([`probe::demuxer_view`]), which is what a player built on FFmpeg sees:
//! its spherical side data, its ambisonic layout, and no complaint of a
//! spherical box in its log ([`check_demuxer_view`], F123). A failed check
//! fails the export, and the file is never put in place.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::project::Codec;
use spherecut_core::time::{FrameRate, Time};
use spherecut_media::Source;
use spherecut_media::probe::{self, DemuxerView, StreamInfo, StreamKind};
use spherecut_media::spherical::{Projection, SpatialAudio, SphericalV2, StereoMode};

use crate::error::{ExportError, Stage};

/// A check the finished file passed, and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// `probe`, `layout`, `codec`, `size`, `frames`, `duration`, `rate`,
    /// `st3d`, `sv3d`, `v1` or `spherical` (none, for a flat export),
    /// `audio`, `sa3d`, then what FFmpeg's own demuxer reads:
    /// `ffmpeg_sphere`, `ffmpeg_ambix` (for ambiX audio) and `ffmpeg_log`.
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
        self.expect_by(name, "the file has", ok, found, wanted)
    }

    /// [`expect`](Self::expect), saying who found it: "`by` {found}, not
    /// {wanted}".
    fn expect_by(
        &mut self,
        name: &'static str,
        by: &str,
        ok: bool,
        found: String,
        wanted: impl FnOnce() -> String,
    ) -> Result<(), ExportError> {
        if !ok {
            return Err(failed(name, format!("{by} {found}, not {}", wanted())));
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
    let report = probe::probe_cancellable(Source::path(path), interrupt.clone())
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
                // As FFmpeg 6.1's `mov_read_SA3D` takes it (F124): it
                // ignores another version, head-locked stereo and a
                // channel map that reorders.
                let sa = s.spherical.spatial_audio.as_ref();
                let identity =
                    |sa: &SpatialAudio| sa.channel_map.iter().copied().eq(0..u32::from(a.channels));
                c.expect(
                    "sa3d",
                    sa.is_some_and(|sa| {
                        sa.version == 0
                            && !sa.head_locked_stereo
                            && sa.is_ambix()
                            && sa.ambisonic_order == 1
                            && identity(sa)
                    }),
                    sa.map_or("no SA3D".into(), |sa| {
                        format!(
                            "version {}, order {}, ambiX {}, head-locked stereo {}, channel map {:?}",
                            sa.version,
                            sa.ambisonic_order,
                            sa.is_ambix(),
                            sa.head_locked_stereo,
                            sa.channel_map
                        )
                    }),
                    || {
                        format!(
                            "version 0, first-order ambiX with no head-locked stereo, channel map {:?}",
                            (0..u32::from(a.channels)).collect::<Vec<_>>()
                        )
                    },
                )?;
            }
        }
    }

    let view = probe::demuxer_view(Source::path(path), Some(interrupt)).map_err(|e| {
        failed(
            "ffmpeg_sphere",
            format!("FFmpeg's demuxer doesn't open it: {e}"),
        )
    })?;
    c.0.extend(check_demuxer_view(&view, want)?);
    Ok(c.0)
}

/// Whether a line in which FFmpeg's demuxer complains of a spherical box
/// fails the export (`ffmpeg_log`). When it doesn't, the check passes,
/// and what it found lists the lines. A complaint means FFmpeg read a box
/// we wrote otherwise than we meant, so for now it fails; Pat may make it
/// a warning by setting this to `false`.
const SPHERICAL_COMPLAINT_FAILS: bool = true;

/// The checks of what FFmpeg's own demuxer reads of the file
/// ([`probe::demuxer_view`]), against `want`:
///
/// - `ffmpeg_sphere`: the one video stream has FFmpeg's spherical side
///   data, the whole sphere equirect, mono, at no pose, for a 360 export,
///   and none for a flat one;
/// - `ffmpeg_ambix`, for ambiX audio only: the one audio stream's layout
///   is "ambisonic 1", which FFmpeg gives only from an `SA3D` box it
///   takes;
/// - `ffmpeg_log`: no line of the log complains of a spherical box
///   ([`DemuxerView::spherical_complaints`]).
///
/// The checks passed, or the first that failed, as [`check`] gives them.
pub fn check_demuxer_view(
    view: &DemuxerView,
    want: &Expected,
) -> Result<Vec<Checked>, ExportError> {
    const BY: &str = "FFmpeg's demuxer reads";
    let mut c = Checks(Vec::new());

    let video: Vec<_> = view
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .collect();
    let spherical = match video.as_slice() {
        [v] => v.spherical.as_ref(),
        _ => {
            return Err(failed(
                "ffmpeg_sphere",
                format!("{BY} {} video streams, not one", video.len()),
            ));
        }
    };
    let found = spherical.map_or_else(|| "no spherical side data".into(), describe);
    if want.sphere {
        let whole = spherical.is_some_and(|s| {
            matches!(
                s.projection,
                Some(Projection::Equirect {
                    top: 0,
                    bottom: 0,
                    left: 0,
                    right: 0
                })
            ) && s.stereo == Some(StereoMode::Mono)
                && s.pose_yaw == 0.0
                && s.pose_pitch == 0.0
                && s.pose_roll == 0.0
        });
        c.expect_by("ffmpeg_sphere", BY, whole, found, || {
            "the whole sphere, equirect, mono, at no pose".into()
        })?;
    } else {
        c.expect_by("ffmpeg_sphere", BY, spherical.is_none(), found, || {
            "no spherical side data on a flat export".into()
        })?;
    }

    if want.audio.as_ref().is_some_and(|a| a.ambix) {
        let audio: Vec<_> = view
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Audio)
            .collect();
        let layout = match audio.as_slice() {
            [a] => a.channel_layout.as_deref(),
            _ => {
                return Err(failed(
                    "ffmpeg_ambix",
                    format!("{BY} {} audio streams, not one", audio.len()),
                ));
            }
        };
        c.expect_by(
            "ffmpeg_ambix",
            BY,
            layout == Some("ambisonic 1"),
            layout.map_or_else(
                || "no channel layout".into(),
                |l| format!("a layout of {l}"),
            ),
            || "a layout of ambisonic 1".into(),
        )?;
    }

    let complaints = view.spherical_complaints();
    c.expect_by(
        "ffmpeg_log",
        "FFmpeg's demuxer logs",
        complaints.is_empty() || !SPHERICAL_COMPLAINT_FAILS,
        if complaints.is_empty() {
            "no complaint of a spherical box".into()
        } else {
            complaints.join("; ")
        },
        || "no complaint of a spherical box".into(),
    )?;
    Ok(c.0)
}

/// FFmpeg's spherical mapping, in words: "equirect, mono, at no pose".
fn describe(s: &SphericalV2) -> String {
    let projection = match &s.projection {
        Some(Projection::Equirect {
            top: 0,
            bottom: 0,
            left: 0,
            right: 0,
        }) => "equirect".into(),
        Some(Projection::Equirect {
            top,
            bottom,
            left,
            right,
        }) => format!("an equirect tile cropped {top}, {bottom}, {left}, {right}"),
        Some(Projection::Cubemap { layout, padding }) => {
            format!("a cubemap of layout {layout}, padding {padding}")
        }
        Some(Projection::Mesh) => "a mesh".into(),
        Some(Projection::Other { fourcc }) => format!("a {fourcc} projection"),
        None => "no projection".into(),
    };
    let stereo = s.stereo.map_or("no stereo mode", StereoMode::name);
    let pose = if s.pose_yaw == 0.0 && s.pose_pitch == 0.0 && s.pose_roll == 0.0 {
        "no pose".into()
    } else {
        format!(
            "a pose of {}, {}, {}",
            s.pose_yaw, s.pose_pitch, s.pose_roll
        )
    };
    format!("{projection}, {stereo}, at {pose}")
}

#[cfg(test)]
mod tests {
    use spherecut_core::time::{FrameRate, Time};
    use spherecut_media::probe::{DemuxedStream, DemuxerView, StreamKind};
    use spherecut_media::spherical::{Projection, SphericalV2, StereoMode};
    use spherecut_testkit::control::must_fail;

    use super::{Expected, ExpectedAudio, check_demuxer_view};

    /// What the export wants: 360 or flat, with first-order ambiX audio
    /// or stereo.
    fn want(sphere: bool, ambix: bool) -> Expected {
        Expected {
            codec: spherecut_core::project::Codec::Hevc,
            size: (3840, 1920),
            frames: 15,
            duration: Time::from_secs(1),
            rate: FrameRate::FPS_30,
            sphere,
            audio: Some(ExpectedAudio {
                codec: "aac".into(),
                rate: 48_000,
                channels: if ambix { 4 } else { 2 },
                ambix,
            }),
        }
    }

    /// The whole sphere, equirect, mono, at no pose, as FFmpeg exports it.
    fn whole() -> SphericalV2 {
        SphericalV2 {
            stereo: Some(StereoMode::Mono),
            metadata_source: None,
            pose_yaw: 0.0,
            pose_pitch: 0.0,
            pose_roll: 0.0,
            projection: Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0,
            }),
        }
    }

    /// A view of a video stream with `spherical` and an audio stream laid
    /// out as `layout`, with nothing logged.
    fn view(spherical: Option<SphericalV2>, layout: &str) -> DemuxerView {
        DemuxerView {
            streams: vec![
                DemuxedStream {
                    index: 0,
                    kind: StreamKind::Video,
                    spherical,
                    channel_layout: None,
                },
                DemuxedStream {
                    index: 1,
                    kind: StreamKind::Audio,
                    spherical: None,
                    channel_layout: Some(layout.into()),
                },
            ],
            ffmpeg_log: Vec::new(),
        }
    }

    /// The name of the check that fails `view` against `want`, and its
    /// message; panics if every check passes.
    fn fails(what: &str, view: &DemuxerView, want: &Expected) -> String {
        must_fail(what, check_demuxer_view(view, want)).to_string()
    }

    /// FFmpeg's reading passes as the export wrote it: a 360 file with
    /// ambiX audio, and a flat one with stereo.
    ///
    /// Controls: each of FFmpeg's other readings fails, by the check
    /// that names it.
    #[test]
    fn ffmpeg_reading_passes_only_as_written() {
        let good =
            check_demuxer_view(&view(Some(whole()), "ambisonic 1"), &want(true, true)).unwrap();
        let names: Vec<_> = good.iter().map(|c| c.name).collect();
        assert_eq!(names, ["ffmpeg_sphere", "ffmpeg_ambix", "ffmpeg_log"]);
        assert_eq!(good[0].found, "equirect, mono, at no pose");
        assert_eq!(good[1].found, "a layout of ambisonic 1");
        let flat = check_demuxer_view(&view(None, "stereo"), &want(false, false)).unwrap();
        let names: Vec<_> = flat.iter().map(|c| c.name).collect();
        assert_eq!(names, ["ffmpeg_sphere", "ffmpeg_log"]);
        assert_eq!(flat[0].found, "no spherical side data");

        let sphere = want(true, true);
        let with = |f: fn(&mut SphericalV2)| {
            let mut s = whole();
            f(&mut s);
            view(Some(s), "ambisonic 1")
        };
        let mut two = view(Some(whole()), "ambisonic 1");
        two.streams.push(DemuxedStream {
            index: 2,
            ..two.streams[0].clone()
        });
        let mut complaint = view(Some(whole()), "ambisonic 1");
        complaint
            .ffmpeg_log
            .push("[mov,mp4,m4a,3gp,3g2,mj2 @ 0x1] Missing projection header box".into());
        for (what, v, want, check) in [
            (
                "a cubemap",
                with(|s| {
                    s.projection = Some(Projection::Cubemap {
                        layout: 0,
                        padding: 0,
                    });
                }),
                &sphere,
                "ffmpeg_sphere",
            ),
            (
                "an equirect tile",
                with(|s| {
                    s.projection = Some(Projection::Equirect {
                        top: 1 << 30,
                        bottom: 0,
                        left: 0,
                        right: 0,
                    });
                }),
                &sphere,
                "ffmpeg_sphere",
            ),
            (
                "a yaw of 90",
                with(|s| s.pose_yaw = 90.0),
                &sphere,
                "ffmpeg_sphere",
            ),
            (
                "top-bottom stereo",
                with(|s| s.stereo = Some(StereoMode::TopBottom)),
                &sphere,
                "ffmpeg_sphere",
            ),
            (
                "no stereo mode",
                with(|s| s.stereo = None),
                &sphere,
                "ffmpeg_sphere",
            ),
            (
                "no spherical side data",
                view(None, "ambisonic 1"),
                &sphere,
                "ffmpeg_sphere",
            ),
            ("two video streams", two, &sphere, "ffmpeg_sphere"),
            (
                "spherical side data on a flat export",
                view(Some(whole()), "stereo"),
                &want(false, false),
                "ffmpeg_sphere",
            ),
            (
                "a layout of 4.0",
                view(Some(whole()), "4.0"),
                &sphere,
                "ffmpeg_ambix",
            ),
            ("a complaint in the log", complaint, &sphere, "ffmpeg_log"),
        ] {
            let message = fails(what, &v, want);
            assert!(
                message.starts_with(&format!("check: {check}: ")),
                "{what}: {message}"
            );
        }
    }
}
