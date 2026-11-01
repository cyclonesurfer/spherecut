//! Pat's takes by role (M3 plan 4.4): `samples::role("tripod_seam")` is
//! clip 8's master, wherever the samples folder is.
//!
//! The roles are `samples-roles.toml`, checked in beside this crate's
//! `Cargo.toml` and built into it. A role gives the take's number in
//! docs/SAMPLES.md, the camera's number for its files and its mode; a
//! take still to come has a role with no file, and its tests skip until
//! it has one. [`role`] and [`lrv`] find the files through
//! [`tier2::require_samples_cpu`], so a test that uses them is a Tier 2c
//! test, which runs in the cloud as well as under hw-test.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::tier2;

/// The roles file, as checked in.
const ROLES_TOML: &str = include_str!("../samples-roles.toml");

/// A recording mode, as the camera names it. The sizes and rates are the
/// tracks' in Pat's files (docs/formats/insta360.md, "Pat's X5 samples").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// 8K30: two 3840² tracks at 29.97 fps.
    K8_30,
    /// 8K25: two 3840² tracks at 25 fps.
    K8_25,
    /// 5.7K60: two 2880² tracks at 59.94 fps.
    K5_7_60,
    /// 4K120: two 1920² tracks at 119.88 fps.
    K4_120,
    /// 5.7K30: two 2880² tracks at 29.97 fps, as docs/SAMPLES.md asks for
    /// take 16. No sample has it yet.
    K5_7_30,
}

impl Mode {
    pub const ALL: [Mode; 5] = [
        Mode::K8_30,
        Mode::K8_25,
        Mode::K5_7_60,
        Mode::K4_120,
        Mode::K5_7_30,
    ];

    /// The name the camera and samples-roles.toml give it, such as
    /// `5.7K60`.
    pub fn name(self) -> &'static str {
        match self {
            Mode::K8_30 => "8K30",
            Mode::K8_25 => "8K25",
            Mode::K5_7_60 => "5.7K60",
            Mode::K4_120 => "4K120",
            Mode::K5_7_30 => "5.7K30",
        }
    }

    /// The side of each of the master's two square tracks, in pixels.
    pub fn side(self) -> u32 {
        match self {
            Mode::K8_30 | Mode::K8_25 => 3840,
            Mode::K5_7_60 | Mode::K5_7_30 => 2880,
            Mode::K4_120 => 1920,
        }
    }

    /// The tracks' frame rate as a fraction, numerator and denominator:
    /// `(30000, 1001)` at 29.97 fps.
    pub fn rate(self) -> (u32, u32) {
        match self {
            Mode::K8_30 | Mode::K5_7_30 => (30000, 1001),
            Mode::K8_25 => (25, 1),
            Mode::K5_7_60 => (60000, 1001),
            Mode::K4_120 => (120000, 1001),
        }
    }

    fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// One role of samples-roles.toml: the take that plays it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Role {
    /// Its name, such as `tripod_seam`.
    pub name: &'static str,
    /// The number docs/SAMPLES.md gives the clip or take: 8 for clip 8,
    /// 14 for take 14.
    pub take: u32,
    /// The camera's number for the take's files, the `NNN` of
    /// `VID_…_00_NNN.insv` and `LRV_…_01_NNN.lrv`, or `None` for a take
    /// still to come.
    pub file: Option<&'static str>,
    /// The recording mode.
    pub mode: Mode,
    /// The height from the ground to the lenses in metres, for a take on
    /// the tripod whose note gives it (take 14's, for M3 plan 8.4's ground
    /// model); `None` until Pat's NOTES.txt does.
    pub tripod_height_m: Option<f64>,
}

/// samples-roles.toml, parsed.
#[derive(Debug)]
struct Roles {
    /// The camera's folder under `SPHERECUT_SAMPLES`.
    folder: &'static str,
    roles: Vec<Role>,
}

/// The roles, parsed once; a file that doesn't parse fails the test.
fn table() -> &'static Roles {
    static TABLE: OnceLock<Roles> = OnceLock::new();
    TABLE.get_or_init(|| parse(ROLES_TOML).unwrap_or_else(|e| panic!("{e}")))
}

/// Every role, in the file's order.
pub fn roles() -> &'static [Role] {
    &table().roles
}

/// The role called `name`. A name samples-roles.toml lacks fails the test
/// rather than skipping it: a misspelt role is the test's fault, not a
/// missing take.
#[track_caller]
pub fn entry(name: &str) -> &'static Role {
    match roles().iter().find(|r| r.name == name) {
        Some(r) => r,
        None => panic!("no role `{name}` in samples-roles.toml"),
    }
}

