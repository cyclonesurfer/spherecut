//! How many bytes of decoded frames the frame cache may hold (M1 plan
//! section 8.3, DECISIONS D34).
//!
//! [`CacheBudget::Auto`] sizes it from the memory the kernel says is
//! available: clamp(¼ × `MemAvailable`, 512 MiB, 8 GiB), capped at half
//! the room a cgroup v2 memory limit leaves (Flatpak, containers, a
//! systemd slice), `memory.max` − `memory.current`, so that the cache
//! never takes more than half of what the process may still use. The
//! limits read are those of the process's cgroup and its ancestors up to
//! the mount's root, which in a container with its own cgroup namespace
//! is the container's. On the
//! machines with an iGPU, `MemAvailable` already counts what the GPU
//! uses, since it shares the RAM. [`CacheBudget::Bytes`] is a size given
//! in Settings, with `--cache-mb`, or in `SPHERECUT_FRAME_CACHE_MIB`.
//!
//! The arithmetic is pure, over the text of `/proc/meminfo` and the
//! cgroup files, so that tests feed it the machines it runs on
//! ([`auto_budget`]); [`read_auto_budget`] reads the files.

use std::path::{Path, PathBuf};

use thiserror::Error;

pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;

/// The automatic budget's floor and cap.
pub const AUTO_MIN: u64 = 512 * MIB;
pub const AUTO_MAX: u64 = 8 * GIB;

/// Below this much available memory, a recheck shrinks the budget under
/// [`AUTO_MIN`] (and an explicit one too), evicting history first.
pub const PRESSURE: u64 = GIB;

/// The environment variable that sets the budget in MiB, or `auto`.
pub const ENV: &str = "SPHERECUT_FRAME_CACHE_MIB";

/// The frame cache's budget, as configured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheBudget {
    /// From the memory available ([`auto_budget`]).
    #[default]
    Auto,
    /// This many bytes.
    Bytes(u64),
}

/// A budget that can't be read.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BudgetError {
    #[error("`{0}` is not a whole number of MiB or `auto`")]
    NotANumber(String),
    #[error("a frame cache of 0 MiB holds nothing; give at least 1, or `auto`")]
    Zero,
    #[error("{0} MiB is more memory than can be addressed")]
    TooLarge(u64),
}

/// Where the budget in force came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetFrom {
    /// `--cache-mb`.
    Flag,
    /// [`ENV`].
    Env,
    /// The settings (or their default, [`CacheBudget::Auto`]).
    Settings,
}

/// The budget in force, where it came from, and an environment value
/// that was set but couldn't be read, which the caller warns about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenBudget {
    pub budget: CacheBudget,
    pub from: BudgetFrom,
    pub env_error: Option<BudgetError>,
}

impl CacheBudget {
    /// A size in MiB, as `--cache-mb` and [`ENV`] give it, or `auto`.
    pub fn parse_mib(s: &str) -> Result<CacheBudget, BudgetError> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            return Ok(CacheBudget::Auto);
        }
        let mib: u64 = s
            .parse()
            .map_err(|_| BudgetError::NotANumber(s.to_owned()))?;
        if mib == 0 {
            return Err(BudgetError::Zero);
        }
        mib.checked_mul(MIB)
            .map(CacheBudget::Bytes)
            .ok_or(BudgetError::TooLarge(mib))
    }

    /// The budget in force: the command line's, else the environment's
    /// (`env` is [`ENV`]'s value), else the settings'. An environment
    /// value that can't be read is passed over, and returned to warn
    /// about.
    pub fn choose(
        flag: Option<CacheBudget>,
        env: Option<&str>,
        settings: CacheBudget,
    ) -> ChosenBudget {
        let env = env.map(CacheBudget::parse_mib);
        let env_error = env.clone().and_then(Result::err);
        let (budget, from) = match (flag, env) {
            (Some(b), _) => (b, BudgetFrom::Flag),
            (None, Some(Ok(b))) => (b, BudgetFrom::Env),
            _ => (settings, BudgetFrom::Settings),
        };
        ChosenBudget {
            budget,
            from,
            env_error,
        }
    }

    /// [`ENV`]'s value, if it is set (and valid Unicode).
    pub fn env_value() -> Option<String> {
        std::env::var(ENV).ok()
    }

    /// The bytes it stands for: an explicit size as it is, `Auto` from
    /// `auto` (the system's, from [`read_auto_budget`]).
    pub fn bytes(self, auto: &AutoBudget) -> u64 {
        match self {
            CacheBudget::Auto => auto.bytes,
            CacheBudget::Bytes(b) => b,
        }
    }
}

