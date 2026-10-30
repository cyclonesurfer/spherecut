//! media's packet reader (a1-plan section 6.3), on clips synth writes:
//! one packet a frame with the clip's timestamps, keyframes where the GOP
//! puts them, frames an edit list hides passed over, a seek that lands on
//! the keyframe before, and the interrupt; and `jitter-dups`, analyze's
//! clip, holding what it plants.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use spherecut_media::packets::{NextPacket, PacketInfo, PacketReader};
use spherecut_synth::media::{self, Gop, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn dir(name: &str) -> TempDir {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("packets-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

/// `spec` written into its own directory, which goes with the guard.
fn clip(spec: Spec) -> (TempDir, PathBuf) {
    let d = dir(&spec.name);
    let path = media::write(&spec, &d.0).unwrap();
    (d, path)
}

/// Every packet from the reader's position to the end.
fn all(r: &mut PacketReader) -> Vec<PacketInfo> {
    let mut out = Vec::new();
    loop {
        match r.next_packet() {
            NextPacket::Packet(p) => out.push(p),
            NextPacket::End { damaged } => {
                assert!(!damaged);
                return out;
            }
            NextPacket::Stopped => panic!("stopped without being asked"),
        }
    }
}

fn open(path: &Path) -> PacketReader {
    PacketReader::open(path, 0, Arc::new(AtomicBool::new(false))).unwrap()
}

/// pts in presentation order, as seconds.
fn shown(packets: &[PacketInfo], r: &PacketReader) -> Vec<f64> {
    let tb = r.time_base().to_f64();
    let mut pts: Vec<f64> = packets.iter().map(|p| p.pts.unwrap() as f64 * tb).collect();
    pts.sort_by(f64::total_cmp);
    pts
}

#[test]
fn one_packet_a_frame_with_keyframes_where_the_gop_puts_them() {
    let (_dir, path) = clip(Spec {
        name: "gop".into(),
        frames: 40,
        gop: Some(Gop {
            keyint: 10,
            bframes: 2,
            open: false,
            radl: 0,
        }),
        ..Spec::default()
    });
    let mut r = open(&path);
    let packets = all(&mut r);
    assert_eq!(packets.len(), 40);
    assert_eq!(packets.iter().filter(|p| p.key).count(), 4);
    assert!(packets.iter().all(|p| p.size > 0 && !p.corrupt));
    let pts = shown(&packets, &r);
    for (i, t) in pts.iter().enumerate() {
        let want = (i as f64) / 30.0 + pts[0];
        assert!((t - want).abs() < 1e-6, "frame {i} at {t}, not {want}");
    }
    // B-frames come after the frames they refer to: file order isn't
    // presentation order.
    assert!(packets.windows(2).any(|w| w[1].pts < w[0].pts));
}

#[test]
fn frames_an_edit_list_hides_are_passed_over() {
    let (_dir, path) = clip(Spec {
        name: "edit".into(),
        frames: 30,
        edit_start: 3,
        ..Spec::default()
    });
    let mut r = open(&path);
    let packets = all(&mut r);
    assert_eq!(packets.len(), 27);
    let pts = shown(&packets, &r);
    let start = r
        .start_pts()
        .map_or(0.0, |s| s as f64 * r.time_base().to_f64());
    assert!((pts[0] - start).abs() < 1e-6, "{} vs {start}", pts[0]);
}

#[test]
fn a_seek_lands_on_the_keyframe_before() {
    let (_dir, path) = clip(Spec {
        name: "seek".into(),
        frames: 40,
        gop: Some(Gop {
            keyint: 10,
            bframes: 0,
            open: false,
            radl: 0,
        }),
        ..Spec::default()
    });
    let mut r = open(&path);
    let tb = r.time_base();
    // Frame 25's pts: 25/30 s.
    let pts = (25.0 / 30.0 / tb.to_f64()).round() as i64 + r.start_pts().unwrap_or(0);
    r.seek(pts).unwrap();
    let packets = all(&mut r);
    assert!(packets[0].key);
    assert_eq!(packets.len(), 20, "from frame 20 on");
}

#[test]
fn the_interrupt_stops_a_read() {
    // Noisy frames, so that the file is far larger than what the demuxer
    // buffers and reads go on reaching the file.
    let (_dir, path) = clip(Spec {
        name: "stop".into(),
        frames: 60,
        width: 512,
        height: 256,
        noise: Some(1),
        ..Spec::default()
    });
    let flag = Arc::new(AtomicBool::new(false));
    let mut r = PacketReader::open(&path, 0, flag.clone()).unwrap();
    assert!(matches!(r.next_packet(), NextPacket::Packet(_)));
    flag.store(true, Ordering::Release);
    // What the demuxer already holds may still come out; the read after it
    // stops.
    let stopped = (0..20).any(|_| matches!(r.next_packet(), NextPacket::Stopped));
    assert!(stopped);
}

#[test]
fn jitter_dups_holds_what_it_plants() {
    let d = dir("jitter");
    let path = media::write_jitter_dups(&d.0).unwrap();
    let planned = media::jitter_dups_frames();
    let mut r = open(&path);
    let packets = all(&mut r);
    assert_eq!(packets.len(), planned.len());
    // No B-frames: file order is presentation order, and the timestamps
    // are the plan's, from the first.
    let first = packets[0].pts.unwrap();
    let tb = r.time_base();
    for (p, f) in packets.iter().zip(&planned) {
        let ticks = (p.pts.unwrap() - first) as f64 * tb.to_f64() * media::VFR_TIMESCALE as f64;
        assert!((ticks - f.pts as f64).abs() < 0.5, "{p:?} vs {f:?}");
    }
    let keys: Vec<usize> = (0..packets.len()).filter(|&i| packets[i].key).collect();
    let want: Vec<usize> = (0..packets.len())
        .step_by(media::JITTER_KEYINT as usize)
        .collect();
    assert_eq!(keys, want);
    let mut sizes: Vec<usize> = packets
        .iter()
        .zip(&planned)
        .filter(|(p, f)| !p.key && !f.repeat)
        .map(|(p, _)| p.size)
        .collect();
    sizes.sort_unstable();
    let median = sizes[sizes.len() / 2];
    // analyze's line is 5 % of the median (limits::REPEAT_UNDER).
    for (p, f) in packets.iter().zip(&planned) {
        if f.repeat {
            assert!(p.size * 20 < median, "{f:?}: {} of {median}", p.size);
        } else if !p.key {
            assert!(p.size * 5 > median, "{f:?}: {} of {median}", p.size);
        }
    }
}