/// The mode of the take that plays `name`.
#[track_caller]
pub fn mode(name: &str) -> Mode {
    entry(name).mode
}

/// The master of the take that plays `name`, its `VID_…_00_NNN.insv`,
/// from the samples folder that [`tier2::require_samples_cpu`] gives; or
/// skip, saying which take is missing.
#[track_caller]
pub fn role(name: &str) -> PathBuf {
    find(name, Kind::Master)
}

/// The LRV of the take that plays `name`, its `LRV_…_01_NNN.lrv`, found
/// as [`role`] finds the master.
#[track_caller]
pub fn lrv(name: &str) -> PathBuf {
    find(name, Kind::Lrv)
}

/// The master of `role`'s take in the samples folder `dir`, or why it
/// isn't there, for a test that goes on without it. The file is looked
/// for in `dir`'s camera folder (`insta360-x5`) if there is one, else in
/// `dir` itself.
pub fn master_in(dir: &Path, role: &Role) -> Result<PathBuf, String> {
    file_in(dir, role, Kind::Master)
}

/// The LRV of `role`'s take in the samples folder `dir`, or why it isn't
/// there, found as [`master_in`] finds the master.
pub fn lrv_in(dir: &Path, role: &Role) -> Result<PathBuf, String> {
    file_in(dir, role, Kind::Lrv)
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Master,
    Lrv,
}

impl Kind {
    /// The end of the file's name, from the camera's number.
    fn suffix(self, file: &str) -> String {
        match self {
            Kind::Master => format!("_00_{file}.insv"),
            Kind::Lrv => format!("_01_{file}.lrv"),
        }
    }
}

/// A take still to come is skipped before the samples folder is looked
/// for, so the reason names the take.
#[track_caller]
fn find(name: &str, kind: Kind) -> PathBuf {
    let role = entry(name);
    if role.file.is_none() {
        tier2::skip(no_file(role));
    }
    let dir = tier2::require_samples_cpu();
    file_in(&dir, role, kind).unwrap_or_else(|why| tier2::skip(why))
}

fn no_file(role: &Role) -> String {
    format!(
        "take {} ({}) has no file yet: samples-roles.toml names it once Pat's take is in",
        role.take, role.name
    )
}

fn file_in(dir: &Path, role: &Role, kind: Kind) -> Result<PathBuf, String> {
    let Some(file) = role.file else {
        return Err(no_file(role));
    };
    let camera = dir.join(table().folder);
    let dir = if camera.is_dir() {
        camera
    } else {
        dir.to_path_buf()
    };
    let suffix = kind.suffix(file);
    let take = format!("take {} ({})", role.take, role.name);
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("{take}: cannot list {}: {e}", dir.display()))?;
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.as_bytes().ends_with(suffix.as_bytes()))
                && p.is_file()
        })
        .collect();
    found.sort();
    match &found[..] {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "{take}: no file ending in {suffix} in {}",
            dir.display()
        )),
        several => Err(format!(
            "{take}: {} files end in {suffix} in {}, so which is the take isn't known",
            several.len(),
            dir.display()
        )),
    }
}

/// The roles in `text`, or what is wrong with it, by line. The format is
/// samples-roles.toml's alone: a top-level `folder`, then `[role]` tables
/// of `take`, `file`, `mode` and `tripod_height_m`, strings in double
/// quotes, numbers bare, and comments on lines of their own. A name, a
/// take or a file named twice is refused.
fn parse(text: &'static str) -> Result<Roles, String> {
    let mut folder = None;
    let mut roles: Vec<Role> = Vec::new();
    let mut open: Option<Draft> = None;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let at = |what: String| format!("samples-roles.toml line {n}: {what}");
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some(done) = open.take() {
                roles.push(done.finish()?);
            }
            let named = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
            if name.is_empty() || !name.chars().all(named) {
                return Err(at(format!(
                    "`{name}` is not a role's name (a to z, 0 to 9 and _)"
                )));
            }
            if roles.iter().any(|r| r.name == name) {
                return Err(at(format!("role `{name}` again")));
            }
            open = Some(Draft::new(name, n));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(at(format!(
                "`{line}` is neither a [role] nor a `key = value`"
            )));
        };
        let (key, value) = (key.trim(), value.trim());
        match &mut open {
            Some(draft) => draft.set(key, value).map_err(at)?,
            None if key == "folder" && folder.is_none() => {
                folder = Some(
                    quoted(value)
                        .filter(|f| !f.is_empty())
                        .ok_or_else(|| at(format!("folder {value} is not a name in quotes")))?,
                );
            }
            None if key == "folder" => return Err(at("`folder` again".to_owned())),
            None => {
                return Err(at(format!("`{key}` before any [role] is not `folder`")));
            }
        }
    }
    if let Some(done) = open.take() {
        roles.push(done.finish()?);
    }
    for (i, r) in roles.iter().enumerate() {
        if let Some(o) = roles[..i].iter().find(|o| o.take == r.take) {
            return Err(format!(
                "samples-roles.toml: `{}` and `{}` are both take {}",
                o.name, r.name, r.take
            ));
        }
        if let Some((o, f)) = r.file.and_then(|f| {
            roles[..i]
                .iter()
                .find(|o| o.file == Some(f))
                .map(|o| (o, f))
        }) {
            return Err(format!(
                "samples-roles.toml: `{}` and `{}` are both file {f}",
                o.name, r.name
            ));
        }
    }
    let folder = folder.ok_or("samples-roles.toml: no `folder`")?;
    Ok(Roles { folder, roles })
}

