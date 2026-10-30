//! The panes and the notices, through the UI as a user meets them (M1
//! plan sections 7.2 and 7.6): the doctor's banners from a fixture
//! report, lavapipe's own warning, the Doctor tab before and after its
//! report, the Files pane listing a folder and opening a clip from it,
//! and the player's notices for a file without audio, a null output and a
//! hardware fallback.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use spherecut_app::notices::NoticeKind;
use spherecut_app::{DoctorRun, SpherecutApp};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodeMode, Fallback, Fault, FaultPlan, FfmpegBackend, HwKind,
    MediaBackend, VideoDecode, VideoRequest,
};
use spherecut_media::doctor::Severity;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_playback::{AudioDevice, DeviceAudio};
use spherecut_synth::media::{self, Audio, Codec, Spec};

const WAIT: Duration = Duration::from_secs(20);

const SIZE: [f32; 2] = [1200.0, 760.0];

fn whats(app: &SpherecutApp) -> Vec<(Severity, String)> {
    app.state
        .notices
        .shown()
        .iter()
        .map(|n| (n.severity, n.what.clone()))
        .collect()
}

/// Bring a side pane's tab forward, by clicking it.
fn tab(h: &mut Harness<'_, SpherecutApp>, title: &str) {
    h.get_by_role_and_label(Role::Button, title).click();
    h.step();
    h.step();
}

#[test]
fn the_doctors_errors_and_warnings_are_banners_and_its_notes_are_not() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    parts.doctor = DoctorRun::Report(Box::new(common::doctor_report(vec![
        common::issue(Severity::Info, "Vulkan video is off"),
        common::issue(Severity::Warning, "No HEVC decoder"),
        common::issue(Severity::Error, "VA-API can't open the render node"),
    ])));
    let mut h = common::harness(SpherecutApp::new(parts), &rs, SIZE, 1.0);
    h.step();
    h.step();

    // Errors first; lavapipe's warning is there too.
    let shown = whats(h.state());
    assert_eq!(shown.len(), 3, "{shown:?}");
    assert_eq!(
        shown[0],
        (Severity::Error, "VA-API can't open the render node".into())
    );
    assert!(
        shown.contains(&(Severity::Warning, "No HEVC decoder".into())),
        "{shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|(s, w)| *s == Severity::Warning && w.starts_with("Rendering on a CPU adapter")),
        "{shown:?}"
    );
    let _ = h.get_by_label("VA-API can't open the render node");
    let _ = h.get_by_label("No HEVC decoder");
    assert!(h.query_by_label_contains("Vulkan video is off").is_none());
    // The fixes are shown as they'd be typed.
    assert!(h.query_all_by_label("sudo dnf install a-fix").count() >= 2);
    let _ = h.get_by_label_contains("sudo dnf install mesa-vulkan-drivers");

    // The error's Dismiss, the first: it goes, and stays gone.
    h.get_all_by_label("Dismiss")
        .next()
        .expect("a Dismiss")
        .click();
    for _ in 0..3 {
        h.step();
    }
    let shown = whats(h.state());
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(
        shown.iter().all(|(s, _)| *s == Severity::Warning),
        "{shown:?}"
    );
    assert!(
        h.query_by_label("VA-API can't open the render node")
            .is_none()
    );
    let kind = NoticeKind::Doctor("VA-API can't open the render node".into());
    assert!(h.state().state.notices.get(&kind).expect("kept").dismissed);

    // The Doctor tab has the whole report, the note included, and what
    // Spherecut found itself.
    tab(&mut h, "Doctor");
    let _ = h.get_by_label_contains("Vulkan video is off");
    let _ = h.get_by_label("Spherecut found");
    // Lavapipe's warning, in its banner and in the tab.
    assert_eq!(
        h.query_all_by_label_contains("Rendering on a CPU adapter")
            .count(),
        2
    );
    let _ = h.get_by_role_and_label(Role::Button, "Copy the report");
}

#[test]
fn the_doctor_tab_says_it_is_checking_until_the_report_comes() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    let mut h = common::harness(SpherecutApp::new(parts), &rs, SIZE, 1.0);
    h.step();
    tab(&mut h, "Doctor");
    let _ = h.get_by_label("Checking codecs and hardware…");
    // Nothing is open yet, and the viewer says how to open something.
    assert_eq!(
        h.state().state.placeholder(),
        Some("Open a video from the Files pane")
    );
    let _ = h.get_by_label("Open a video from the Files pane");
}

