//! A folder's video files as clips (a1-plan section 6.1, amendment 31):
//! a camera's lens files, proxies and chapters together, by the naming
//! rules in `docs/formats/`, sorted by recording time without probing
//! anything.
//!
//! - Insta360 (insta360.md 2.3): `VID_YYYYMMDD_HHMMSS_LK_NNN.ext`, also
//!   `LRV_` and FreeFrame's `PRO_VID_` and `PRO_LRV_`. `L` is the lens
//!   (0 the back, 1 the screen side) and `K` 0 for a master, 1 for a
//!   proxy, so `_00_` and `_10_` are the lenses of a two-file camera and
//!   `_01_` or the ONE X2's `_11_` its proxy. A `-trim` copy is a clip of
//!   its own.
//! - GoPro (gopro.md 2.5): `G?ccnnnn.ext`, chapter `cc` of recording
//!   `nnnn`, with `GL` (or `.LRV`) the proxy; and the older `GOPRnnnn`
//!   and `GPccnnnn`.
//! - DJI (dji.md): an `.OSV` and the `.LRF` of the same stem.
//! - Any other video file is a clip by itself.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use serde_json::{Map, Value};

use crate::roots::{EntryKind, Listing};
use crate::text;

/// The extensions of the files Spherecut opens, and the cameras' own.
const VIDEO: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "insv", "lrv", "360", "osv", "lrf",
];

/// What a file is to its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// The one file of a one-file recording.
    Main,
    Lens0,
    Lens1,
    /// A recording's chapter, from 0.
    Chapter(u32),
    Proxy,
    /// A chapter's proxy.
    ChapterProxy(u32),
}

/// One recording's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// By role, in role order.
    pub files: Vec<(Role, OsString)>,
    /// When it was recorded: the camera's clock from the name, as
    /// `YYYY-MM-DDTHH:MM:SS` (local time), else the primary file's
    /// modification time, ending in `Z` (UTC).
    pub recorded: Option<String>,
}

impl Clip {
    /// The file a summary is of: a master before a proxy.
    pub fn primary(&self) -> &OsStr {
        self.files
            .iter()
            .find(|(r, _)| !matches!(r, Role::Proxy | Role::ChapterProxy(_)))
            .or(self.files.first())
            .map_or(OsStr::new(""), |(_, n)| n)
    }

    /// The files, as the agent gets them.
    pub fn files_json(&self) -> Value {
        let name = |n: &OsStr| Value::String(text::field(&n.to_string_lossy()));
        let mut m = Map::new();
        let mut chapters = Vec::new();
        let mut proxies = Vec::new();
        for (role, n) in &self.files {
            match role {
                Role::Main => {
                    m.insert("main".into(), name(n));
                }
                Role::Lens0 => {
                    m.insert("lens0".into(), name(n));
                }
                Role::Lens1 => {
                    m.insert("lens1".into(), name(n));
                }
                Role::Proxy => {
                    m.insert("proxy".into(), name(n));
                }
                Role::Chapter(_) => chapters.push(name(n)),
                Role::ChapterProxy(_) => proxies.push(name(n)),
            }
        }
        if !chapters.is_empty() {
            m.insert("chapters".into(), Value::Array(chapters));
        }
        if !proxies.is_empty() {
            m.insert("proxies".into(), Value::Array(proxies));
        }
        Value::Object(m)
    }
}

/// A folder's listing, sorted out.
#[derive(Debug, Default)]
pub struct Grouped {
    /// Sorted by recording time, then by the primary's name.
    pub clips: Vec<Clip>,
    /// Subfolders, by name.
    pub folders: Vec<OsString>,
    /// Entries that are neither video nor a folder.
    pub skipped: usize,
}

/// What a name says about its clip.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Insta360 {
        pro: bool,
        date: String,
        time: String,
        /// The sequence number with any suffix, `-trim` included.
        seq: String,
    },
    GoPro {
        recording: String,
    },
    Dji {
        stem: Vec<u8>,
    },
    /// A file by itself.
    Alone(OsString),
}

