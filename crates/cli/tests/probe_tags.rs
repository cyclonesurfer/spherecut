//! `spherecut probe --json` on metadata that is not UTF-8. FFmpeg passes
//! a title's bytes through as they are, so they come out as U+FFFD, and
//! the JSON stays valid.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::process::Command;

mod document;

/// A Matroska element with an 8-byte size.
fn el(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let size = (payload.len() as u64 | 1 << 56).to_be_bytes();
    [id, &size, payload].concat()
}

/// A small Matroska file with one 8 kHz PCM track, whose file title and
/// track name are "café" in Latin-1.
fn matroska() -> Vec<u8> {
    let latin1 = b"caf\xe9";
    let header = el(
        b"\x1a\x45\xdf\xa3",
        &[el(b"\x42\x82", b"matroska"), el(b"\x42\x87", &[4])].concat(),
    );
    let info = el(
        b"\x15\x49\xa9\x66",
        &[
            el(b"\x2a\xd7\xb1", &[0x0f, 0x42, 0x40]),
            el(b"\x4d\x80", b"t"),
            el(b"\x7b\xa9", latin1),
        ]
        .concat(),
    );
    let audio = [
        el(b"\xb5", &8000f64.to_be_bytes()),
        el(b"\x9f", &[1]),
        el(b"\x62\x64", &[16]),
    ]
    .concat();
    let track = [
        el(b"\xd7", &[1]),
        el(b"\x73\xc5", &[1]),
        el(b"\x83", &[2]),
        el(b"\x86", b"A_PCM/INT/LIT"),
        el(b"\x53\x6e", latin1),
        el(b"\xe1", &audio),
    ]
    .concat();
    let tracks = el(b"\x16\x54\xae\x6b", &el(b"\xae", &track));
    let block = [&b"\x81\x00\x00\x80"[..], &[0; 1600]].concat();
    let cluster = el(
        b"\x1f\x43\xb6\x75",
        &[el(b"\xe7", &[0]), el(b"\xa3", &block)].concat(),
    );
    let segment = [info, tracks, cluster].concat();
    [header, el(b"\x18\x53\x80\x67", &segment)].concat()
}

#[test]
fn tags_that_are_not_utf8_come_out_as_valid_json() {
    let dir = std::env::temp_dir().join(format!("spherecut-cli-{}-tags", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("latin1.mkv");
    std::fs::write(&path, matroska()).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(&path)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let v = document::document(&out.stdout);
    let report = &v["result"]["files"][0]["report"];
    assert_eq!(report["container"]["tags"]["title"], "caf\u{fffd}");
    assert_eq!(report["streams"][0]["tags"]["title"], "caf\u{fffd}");

    std::fs::remove_dir_all(&dir).ok();
}
