//! media's packet reader (a1-plan section 6.3), on clips synth writes:
//! one packet a frame with the clip's timestamps, keyframes where the GOP
//! puts them, frames an edit list hides passed over, a seek that lands on
//! the keyframe before, and the interrupt.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use spherecut_media::packets::{NextPacket, PacketInfo, PacketReader};
use spherecut_synth::media::{self, Gop, Spec};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-packets-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip(spec: Spec) -> PathBuf {
    let d = dir(&spec.name);
    media::write(&spec, &d).unwrap()
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
    let mut pts: Vec<f64> = packets
        .iter()
        .map(|p| p.pts.unwrap() as f64 * tb)
        .collect();
    pts.sort_by(f64::total_cmp);
    pts
}

#[test]
fn one_packet_a_frame_with_keyframes_where_the_gop_puts_them() {
    let path = clip(Spec {
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
    let path = clip(Spec {
        name: "edit".into(),
        frames: 30,
        edit_start: 3,
        ..Spec::default()
    });
    let mut r = open(&path);
    let packets = all(&mut r);
    assert_eq!(packets.len(), 27);
    let pts = shown(&packets, &r);
    let start = r.start_pts().map_or(0.0, |s| s as f64 * r.time_base().to_f64());
    assert!((pts[0] - start).abs() < 1e-6, "{} vs {start}", pts[0]);
}

#[test]
fn a_seek_lands_on_the_keyframe_before() {
    let path = clip(Spec {
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
    let path = clip(Spec {
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