/// The automatic budget, and what it was worked out from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoBudget {
    pub bytes: u64,
    /// `MemAvailable`, in bytes; `None` when `/proc/meminfo` doesn't say,
    /// which leaves the budget at [`AUTO_MIN`] (or under a cgroup's cap).
    pub mem_available: Option<u64>,
    /// What a cgroup v2 memory limit leaves, `memory.max` −
    /// `memory.current`, at the tightest of the process's cgroup and its
    /// ancestors; `None` without a limit.
    pub cgroup_room: Option<u64>,
}

/// The automatic budget from the text of `/proc/meminfo` and of a cgroup
/// v2 `memory.max` and `memory.current` (`None` where there is no such
/// file): clamp(¼ × `MemAvailable`, 512 MiB, 8 GiB), capped at half of
/// `memory.max` − `memory.current`. `memory.max` reading `max` means no
/// limit. Text that can't be read counts as saying nothing: no
/// `MemAvailable` leaves the floor, and a `memory.max` that isn't a
/// number no cap. A `memory.current` that can't be read counts as 0.
pub fn auto_budget(meminfo: &str, cgroup_max: Option<&str>, cgroup_current: Option<&str>) -> u64 {
    auto_budget_detail(meminfo, cgroup_max, cgroup_current).bytes
}

/// [`auto_budget`], with what it was worked out from.
pub fn auto_budget_detail(
    meminfo: &str,
    cgroup_max: Option<&str>,
    cgroup_current: Option<&str>,
) -> AutoBudget {
    budget_from(
        mem_available(meminfo),
        cgroup_room(cgroup_max, cgroup_current),
    )
}

/// The automatic budget from `MemAvailable` and a cgroup's room, both in
/// bytes.
pub fn budget_from(mem_available: Option<u64>, cgroup_room: Option<u64>) -> AutoBudget {
    let mut bytes = mem_available.map_or(AUTO_MIN, |a| (a / 4).clamp(AUTO_MIN, AUTO_MAX));
    if let Some(room) = cgroup_room {
        bytes = bytes.min(room / 2);
    }
    AutoBudget {
        bytes,
        mem_available,
        cgroup_room,
    }
}

/// `MemAvailable` from `/proc/meminfo`'s text, in bytes: the kernel
/// writes it in kB, which are KiB.
pub fn mem_available(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemAvailable:")?;
        let mut words = rest.split_whitespace();
        let kib: u64 = words.next()?.parse().ok()?;
        match (words.next(), words.next()) {
            (Some("kB"), None) => kib.checked_mul(1024),
            _ => None,
        }
    })
}

/// The room a cgroup v2 memory limit leaves, `memory.max` −
/// `memory.current`, from their text; `None` for no limit (`max`, no
/// file, or text that isn't a number).
pub fn cgroup_room(max: Option<&str>, current: Option<&str>) -> Option<u64> {
    let max: u64 = max?.trim().parse().ok()?;
    let current = current
        .and_then(|c| c.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Some(max.saturating_sub(current))
}

/// The budget a recheck sets (every few seconds, on the upload thread):
/// what `configured` stands for now, and under memory pressure, with
/// less than [`PRESSURE`] available, no more than ¼ of what is, below the
/// automatic floor and below an explicit size.
pub fn recheck(configured: CacheBudget, now: &AutoBudget) -> u64 {
    let bytes = configured.bytes(now);
    match now.mem_available {
        Some(a) if a < PRESSURE => bytes.min(a / 4),
        _ => bytes,
    }
}

/// The automatic budget of this process: `/proc/meminfo`, and the cgroup
/// v2 limits of its cgroup and every ancestor. Reads files, so it blocks.
pub fn read_auto_budget() -> AutoBudget {
    read_auto_budget_at(Path::new("/proc"), Path::new("/sys/fs/cgroup"))
}

/// [`read_auto_budget`] with `/proc` at `proc` and the cgroup v2 mount at
/// `cgroup_mount`, for tests.
pub fn read_auto_budget_at(proc: &Path, cgroup_mount: &Path) -> AutoBudget {
    spherecut_core::may_block("reading the memory available");
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    let meminfo = read(&proc.join("meminfo")).unwrap_or_default();
    let room = read(&proc.join("self/cgroup"))
        .and_then(|text| cgroup_path(&text))
        .map(|rel| cgroup_dirs(cgroup_mount, &rel))
        .into_iter()
        .flatten()
        .filter_map(|dir| {
            cgroup_room(
                read(&dir.join("memory.max")).as_deref(),
                read(&dir.join("memory.current")).as_deref(),
            )
        })
        .min();
    budget_from(mem_available(&meminfo), room)
}

/// The cgroup v2 path in `/proc/self/cgroup`'s text: the `0::` line.
fn cgroup_path(text: &str) -> Option<PathBuf> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| PathBuf::from(p.trim().trim_start_matches('/')))
}