#[test]
fn the_files_pane_lists_a_folder_and_opens_a_clip_from_it() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("files-pane");
    std::fs::create_dir_all(dir.join("Clips")).unwrap();
    std::fs::copy(common::grid_clip(), dir.join("grid.mp4")).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a video").unwrap();
    std::fs::write(dir.join(".hidden.mp4"), "hidden").unwrap();

    let mut parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    parts.list_dirs = true;
    parts.settings.last_dir = Some(dir.clone());
    let mut h = common::harness(SpherecutApp::new(parts), &rs, SIZE, 1.0);
    h.step();
    tab(&mut h, "Files");
    step_until_label(&mut h, "grid.mp4");
    let _ = h.get_by_label("Clips/");
    assert!(h.query_by_label("notes.txt").is_none());
    assert!(h.query_by_label(".hidden.mp4").is_none());
    // Folders first.
    let order: Vec<f32> = ["Clips/", "grid.mp4"]
        .iter()
        .map(|l| h.get_by_label(l).rect().top())
        .collect();
    assert!(order[0] < order[1], "{order:?}");

    h.get_by_label("grid.mp4").click();
    common::until_shown(&mut h, &rs);
    let s = &h.state().state;
    assert_eq!(s.settings.last_dir.as_deref(), Some(dir.as_path()));
    assert!(
        s.streams.first().is_some_and(Option::is_some),
        "the stream's info"
    );

    // Into the folder and back up.
    h.get_by_label("Clips/").click();
    step_until_label(&mut h, "No videos or folders here");
    h.get_by_role_and_label(Role::Button, "Up").click();
    step_until_label(&mut h, "grid.mp4");
}

fn step_until_label(h: &mut Harness<'_, SpherecutApp>, label: &str) {
    let start = std::time::Instant::now();
    loop {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
        assert!(start.elapsed() < WAIT, "no {label:?} within {WAIT:?}");
        common::pace(Duration::from_millis(5));
    }
}

/// A clip with a stereo track, beside the grid.
fn stereo_clip() -> PathBuf {
    media::write(
        &Spec {
            name: "stereo-gui".into(),
            width: 256,
            height: 128,
            frames: 12,
            codec: Codec::H264Lossless,
            audio: Audio::Stereo,
            ..Spec::default()
        },
        common::tmp(),
    )
    .unwrap()
}

#[test]
fn a_silent_file_and_a_null_output_each_say_so() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let backend: Arc<dyn MediaBackend> = Arc::new(FfmpegBackend::new());
    let mut parts = common::parts(&gpu, backend.clone());
    parts.audio = Some(Arc::new(DeviceAudio::new(
        backend,
        AudioDevice::Null {
            rate: 48_000,
            channels: 2,
        },
    )));
    parts.open = vec![common::grid_clip().to_path_buf()];
    let mut h = common::harness(SpherecutApp::new(parts), &rs, SIZE, 1.0);
    common::step_until(&mut h, WAIT, "the no-audio notice", |a| {
        a.state.notices.get(&NoticeKind::NoAudio).is_some()
    });
    h.step();
    let _ = h.get_by_label("This file has no audio");
    assert_eq!(
        h.state()
            .state
            .notices
            .get(&NoticeKind::NoAudio)
            .unwrap()
            .severity,
        Severity::Info
    );

    // The next file forgets the last one's notices.
    let stereo = stereo_clip();
    h.state_mut().state.open(stereo);
    common::step_until(&mut h, WAIT, "the null-output notice", |a| {
        a.state.notices.get(&NoticeKind::NoAudioDevice).is_some()
    });
    h.step();
    let s = &h.state().state;
    assert!(s.notices.get(&NoticeKind::NoAudio).is_none());
    assert!(
        s.app_issues
            .iter()
            .any(|i| i.what == "No audio output device: playing silently")
    );
    let _ = h.get_by_label("No audio output device: playing silently");
    let _ = h.get_by_label_contains("systemctl --user status pipewire");
}

/// FFmpeg's backend, failing frame 2 "as hardware": decoding in software
/// in VA-API's place, as on a machine where no device decodes, until the
/// fault, and then in software for real.
#[derive(Debug)]
struct FaultBackend(FfmpegBackend);

impl MediaBackend for FaultBackend {
    fn name(&self) -> &'static str {
        "fault"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.0.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            decode: DecodeMode::Hardware {
                kind: HwKind::VaApi,
                device: None,
                fallback: Fallback::Software,
            },
            fault: Some(FaultPlan {
                frame: 2,
                fault: Fault::Decode,
                stand_in: true,
            }),
            ..req.clone()
        };
        self.0.open_video(path, &req)
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.0.open_audio(path, req)
    }
}

#[test]
fn a_hardware_fallback_is_a_warning() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut parts = common::parts(&gpu, Arc::new(FaultBackend(FfmpegBackend::new())));
    parts.open = vec![common::grid_clip().to_path_buf()];
    let mut h = common::harness(SpherecutApp::new(parts), &rs, SIZE, 1.0);
    common::until_shown(&mut h, &rs);
    // Frames on, past the failing one.
    for _ in 0..4 {
        h.key_press(egui::Key::ArrowRight);
        common::until_shown(&mut h, &rs);
    }
    common::step_until(&mut h, WAIT, "the fallback notice", |a| {
        a.state.notices.get(&NoticeKind::Fallback(0)).is_some()
    });
    h.step();
    let n = h
        .state()
        .state
        .notices
        .get(&NoticeKind::Fallback(0))
        .unwrap();
    assert_eq!(n.severity, Severity::Warning);
    assert_eq!(n.what, "VA-API decoding went on in software");
    let _ = h.get_by_label("VA-API decoding went on in software");
}