pub fn group(listing: &Listing) -> Grouped {
    let mut out = Grouped::default();
    // Each clip's files, with when each was last modified.
    let mut clips: BTreeMap<Key, Vec<(Role, OsString, Option<i64>)>> = BTreeMap::new();
    for e in &listing.entries {
        match e.kind {
            EntryKind::Folder => {
                out.folders.push(e.name.clone());
                continue;
            }
            EntryKind::Other => {
                out.skipped += 1;
                continue;
            }
            EntryKind::File | EntryKind::Link => {}
        }
        let Some((key, role)) = classify(&e.name) else {
            out.skipped += 1;
            continue;
        };
        clips
            .entry(key)
            .or_default()
            .push((role, e.name.clone(), e.modified));
    }
    for (key, mut found) in clips {
        found.sort();
        // One master and no second lens: the file holds both lenses, or
        // is the only one.
        if let [(Role::Lens0, ..)] | [(Role::Lens0, ..), (Role::Proxy, ..)] = found.as_slice() {
            found[0].0 = Role::Main;
        }
        let files: Vec<(Role, OsString)> = found.iter().map(|(r, n, _)| (*r, n.clone())).collect();
        // The primary's: the clip's first master, else its proxy.
        let modified = found
            .iter()
            .find(|(r, ..)| !matches!(r, Role::Proxy | Role::ChapterProxy(_)))
            .or(found.first())
            .and_then(|(.., m)| *m);
        let recorded = match &key {
            Key::Insta360 { date, time, .. } => Some(format!(
                "{}-{}-{}T{}:{}:{}",
                &date[..4],
                &date[4..6],
                &date[6..],
                &time[..2],
                &time[2..4],
                &time[4..]
            )),
            _ => modified.map(utc),
        };
        out.clips.push(Clip { files, recorded });
    }
    out.clips.sort_by(|a, b| match (&a.recorded, &b.recorded) {
        (Some(x), Some(y)) => x.cmp(y).then_with(|| a.primary().cmp(b.primary())),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.primary().cmp(b.primary()),
    });
    out
}

