//! FFmpeg's log, captured on one thread while another thread with no
//! subscriber logs FFmpeg's first error of the process: the capture still
//! gets its own lines. In a test binary of its own, so that no subscriber
//! is set anywhere and the callsite FFmpeg's errors reach is new when the
//! other thread logs.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::mpsc;

use spherecut_media::capture_ffmpeg_log;
use spherecut_media::decode::VideoDecoder;

/// The test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn a_capture_keeps_its_lines_when_a_thread_without_a_subscriber_logs_first() {
    let dir = TempDir(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("ffmpeg-log-capture-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).unwrap();
    // An MP4 with no `moov`, which FFmpeg's demuxer says at error level.
    let bad = dir.0.join("no-moov.mp4");
    let mut bytes = b"\0\0\0\x14ftypisom\0\0\x02\0isom".to_vec();
    bytes.extend_from_slice(b"\0\0\0\x10mdat\0\0\0\0\0\0\0\0");
    std::fs::write(&bad, bytes).unwrap();
    let (go, wait) = mpsc::channel();
    let (done, logged) = mpsc::channel();
    let other = std::thread::spawn({
        let bad = bad.clone();
        move || {
            wait.recv().unwrap();
            VideoDecoder::open(&bad, None, None).unwrap_err();
            done.send(()).unwrap();
        }
    });
    let ((), lines) = capture_ffmpeg_log(|| {
        go.send(()).unwrap();
        logged.recv().unwrap();
        VideoDecoder::open(&bad, None, None).unwrap_err();
    });
    other.join().unwrap();
    assert!(
        lines.iter().any(|l| l.contains("moov atom not found")),
        "{lines:?}"
    );
}