/// A role whose table is being read.
struct Draft {
    name: &'static str,
    line: usize,
    take: Option<u32>,
    file: Option<&'static str>,
    mode: Option<Mode>,
    tripod_height_m: Option<f64>,
}

impl Draft {
    fn new(name: &'static str, line: usize) -> Draft {
        Draft {
            name,
            line,
            take: None,
            file: None,
            mode: None,
            tripod_height_m: None,
        }
    }

    fn set(&mut self, key: &str, value: &'static str) -> Result<(), String> {
        fn once<T>(slot: &mut Option<T>, key: &str, v: T) -> Result<(), String> {
            if slot.is_some() {
                return Err(format!("`{key}` again"));
            }
            *slot = Some(v);
            Ok(())
        }
        match key {
            "take" => {
                let take = value
                    .parse()
                    .map_err(|_| format!("take `{value}` is not a whole number"))?;
                once(&mut self.take, key, take)
            }
            "file" => {
                let file = quoted(value)
                    .filter(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or_else(|| {
                        format!(
                            "file {value} is not the camera's number in quotes, such as \"008\""
                        )
                    })?;
                once(&mut self.file, key, file)
            }
            "mode" => {
                let mode = quoted(value).and_then(Mode::from_name).ok_or_else(|| {
                    let names: Vec<&str> = Mode::ALL.iter().map(|m| m.name()).collect();
                    format!("mode {value} is none of {}", names.join(", "))
                })?;
                once(&mut self.mode, key, mode)
            }
            "tripod_height_m" => {
                let h = value
                    .parse::<f64>()
                    .ok()
                    .filter(|h| h.is_finite() && *h > 0.0)
                    .ok_or_else(|| {
                        format!("tripod_height_m `{value}` is not a height in metres")
                    })?;
                once(&mut self.tripod_height_m, key, h)
            }
            _ => Err(format!(
                "`{key}` is not a role's key (take, file, mode, tripod_height_m)"
            )),
        }
    }

    fn finish(self) -> Result<Role, String> {
        let missing = |key: &str| {
            format!(
                "samples-roles.toml line {}: role `{}` has no `{key}`",
                self.line, self.name
            )
        };
        Ok(Role {
            name: self.name,
            take: self.take.ok_or_else(|| missing("take"))?,
            file: self.file,
            mode: self.mode.ok_or_else(|| missing("mode"))?,
            tripod_height_m: self.tripod_height_m,
        })
    }
}

/// `value` without its double quotes, if it is a string.
fn quoted(value: &'static str) -> Option<&'static str> {
    value
        .strip_prefix('"')?
        .strip_suffix('"')
        .filter(|s| !s.contains('"'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::must_fail;

    /// What `f` panicked with, or what it returned.
    fn outcome<T>(f: impl FnOnce() -> T + std::panic::UnwindSafe) -> Result<T, String> {
        std::panic::catch_unwind(f).map_err(|e| {
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default()
        })
    }

    /// A test's own directory, removed with what it holds when dropped.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn the_roles_file_parses() {
        assert_eq!(table().folder, "insta360-x5");
        assert_eq!(
            *entry("tripod_seam"),
            Role {
                name: "tripod_seam",
                take: 8,
                file: Some("008"),
                mode: Mode::K8_30,
                tripod_height_m: None,
            }
        );
        // The camera numbers its own files: clip 1 is 004.
        let shot = [
            ("clip1_8k30", 1, "004", Mode::K8_30),
            ("clip2_8k25", 2, "005", Mode::K8_25),
            ("clip3_5k7_60", 3, "006", Mode::K5_7_60),
            ("clip4_4k120", 4, "007", Mode::K4_120),
            ("tripod_seam", 8, "008", Mode::K8_30),
            ("handheld_flips", 9, "009", Mode::K8_30),
            ("slow_turns", 10, "010", Mode::K8_30),
        ];
        for (name, take, file, m) in shot {
            let r = entry(name);
            assert_eq!((r.take, r.file, r.mode), (take, Some(file), m), "{name}");
        }
        // Takes 14 to 17 are still to come, and take 14's height waits
        // for Pat's note.
        let to_come = [
            ("open_field", 14, Mode::K8_30),
            ("lens_cover", 15, Mode::K8_30),
            ("mode_5k7_30", 16, Mode::K5_7_30),
            ("shade_to_sun", 17, Mode::K8_30),
        ];
        for (name, take, m) in to_come {
            let r = entry(name);
            assert_eq!((r.take, r.file, r.mode), (take, None, m), "{name}");
        }
        assert_eq!(entry("open_field").tripod_height_m, None);
        assert_eq!(roles().len(), shot.len() + to_come.len());
        assert_eq!(mode("clip3_5k7_60"), Mode::K5_7_60);
        for m in Mode::ALL {
            assert_eq!(Mode::from_name(m.name()), Some(m));
        }
    }

    #[test]
    fn every_role_is_unique() {
        let all = roles();
        for (i, r) in all.iter().enumerate() {
            for o in &all[..i] {
                assert_ne!(o.name, r.name);
                assert_ne!(o.take, r.take, "{} {}", o.name, r.name);
                assert!(
                    r.file.is_none() || o.file != r.file,
                    "{} {}",
                    o.name,
                    r.name
                );
            }
        }
        // Controls: a name, a take or a file named twice is refused.
        let twice = [
            (
                "a role named twice",
                "folder = \"x\"\n[a]\ntake = 1\nmode = \"8K30\"\n[a]\ntake = 2\nmode = \"8K30\"\n",
                "line 5: role `a` again",
            ),
            (
                "two roles of one take",
                "folder = \"x\"\n[a]\ntake = 1\nmode = \"8K30\"\n[b]\ntake = 1\nmode = \"8K30\"\n",
                "`a` and `b` are both take 1",
            ),
            (
                "two roles of one file",
                "folder = \"x\"\n[a]\ntake = 1\nfile = \"004\"\nmode = \"8K30\"\n\
                 [b]\ntake = 2\nfile = \"004\"\nmode = \"8K30\"\n",
                "`a` and `b` are both file 004",
            ),
        ];
        for (control, text, says) in twice {
            let e = must_fail(control, parse(text));
            assert!(e.ends_with(says), "{e}");
        }
    }

    #[test]
    fn the_parser_refuses_what_it_does_not_know() {
        let good = "folder = \"insta360-x5\"\n\n  # A comment.\n[a]\ntake = 1\nfile = \"004\"\n\
                    mode = \"8K30\"\n[b_2]\ntake = 14\nmode = \"5.7K30\"\ntripod_height_m = 1.52\n";
        let t = parse(good).unwrap();
        assert_eq!(t.folder, "insta360-x5");
        assert_eq!(
            t.roles[1],
            Role {
                name: "b_2",
                take: 14,
                file: None,
                mode: Mode::K5_7_30,
                tripod_height_m: Some(1.52),
            }
        );
        let bad = [
            (
                "[a]\ntake = 1\nmode = \"8K30\"\n",
                "samples-roles.toml: no `folder`",
            ),
            ("folder = \"x\"\nfolder = \"y\"\n", "line 2: `folder` again"),
            ("folder = x\n", "line 1: folder x is not a name in quotes"),
            (
                "folder = \"x\"\ntake = 1\n",
                "line 2: `take` before any [role] is not `folder`",
            ),
            (
                "folder = \"x\"\n[Clip-1]\n",
                "line 2: `Clip-1` is not a role's name (a to z, 0 to 9 and _)",
            ),
            (
                "folder = \"x\"\n[a]\ntake\n",
                "line 3: `take` is neither a [role] nor a `key = value`",
            ),
            (
                "folder = \"x\"\n[a]\nlens = 0\n",
                "line 3: `lens` is not a role's key (take, file, mode, tripod_height_m)",
            ),
            (
                "folder = \"x\"\n[a]\ntake = 1 # one\n",
                "line 3: take `1 # one` is not a whole number",
            ),
            (
                "folder = \"x\"\n[a]\ntake = 1\ntake = 2\n",
                "line 4: `take` again",
            ),
            (
                "folder = \"x\"\n[a]\nfile = 008\n",
                "line 3: file 008 is not the camera's number in quotes, such as \"008\"",
            ),
            (
                "folder = \"x\"\n[a]\nfile = \"08a\"\n",
                "line 3: file \"08a\" is not the camera's number in quotes, such as \"008\"",
            ),
            (
                "folder = \"x\"\n[a]\nmode = \"8K60\"\n",
                "line 3: mode \"8K60\" is none of 8K30, 8K25, 5.7K60, 4K120, 5.7K30",
            ),
            (
                "folder = \"x\"\n[a]\ntripod_height_m = -1.5\n",
                "line 3: tripod_height_m `-1.5` is not a height in metres",
            ),
            (
                "folder = \"x\"\n[a]\nmode = \"8K30\"\n",
                "line 2: role `a` has no `take`",
            ),
            (
                "folder = \"x\"\n[a]\ntake = 1\n[b]\n",
                "line 2: role `a` has no `mode`",
            ),
        ];
        for (text, says) in bad {
            let e = must_fail(says, parse(text));
            assert!(e.ends_with(says), "{e}");
        }
    }

    #[test]
    fn a_role_is_found_by_its_file_number() {
        let base = std::env::temp_dir().join(format!("spherecut-roles-{}", std::process::id()));
        let guard = TempDir(base.clone());
        let camera = base.join("insta360-x5");
        std::fs::create_dir_all(&camera).unwrap();
        let clip8 = entry("tripod_seam");
        // Control: an empty folder hasn't clip 8, and says what it looked
        // for.
        let why = must_fail("clip 8 in an empty folder", master_in(&base, clip8));
        assert_eq!(
            why,
            format!(
                "take 8 (tripod_seam): no file ending in _00_008.insv in {}",
                camera.display()
            )
        );
        // The camera's files, beside a sidecar and another take that
        // aren't clip 8's.
        for name in [
            "VID_20261022_165829_00_008.insv",
            "LRV_20261022_165829_01_008.lrv",
            "VID_20261022_165829_00_008.insv.trim_thumbnail.bin",
            "LRV_20261022_165829_01_008.lrv.thumbnail.bin",
            "VID_20261022_170000_00_1008.insv",
        ] {
            std::fs::write(camera.join(name), b"").unwrap();
        }
        let master = camera.join("VID_20261022_165829_00_008.insv");
        let lrv = camera.join("LRV_20261022_165829_01_008.lrv");
        // Under the samples folder, as hw-test gives it, or as the camera's
        // folder itself, as the cloud does.
        assert_eq!(master_in(&base, clip8), Ok(master.clone()));
        assert_eq!(lrv_in(&base, clip8), Ok(lrv.clone()));
        assert_eq!(master_in(&camera, clip8), Ok(master.clone()));
        assert_eq!(lrv_in(&camera, clip8), Ok(lrv));
        // Control: two files for one take are refused.
        std::fs::write(camera.join("VID_20261023_090000_00_008.insv"), b"").unwrap();
        let why = must_fail("two files for clip 8", master_in(&base, clip8));
        assert!(
            why.starts_with("take 8 (tripod_seam): 2 files end in _00_008.insv in "),
            "{why}"
        );
        drop(guard);
    }

    #[test]
    fn a_role_without_its_file_skips() {
        // A take still to come skips before the samples folder is looked
        // for, naming the take.
        let says = "TIER2-SKIP: take 14 (open_field) has no file yet: samples-roles.toml names \
                    it once Pat's take is in";
        assert_eq!(
            must_fail("take 14 before it is shot", outcome(|| role("open_field"))),
            says
        );
        assert_eq!(must_fail("its LRV", outcome(|| lrv("open_field"))), says);
        let why = must_fail(
            "take 14 in any folder",
            master_in(&std::env::temp_dir(), entry("open_field")),
        );
        assert_eq!(format!("{} {why}", tier2::SKIP_MARKER), says);
    }

    #[test]
    fn an_unknown_role_fails_rather_than_skips() {
        let why = must_fail("a misspelt role", outcome(|| entry("tripod_sean").take));
        assert_eq!(why, "no role `tripod_sean` in samples-roles.toml");
    }
}