/// A video file's clip and role, from its name; None for a file that
/// isn't video.
fn classify(name: &OsStr) -> Option<(Key, Role)> {
    let bytes = name.as_bytes();
    let dot = bytes.iter().rposition(|&b| b == b'.')?;
    let (stem, ext) = (&bytes[..dot], &bytes[dot + 1..]);
    let ext = std::str::from_utf8(ext).ok()?.to_ascii_lowercase();
    if !VIDEO.contains(&ext.as_str()) {
        return None;
    }
    let stem_str = std::str::from_utf8(stem).ok();
    if let Some(found) = stem_str.and_then(|s| insta360(s, &ext)) {
        return Some(found);
    }
    if let Some(found) = stem_str.and_then(|s| gopro(s, &ext)) {
        return Some(found);
    }
    if ext == "osv" || ext == "lrf" {
        let role = if ext == "osv" {
            Role::Main
        } else {
            Role::Proxy
        };
        return Some((
            Key::Dji {
                stem: stem.to_ascii_uppercase(),
            },
            role,
        ));
    }
    Some((Key::Alone(name.to_owned()), Role::Main))
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

/// `[PRO_](VID|LRV)_YYYYMMDD_HHMMSS_LK_NNN[-trim]`.
fn insta360(stem: &str, ext: &str) -> Option<(Key, Role)> {
    if !matches!(ext, "insv" | "lrv" | "mp4") {
        return None;
    }
    let (pro, rest) = match stem.strip_prefix("PRO_") {
        Some(rest) => (true, rest),
        None => (false, stem),
    };
    let mut parts = rest.splitn(5, '_');
    let kind = parts.next()?;
    let (date, time, lk, seq) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let proxy_name = match kind {
        "VID" => false,
        "LRV" => true,
        _ => return None,
    };
    let number = seq.split(['-', '(', ' ']).next().unwrap_or("");
    if !digits(date, 8) || !digits(time, 6) || !digits(lk, 2) || number.is_empty() {
        return None;
    }
    if !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let lk = lk.as_bytes();
    let role = if proxy_name || lk[1] == b'1' || ext == "lrv" {
        Role::Proxy
    } else if lk[0] == b'1' {
        Role::Lens1
    } else {
        Role::Lens0
    };
    Some((
        Key::Insta360 {
            pro,
            date: date.into(),
            time: time.into(),
            seq: seq.into(),
        },
        role,
    ))
}

/// `G?ccnnnn` with `?` the encoding (`GL` the proxy), or the older
/// `GOPRnnnn` (chapter 0) and `GPccnnnn`.
fn gopro(stem: &str, ext: &str) -> Option<(Key, Role)> {
    if !matches!(ext, "360" | "mp4" | "lrv") || stem.len() != 8 || !stem.is_ascii() {
        return None;
    }
    let (head, rest) = stem.split_at(2);
    let (chapter, recording) = match head {
        "GO" if rest.starts_with("PR") && digits(&rest[2..], 4) => (0, &rest[2..]),
        "GS" | "GX" | "GH" | "GL" | "GP" if digits(rest, 6) => {
            (rest[..2].parse().ok()?, &rest[2..])
        }
        _ => return None,
    };
    let proxy = head == "GL" || ext == "lrv";
    Some((
        Key::GoPro {
            recording: recording.into(),
        },
        if proxy {
            Role::ChapterProxy(chapter)
        } else {
            Role::Chapter(chapter)
        },
    ))
}

/// `secs` since 1970 as `YYYY-MM-DDTHH:MM:SSZ` (Howard Hinnant's
/// `civil_from_days`).
fn utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The folder's state as far as paging is concerned: a hash of its
/// entries' names, kinds and times, which a cursor carries so that a
/// folder changed between pages is noticed (FNV-1a, 64 bits).
pub fn fingerprint(listing: &Listing) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for e in &listing.entries {
        eat(e.name.as_bytes());
        eat(&[0, e.kind as u8]);
        eat(&e.modified.unwrap_or(i64::MIN).to_le_bytes());
    }
    eat(&listing.left_out.to_le_bytes());
    h
}