/// The cgroup at `rel` under `mount` and each of its ancestors, whose
/// limits all apply, up to the mount's root. The host's root cgroup has
/// no `memory.max`; in a container with its own cgroup namespace (Docker
/// and Podman on cgroup v2), `/proc/self/cgroup` reads `0::/` and the
/// root is the container's, with its limit.
fn cgroup_dirs(mount: &Path, rel: &Path) -> Vec<PathBuf> {
    rel.ancestors().map(|a| mount.join(a)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/proc/meminfo` with `MemAvailable` in kB, laid out as the kernel
    /// writes it.
    fn meminfo(total_kb: u64, available_kb: u64) -> String {
        format!(
            "MemTotal:       {total_kb:>9} kB\nMemFree:          812344 kB\n\
             MemAvailable:   {available_kb:>9} kB\nBuffers:            1024 kB\n\
             Cached:          4401232 kB\nSwapCached:            0 kB\n"
        )
    }

    const KB_PER_GIB: u64 = 1024 * 1024;

    #[test]
    fn huginn_takes_the_cap() {
        // 128 GB of unified memory, most of it free: a quarter is far past
        // the cap. The plan's table: 8 GiB, 345 frames of 5.7K NV12.
        let text = meminfo(131_072_000, 118 * KB_PER_GIB);
        assert_eq!(auto_budget(&text, None, None), AUTO_MAX);
        let frame = 5760 * 2880 * 3 / 2;
        assert_eq!(AUTO_MAX / frame, 345);
    }

    #[test]
    fn odin_takes_a_quarter() {
        // 32 GB with about 24 GiB available: about 6 GiB, as the plan's
        // table has it, about 258 frames of 5.7K NV12.
        let text = meminfo(32_768_000, 24 * KB_PER_GIB);
        let b = auto_budget(&text, None, None);
        assert_eq!(b, 6 * GIB);
        assert_eq!(b / (5760 * 2880 * 3 / 2), 258);
    }

    #[test]
    fn a_small_machine_keeps_the_floor() {
        let text = meminfo(4_000_000, KB_PER_GIB);
        assert_eq!(auto_budget(&text, None, None), AUTO_MIN);
    }

    #[test]
    fn a_container_limit_caps_it() {
        // 64 GiB available on the host, but the container may use 4 GiB
        // and uses 1 GiB: half of the 3 GiB left.
        let text = meminfo(67_108_864, 64 * KB_PER_GIB);
        let max = format!("{}\n", 4 * GIB);
        let current = format!("{}\n", GIB);
        let d = auto_budget_detail(&text, Some(&max), Some(&current));
        assert_eq!(d.bytes, 3 * GIB / 2);
        assert_eq!(d.cgroup_room, Some(3 * GIB));
        assert_eq!(d.mem_available, Some(64 * GIB));
        // The cap goes below the floor: half of what the limit leaves.
        let current = format!("{}\n", 4 * GIB - 200 * MIB);
        assert_eq!(auto_budget(&text, Some(&max), Some(&current)), 100 * MIB);
        // Over the limit (it happens, briefly), nothing is left.
        let current = format!("{}\n", 5 * GIB);
        assert_eq!(auto_budget(&text, Some(&max), Some(&current)), 0);
    }

    #[test]
    fn max_means_no_limit() {
        let text = meminfo(67_108_864, 16 * KB_PER_GIB);
        let d = auto_budget_detail(&text, Some("max\n"), Some("123456789\n"));
        assert_eq!(d.bytes, 4 * GIB);
        assert_eq!(d.cgroup_room, None);
    }

    #[test]
    fn malformed_text_says_nothing() {
        // No MemAvailable (or one that isn't kB, or isn't a number): the
        // floor.
        for text in [
            "",
            "MemTotal: 1000 kB\n",
            "MemAvailable: lots kB\n",
            "MemAvailable: 1000 MB\n",
            "MemAvailable: 1000\n",
            "MemAvailable: 1000 kB extra\n",
            "MemAvailable: -5 kB\n",
            "MemAvailable: 99999999999999999999 kB\n",
            "\u{0}\u{ff}garbage",
        ] {
            let d = auto_budget_detail(text, None, None);
            assert_eq!((d.bytes, d.mem_available), (AUTO_MIN, None), "{text:?}");
        }
        // A memory.max that isn't a number is no cap; a memory.current
        // that isn't counts as 0.
        let text = meminfo(67_108_864, 16 * KB_PER_GIB);
        assert_eq!(auto_budget(&text, Some("lots\n"), Some("1\n")), 4 * GIB);
        assert_eq!(auto_budget(&text, Some(""), None), 4 * GIB);
        let max = format!("{}\n", 2 * GIB);
        assert_eq!(auto_budget(&text, Some(&max), Some("??")), GIB);
        assert_eq!(auto_budget(&text, Some(&max), None), GIB);
        // A kB count past what fits in bytes says nothing either.
        let huge = format!("MemAvailable: {} kB\n", u64::MAX / 512);
        assert_eq!(mem_available(&huge), None);
    }

    #[test]
    fn the_first_mem_available_line_counts() {
        let text = "MemAvailableX: 5 kB\nMemAvailable: 4194304 kB\nMemAvailable: 1 kB\n";
        assert_eq!(mem_available(text), Some(4 * GIB));
    }

    #[test]
    fn budgets_read_from_the_command_line_and_the_environment() {
        assert_eq!(
            CacheBudget::parse_mib("256"),
            Ok(CacheBudget::Bytes(256 * MIB))
        );
        assert_eq!(
            CacheBudget::parse_mib(" 1024\n"),
            Ok(CacheBudget::Bytes(GIB))
        );
        assert_eq!(CacheBudget::parse_mib("AUTO"), Ok(CacheBudget::Auto));
        assert_eq!(CacheBudget::parse_mib("0"), Err(BudgetError::Zero));
        assert_eq!(
            CacheBudget::parse_mib("1.5"),
            Err(BudgetError::NotANumber("1.5".into()))
        );
        assert_eq!(
            CacheBudget::parse_mib("-3"),
            Err(BudgetError::NotANumber("-3".into()))
        );
        let big = u64::MAX / MIB + 1;
        assert_eq!(
            CacheBudget::parse_mib(&big.to_string()),
            Err(BudgetError::TooLarge(big))
        );
        assert_eq!(
            BudgetError::Zero.to_string(),
            "a frame cache of 0 MiB holds nothing; give at least 1, or `auto`"
        );
    }

    #[test]
    fn the_flag_wins_then_the_environment_then_the_settings() {
        let set = CacheBudget::Bytes(GIB);
        let c = CacheBudget::choose(Some(CacheBudget::Auto), Some("300"), set);
        assert_eq!(
            (c.budget, c.from, c.env_error),
            (CacheBudget::Auto, BudgetFrom::Flag, None)
        );
        let c = CacheBudget::choose(None, Some("300"), set);
        assert_eq!(
            (c.budget, c.from),
            (CacheBudget::Bytes(300 * MIB), BudgetFrom::Env)
        );
        let c = CacheBudget::choose(None, None, set);
        assert_eq!((c.budget, c.from), (set, BudgetFrom::Settings));
        // A value that can't be read is passed over, and said.
        let c = CacheBudget::choose(None, Some("big"), set);
        assert_eq!((c.budget, c.from), (set, BudgetFrom::Settings));
        assert_eq!(c.env_error, Some(BudgetError::NotANumber("big".into())));
        // Even when the flag wins.
        let c = CacheBudget::choose(Some(set), Some("0"), CacheBudget::Auto);
        assert_eq!(
            (c.from, c.env_error),
            (BudgetFrom::Flag, Some(BudgetError::Zero))
        );
    }

    #[test]
    fn a_recheck_shrinks_under_pressure() {
        let calm = budget_from(Some(16 * GIB), None);
        assert_eq!(recheck(CacheBudget::Auto, &calm), 4 * GIB);
        assert_eq!(recheck(CacheBudget::Bytes(12 * GIB), &calm), 12 * GIB);
        // 600 MiB available: a quarter, under the floor and under an
        // explicit size.
        let tight = budget_from(Some(600 * MIB), None);
        assert_eq!(tight.bytes, AUTO_MIN);
        assert_eq!(recheck(CacheBudget::Auto, &tight), 150 * MIB);
        assert_eq!(recheck(CacheBudget::Bytes(2 * GIB), &tight), 150 * MIB);
        assert_eq!(recheck(CacheBudget::Bytes(100 * MIB), &tight), 100 * MIB);
        // Nothing known: as configured.
        let unknown = budget_from(None, None);
        assert_eq!(recheck(CacheBudget::Auto, &unknown), AUTO_MIN);
    }

    /// A fake `/proc` and cgroup mount: the process's cgroup has no limit
    /// of its own, its parent the tightest, and the grandparent a looser
    /// one.
    #[test]
    fn the_tightest_cgroup_up_the_tree_counts() {
        let root = std::env::temp_dir().join(format!("spherecut-budget-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let proc = root.join("proc");
        let mount = root.join("cgroup");
        let leaf = mount.join("user.slice/app.slice/spherecut.scope");
        std::fs::create_dir_all(proc.join("self")).unwrap();
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::write(proc.join("meminfo"), meminfo(67_108_864, 32 * KB_PER_GIB)).unwrap();
        std::fs::write(
            proc.join("self/cgroup"),
            "0::/user.slice/app.slice/spherecut.scope\n",
        )
        .unwrap();
        let write = |dir: &str, max: &str, current: u64| {
            let d = mount.join(dir);
            std::fs::write(d.join("memory.max"), max).unwrap();
            std::fs::write(d.join("memory.current"), format!("{current}\n")).unwrap();
        };
        write("user.slice/app.slice/spherecut.scope", "max\n", GIB);
        write("user.slice/app.slice", &format!("{}\n", 6 * GIB), 2 * GIB);
        write("user.slice", &format!("{}\n", 20 * GIB), GIB);
        let d = read_auto_budget_at(&proc, &mount);
        assert_eq!(d.cgroup_room, Some(4 * GIB));
        assert_eq!(d.bytes, 2 * GIB);
        // Without cgroup v2 (no `0::` line), no cap.
        std::fs::write(proc.join("self/cgroup"), "1:memory:/x\n").unwrap();
        let d = read_auto_budget_at(&proc, &mount);
        assert_eq!((d.bytes, d.cgroup_room), (AUTO_MAX, None));
        // Nothing to read at all: the floor.
        let d = read_auto_budget_at(&root.join("none"), &root.join("none"));
        assert_eq!(d, budget_from(None, None));
        std::fs::remove_dir_all(&root).ok();
    }

    /// A container with its own cgroup namespace: `/proc/self/cgroup`
    /// reads `0::/`, and the limit is at the mount's root.
    #[test]
    fn a_cgroup_namespace_s_limit_at_the_root_counts() {
        let root = std::env::temp_dir().join(format!("spherecut-budget-ns-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let proc = root.join("proc");
        let mount = root.join("cgroup");
        std::fs::create_dir_all(proc.join("self")).unwrap();
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(proc.join("meminfo"), meminfo(67_108_864, 32 * KB_PER_GIB)).unwrap();
        std::fs::write(proc.join("self/cgroup"), "0::/\n").unwrap();
        std::fs::write(mount.join("memory.max"), format!("{}\n", 2 * GIB)).unwrap();
        std::fs::write(mount.join("memory.current"), format!("{}\n", GIB)).unwrap();
        let d = read_auto_budget_at(&proc, &mount);
        assert_eq!((d.bytes, d.cgroup_room), (GIB / 2, Some(GIB)));
        // And in a cgroup below it, the root's limit still applies.
        let leaf = mount.join("app");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::write(leaf.join("memory.max"), "max\n").unwrap();
        std::fs::write(proc.join("self/cgroup"), "0::/app\n").unwrap();
        let d = read_auto_budget_at(&proc, &mount);
        assert_eq!((d.bytes, d.cgroup_room), (GIB / 2, Some(GIB)));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn this_machine_has_a_budget() {
        let d = read_auto_budget();
        assert!(d.mem_available.is_some(), "{d:?}");
        assert!(d.bytes <= AUTO_MAX, "{d:?}");
    }
}