/// Every clip's summary keys, for a test of the response's shape.
#[cfg(test)]
pub(crate) fn names(g: &Grouped) -> Vec<Value> {
    g.clips.iter().map(Clip::files_json).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::Entry;
    use serde_json::json;

    fn listing(names: &[(&str, EntryKind, i64)]) -> Listing {
        let mut entries: Vec<Entry> = names
            .iter()
            .map(|(n, kind, t)| Entry {
                name: OsString::from(n),
                kind: *kind,
                modified: Some(*t),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Listing {
            entries,
            left_out: 0,
        }
    }

    fn files(names: &[&str]) -> Listing {
        listing(
            &names
                .iter()
                .map(|n| (*n, EntryKind::File, 1_700_000_000))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn insta360_lenses_proxies_and_trims() {
        let g = group(&files(&[
            "VID_20260814_132640_00_013.insv",
            "VID_20260814_132640_10_013.insv",
            "LRV_20260814_132640_01_013.lrv",
            "VID_20260814_140000_00_014.insv",
            "LRV_20260814_140000_01_014.lrv",
            "VID_20260814_140000_00_014-trim.insv",
            "PRO_VID_20260814_150000_00_015.insv",
            "LRV_20260815_090000_11_016.insv",
            "VID_20260815_090000_00_016.insv",
            "VID_20260815_090000_10_016.insv",
        ]));
        assert_eq!(
            names(&g),
            [
                json!({"lens0": "VID_20260814_132640_00_013.insv", "lens1": "VID_20260814_132640_10_013.insv", "proxy": "LRV_20260814_132640_01_013.lrv"}),
                json!({"main": "VID_20260814_140000_00_014-trim.insv"}),
                json!({"main": "VID_20260814_140000_00_014.insv", "proxy": "LRV_20260814_140000_01_014.lrv"}),
                json!({"main": "PRO_VID_20260814_150000_00_015.insv"}),
                json!({"lens0": "VID_20260815_090000_00_016.insv", "lens1": "VID_20260815_090000_10_016.insv", "proxy": "LRV_20260815_090000_11_016.insv"}),
            ]
        );
        assert_eq!(g.clips[0].recorded.as_deref(), Some("2026-08-14T13:26:40"));
        assert_eq!(g.clips[0].primary(), "VID_20260814_132640_00_013.insv");
        assert_eq!(g.skipped, 0);
    }

    #[test]
    fn gopro_chapters_and_proxies() {
        let g = group(&files(&[
            "GS010013.360",
            "GS020013.360",
            "GL010013.LRV",
            "GX010020.MP4",
            "GOPR0030.MP4",
            "GP010030.MP4",
            "GS010013.THM",
        ]));
        let n = names(&g);
        assert!(
            n.contains(
                &json!({"chapters": ["GS010013.360", "GS020013.360"], "proxies": ["GL010013.LRV"]})
            ),
            "{n:?}"
        );
        assert!(n.contains(&json!({"chapters": ["GX010020.MP4"]})));
        assert!(n.contains(&json!({"chapters": ["GOPR0030.MP4", "GP010030.MP4"]})));
        assert_eq!(g.skipped, 1, "the thumbnail");
        let c = g.clips.iter().find(|c| c.files.len() == 3).unwrap();
        assert_eq!(c.primary(), "GS010013.360");
        assert_eq!(c.recorded.as_deref(), Some("2023-11-14T22:13:20Z"));
    }

    #[test]
    fn dji_and_everything_else() {
        let g = group(&listing(&[
            ("CAM_0001.OSV", EntryKind::File, 30),
            ("CAM_0001.LRF", EntryKind::File, 30),
            ("holiday.mp4", EntryKind::Link, 20),
            ("notes.txt", EntryKind::File, 10),
            ("DCIM", EntryKind::Folder, 10),
            ("fifo", EntryKind::Other, 10),
            ("no-extension", EntryKind::File, 10),
        ]));
        assert_eq!(
            names(&g),
            [
                json!({"main": "holiday.mp4"}),
                json!({"main": "CAM_0001.OSV", "proxy": "CAM_0001.LRF"}),
            ]
        );
        assert_eq!(g.folders, [OsString::from("DCIM")]);
        assert_eq!(g.skipped, 3);
    }

    #[test]
    fn camera_clocks_sort_before_file_times_of_the_same_moment_only_by_text() {
        let g = group(&listing(&[
            ("b.mp4", EntryKind::File, 0),
            ("a.mp4", EntryKind::File, 0),
            ("VID_19691231_235959_00_001.insv", EntryKind::File, 5),
        ]));
        let order: Vec<&OsStr> = g.clips.iter().map(Clip::primary).collect();
        assert_eq!(order, ["VID_19691231_235959_00_001.insv", "a.mp4", "b.mp4"]);
        assert_eq!(g.clips[1].recorded.as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn names_that_only_look_like_a_camera_s_are_clips_alone() {
        for n in [
            "VID_2026081_132640_00_013.insv",
            "VID_20260814_132640_0_013.insv",
            "VID_20260814_132640_00_.insv",
            "GS01001.360",
            "GSab0013.360",
            "VID_20260814_132640_00_013.txt",
        ] {
            let g = group(&files(&[n]));
            let alone = g.clips.len() == 1 && g.clips[0].files == [(Role::Main, OsString::from(n))];
            assert!(alone || g.skipped == 1, "{n}: {g:?}");
        }
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn the_fingerprint_sees_any_change() {
        let a = files(&["a.mp4", "b.mp4"]);
        let mut b = files(&["a.mp4", "b.mp4"]);
        assert_eq!(fingerprint(&a), fingerprint(&b));
        b.entries[1].modified = Some(1);
        assert_ne!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&files(&["a.mp4"])));
    }
}
