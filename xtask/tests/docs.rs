//! The docs held to the code. `docs/perf.md` has a paragraph for every
//! benchmark, each of its targets names Tier 2 tests that exist, and its
//! links to `hw-reports/` resolve (F091). Every `SPHERECUT_*` variable the
//! code reads is documented (F077), DECISIONS.md's index lists its entries
//! as their headings do (F076), and ARCHITECTURE.md's crate map and table
//! name every crate in the workspace (F074). Each check also runs on an
//! input made to fail it (D32).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use spherecut_testkit::control::must_fail;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn perf_md() -> String {
    std::fs::read_to_string(root().join("docs/perf.md")).unwrap()
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "target") {
                rust_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// A function in a source file: its name, whether it is a `#[test]`, and
/// whether it is a Tier 2 one (`#[ignore = "tier2...`).
#[derive(Debug, PartialEq)]
struct Fn {
    name: String,
    test: bool,
    tier2: bool,
}

/// The functions in `src`, each with the attributes right above it: the
/// lines back to a blank one, a comment, or the end of an item.
fn fns(src: &str) -> Vec<Fn> {
    let lines: Vec<&str> = src.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("fn ").or_else(|| t.strip_prefix("pub fn ")) else {
            continue;
        };
        let Some(name) = rest.split(['(', '<']).next() else {
            continue;
        };
        let mut attrs = String::new();
        for above in lines[..i].iter().rev() {
            let a = above.trim();
            if a.is_empty()
                || a.starts_with("//")
                || a.ends_with('}')
                || a.ends_with(';')
                || a.ends_with('{')
            {
                break;
            }
            attrs.push_str(a);
            attrs.push('\n');
        }
        found.push(Fn {
            name: name.to_string(),
            test: attrs.lines().any(|a| a == "#[test]"),
            tier2: attrs.contains("#[ignore = \"tier2"),
        });
    }
    found
}

/// Every function under `crates/` and `xtask/`.
fn all_fns() -> Vec<Fn> {
    let mut files = Vec::new();
    rust_files(&root().join("crates"), &mut files);
    rust_files(&root().join("xtask"), &mut files);
    files
        .iter()
        .flat_map(|f| fns(&std::fs::read_to_string(f).unwrap()))
        .collect()
}

/// The benchmarks among `fns`: tests named `bench_*`, Tier 2 or not.
fn benchmarks(fns: &[Fn]) -> BTreeSet<String> {
    fns.iter()
        .filter(|f| f.test && f.name.starts_with("bench_"))
        .map(|f| f.name.clone())
        .collect()
}

/// The benchmarks `perf` doesn't name in backticks.
fn without_paragraph(benches: &BTreeSet<String>, perf: &str) -> Result<(), Vec<String>> {
    let missing: Vec<String> = benches
        .iter()
        .filter(|b| !perf.contains(&format!("`{b}`")))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[test]
fn every_benchmark_has_its_paragraph_in_perf_md() {
    let benches = benchmarks(&all_fns());
    assert!(
        benches.len() >= 15,
        "only {} found: {benches:?}",
        benches.len()
    );
    let perf = perf_md();
    assert_eq!(without_paragraph(&benches, &perf), Ok(()));
    // A helper named bench_* isn't a benchmark.
    assert!(benchmarks(&fns("fn bench_y() {}\n")).is_empty());
    assert!(
        benchmarks(&fns(
            "/// A helper.\nfn bench_z(x: u32) -> u32 {\n    x\n}\n"
        ))
        .is_empty()
    );
    // The controls: perf.md without the lines naming a benchmark, and a
    // benchmark perf.md doesn't name.
    let cut: String = perf
        .lines()
        .filter(|l| !l.contains("bench_export_4k30_vaapi"))
        .collect::<Vec<_>>()
        .join("\n");
    let missing = must_fail(
        "perf.md without bench_export_4k30_vaapi's lines",
        without_paragraph(&benches, &cut),
    );
    assert_eq!(missing, ["bench_export_4k30_vaapi"]);
    let planted = benchmarks(&fns("#[test]\n#[ignore = \"tier2\"]\nfn bench_x() {}\n"));
    assert_eq!(planted.len(), 1);
    must_fail(
        "a benchmark perf.md doesn't name",
        without_paragraph(&planted, &perf),
    );
}

/// The rows of `perf`'s Targets table, header and rule left out.
fn target_rows(perf: &str) -> Vec<&str> {
    perf.lines()
        .skip_while(|l| !l.starts_with("## Targets"))
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| l.starts_with('|'))
        .skip(2)
        .collect()
}

/// The `bench_*` and `gui_*` names `row` gives in backticks.
fn named_tests(row: &str) -> Vec<&str> {
    row.split('`')
        .skip(1)
        .step_by(2)
        .filter(|n| {
            (n.starts_with("bench_") || n.starts_with("gui_"))
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .collect()
}

/// Why `rows` don't each name Tier 2 tests in `tier2`: a row naming none,
/// or a name that isn't one.
fn targets_unmeasured(rows: &[&str], tier2: &BTreeSet<String>) -> Result<(), Vec<String>> {
    let mut why = Vec::new();
    for row in rows {
        let names = named_tests(row);
        let target = row.split('|').nth(1).unwrap_or("").trim();
        if names.is_empty() {
            why.push(format!("{target}: names no Tier 2 test"));
        }
        for n in names.iter().filter(|n| !tier2.contains(**n)) {
            why.push(format!("{target}: `{n}` is no Tier 2 test"));
        }
    }
    if why.is_empty() { Ok(()) } else { Err(why) }
}

#[test]
fn perf_targets_name_tests_that_exist() {
    let tier2: BTreeSet<String> = all_fns()
        .into_iter()
        .filter(|f| f.test && f.tier2)
        .map(|f| f.name)
        .collect();
    let perf = perf_md();
    let rows = target_rows(&perf);
    assert!(rows.len() >= 6, "{rows:#?}");
    assert_eq!(targets_unmeasured(&rows, &tier2), Ok(()));
    // The controls: a row naming a test that doesn't exist, and a row
    // naming none.
    let why = must_fail(
        "a target measured by bench_nonesuch",
        targets_unmeasured(&["| A target | `bench_nonesuch` |"], &tier2),
    );
    assert_eq!(why, ["A target: `bench_nonesuch` is no Tier 2 test"]);
    let why = must_fail(
        "a target measured by no test",
        targets_unmeasured(&["| A target | export benchmark (M2) |"], &tier2),
    );
    assert_eq!(why, ["A target: names no Tier 2 test"]);
}

/// The `../hw-reports/` links in `perf` whose file isn't in `root`.
fn broken_links(perf: &str, root: &Path) -> Result<(), Vec<String>> {
    let broken: Vec<String> = perf
        .split("](../hw-reports/")
        .skip(1)
        .filter_map(|rest| rest.split([')', '#']).next())
        .filter(|name| !root.join("hw-reports").join(name).exists())
        .map(str::to_owned)
        .collect();
    if broken.is_empty() {
        Ok(())
    } else {
        Err(broken)
    }
}

#[test]
fn every_hw_report_link_resolves() {
    let perf = perf_md();
    let links = perf.matches("](../hw-reports/").count();
    assert!(links >= 10, "{links} links");
    assert_eq!(broken_links(&perf, &root()), Ok(()));
    // The control: a link to a report that isn't there.
    let planted = format!("{perf}\nSee [it](../hw-reports/odin-nonesuch.md).\n");
    let broken = must_fail(
        "a link to a missing report",
        broken_links(&planted, &root()),
    );
    assert_eq!(broken, ["odin-nonesuch.md"]);
}

/// The names a `SPHERECUT_` string literal opens in `src`: each
/// `"SPHERECUT_` followed by capitals, digits and underscores, as every
/// `env::var` call and constant naming a variable has it. A literal that
/// is the bare prefix names nothing.
fn variables_read(src: &str) -> BTreeSet<String> {
    const PREFIX: &str = "SPHERECUT_";
    let mut found = BTreeSet::new();
    for (i, _) in src.match_indices(PREFIX) {
        if !src[..i].ends_with('"') {
            continue;
        }
        let rest = &src[i + PREFIX.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(rest.len());
        if end > 0 {
            found.insert(format!("{PREFIX}{}", &rest[..end]));
        }
    }
    found
}

/// Every `SPHERECUT_` name `docs` gives, whole: `SPHERECUT_FFMPEG_COMPAT`
/// doesn't count as written where only `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`
/// is.
fn variables_named(docs: &str) -> BTreeSet<String> {
    docs.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with("SPHERECUT_") && w.len() > "SPHERECUT_".len())
        .map(str::to_owned)
        .collect()
}

/// The variables in `read` that `docs` never names.
fn undocumented(read: &BTreeSet<String>, docs: &str) -> Result<(), Vec<String>> {
    let named = variables_named(docs);
    let missing: Vec<String> = read.difference(&named).cloned().collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// README.md, ARCHITECTURE.md and every Markdown file under `docs/`, one
/// after the other.
fn user_docs() -> String {
    fn markdown(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                markdown(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    let mut files = vec![root().join("README.md"), root().join("ARCHITECTURE.md")];
    markdown(&root().join("docs"), &mut files);
    files
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_environment_variable_is_documented() {
    let mut files = Vec::new();
    rust_files(&root().join("crates"), &mut files);
    rust_files(&root().join("xtask"), &mut files);
    let read: BTreeSet<String> = files
        .iter()
        .flat_map(|f| variables_read(&std::fs::read_to_string(f).unwrap()))
        .collect();
    assert!(read.len() >= 5, "only {} found: {read:?}", read.len());
    assert!(read.contains("SPHERECUT_EXPORT_DEVICE"), "{read:?}");
    let docs = user_docs();
    assert_eq!(undocumented(&read, &docs), Ok(()));
    // A longer name doesn't stand for a shorter one, nor a prefix for a
    // name.
    assert_eq!(
        variables_named("SPHERECUT_FFMPEG_COMPAT_TARGET_DIR, SPHERECUT_*"),
        BTreeSet::from(["SPHERECUT_FFMPEG_COMPAT_TARGET_DIR".to_owned()])
    );
    // The controls: the docs with every line naming SPHERECUT_EXPORT_DEVICE
    // cut, and a source reading a variable no doc names. The planted name
    // is split here so that this file doesn't read it.
    let cut: String = docs
        .lines()
        .filter(|l| !l.contains("SPHERECUT_EXPORT_DEVICE"))
        .collect::<Vec<_>>()
        .join("\n");
    let missing = must_fail(
        "the docs without SPHERECUT_EXPORT_DEVICE",
        undocumented(&read, &cut),
    );
    assert_eq!(missing, ["SPHERECUT_EXPORT_DEVICE"]);
    let planted = variables_read(concat!(
        "let v = std::env::var(\"",
        "SPHERECUT",
        "_NOT_A_VAR\");"
    ));
    assert_eq!(planted.len(), 1, "{planted:?}");
    let missing = must_fail(
        "a source reading a variable no doc names",
        undocumented(&planted, &docs),
    );
    assert_eq!(missing, [concat!("SPHERECUT", "_NOT_A_VAR")]);
}

/// DECISIONS.md's index (the `- D<n>. <title>` lines under `Index:`) and
/// its `## D<n>. <title>` headings, each as `D<n>. <title>`.
fn index_and_headings(decisions: &str) -> (Vec<&str>, Vec<&str>) {
    let index = decisions
        .lines()
        .skip_while(|l| *l != "Index:")
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("- "))
        .filter(|l| l.starts_with('D'))
        .collect();
    let headings = decisions
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter(|l| l.starts_with('D') && l[1..].starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    (index, headings)
}

/// Where DECISIONS.md's index and its headings part, and whether D60,
/// M3's verification contract, has its heading.
fn index_disagrees(decisions: &str) -> Result<(), Vec<String>> {
    let (index, headings) = index_and_headings(decisions);
    let mut why = Vec::new();
    for h in headings.iter().filter(|h| !index.contains(h)) {
        why.push(format!("not in the index: {h}"));
    }
    for i in index.iter().filter(|i| !headings.contains(i)) {
        why.push(format!("no heading: {i}"));
    }
    if why.is_empty() && index != headings {
        why.push(format!(
            "the index's order isn't the headings': {index:?} against {headings:?}"
        ));
    }
    if !headings.iter().any(|h| h.starts_with("D60. ")) {
        why.push("no D60 heading".to_owned());
    }
    if why.is_empty() { Ok(()) } else { Err(why) }
}

#[test]
fn the_decisions_index_lists_every_entry() {
    let decisions = std::fs::read_to_string(root().join("DECISIONS.md")).unwrap();
    let (index, headings) = index_and_headings(&decisions);
    assert!(headings.len() >= 60, "{} headings", headings.len());
    assert_eq!(index.len(), headings.len());
    assert_eq!(index_disagrees(&decisions), Ok(()));
    // The controls: the index without D77's line, and DECISIONS without
    // D60's heading.
    let without_d77: String = decisions
        .lines()
        .filter(|l| !l.starts_with("- D77. "))
        .collect::<Vec<_>>()
        .join("\n");
    let why = must_fail("the index without D77", index_disagrees(&without_d77));
    assert_eq!(why.len(), 1, "{why:?}");
    assert!(why[0].starts_with("not in the index: D77. "), "{why:?}");
    let without_d60: String = decisions
        .lines()
        .filter(|l| !l.starts_with("## D60. "))
        .collect::<Vec<_>>()
        .join("\n");
    let why = must_fail(
        "DECISIONS without D60's heading",
        index_disagrees(&without_d60),
    );
    assert_eq!(
        why,
        [
            "no heading: D60. M3's verification contract",
            "no D60 heading"
        ]
    );
}

/// The workspace's members, from the root `Cargo.toml`'s `members`, each
/// as its folder's name and its package's.
fn members() -> Vec<(String, String)> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    let line = manifest
        .lines()
        .find(|l| l.starts_with("members = "))
        .unwrap();
    let mut dirs = Vec::new();
    for pattern in line.split('"').skip(1).step_by(2) {
        match pattern.strip_suffix("/*") {
            Some(parent) => {
                for e in std::fs::read_dir(root().join(parent)).unwrap().flatten() {
                    if e.path().join("Cargo.toml").is_file() {
                        dirs.push(e.path());
                    }
                }
            }
            None => dirs.push(root().join(pattern)),
        }
    }
    let mut found: Vec<(String, String)> = dirs
        .iter()
        .map(|d| {
            let toml = std::fs::read_to_string(d.join("Cargo.toml")).unwrap();
            let name = toml
                .lines()
                .find_map(|l| l.strip_prefix("name = \""))
                .and_then(|l| l.strip_suffix('"'))
                .unwrap()
                .to_owned();
            (d.file_name().unwrap().to_string_lossy().into_owned(), name)
        })
        .collect();
    found.sort();
    found
}

/// The crates `diagram` gives a row of their own: a line's first word,
/// after its level if it has one.
fn diagram_rows(diagram: &str) -> BTreeSet<&str> {
    diagram
        .lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let first = words.next()?;
            if first.chars().all(|c| c.is_ascii_digit()) {
                words.next()
            } else {
                Some(first)
            }
        })
        .collect()
}

/// ARCHITECTURE.md's first code block, the crate map.
fn first_code_block(architecture: &str) -> &str {
    let after = architecture.split_once("\n```\n").unwrap().1;
    after.split_once("\n```").unwrap().0
}

/// The packages the rows of ARCHITECTURE.md's crate table name: each
/// row's first cell, in backticks, under `## Crates`.
fn table_rows(architecture: &str) -> BTreeSet<&str> {
    architecture
        .lines()
        .skip_while(|l| *l != "## Crates")
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once("` |"))
        .map(|(name, _)| name)
        .collect()
}

/// The names in `wanted` that `have` leaves out.
fn left_out<'a>(
    wanted: impl IntoIterator<Item = &'a str>,
    have: &BTreeSet<&str>,
) -> Result<(), Vec<String>> {
    let missing: Vec<String> = wanted
        .into_iter()
        .filter(|w| !have.contains(w))
        .map(str::to_owned)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// ARCHITECTURE.md's crate map at 073715b, before agent and the test
/// support had rows.
const DIAGRAM_073715B: &str = r"
          app (eframe viewer; M1 slice 7)   cli (`spherecut` binary)   tests
                          \                |                    /
                           +---- export (M2.4 and M2.5) -------+
                                      |
   playback (Player, decode workers, frame cache, GPU slots, presenter, the file's audio; M1 slices 5 and 6)
                     |                                    |
   render (wgpu on Vulkan, WGSL; M1 slice 3)   audio (cpal output, chunk ring, audio clock; M1 slice 6)
                                      |
                    motion (IMU, stabilization)  (planned)
                                      |
     media (FFmpeg, MediaBackend and frame-exact seek, probing, MP4 boxes, doctor)
                                      |
        camera (cameras' raw files from their bytes: the Insta360 trailer; M3.2)
                                      |
   sphere (projection math)      core (time, frames, colour, stop tokens, clocks, the UI thread's guard, atomic writes, the project model)
";

#[test]
fn architecture_names_every_crate() {
    let members = members();
    assert!(members.len() >= 14, "{members:?}");
    let architecture = std::fs::read_to_string(root().join("ARCHITECTURE.md")).unwrap();
    let diagram = first_code_block(&architecture);
    let rows = diagram_rows(diagram);
    let folders = || members.iter().map(|(folder, _)| folder.as_str());
    let packages = || members.iter().map(|(_, package)| package.as_str());
    assert_eq!(left_out(folders(), &rows), Ok(()));
    let table = table_rows(&architecture);
    assert_eq!(left_out(packages(), &table), Ok(()));
    // The controls: 073715b's map, which had no row for agent; today's map
    // with core's row cut, though other rows still name core among their
    // dependencies; and the table without agent's row.
    let missing = must_fail(
        "073715b's crate map",
        left_out(folders(), &diagram_rows(DIAGRAM_073715B)),
    );
    assert!(missing.iter().any(|m| m == "agent"), "{missing:?}");
    let cut: String = diagram
        .lines()
        .filter(|l| !l.trim_start().starts_with("1    core"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(cut.contains(" core "));
    let missing = must_fail(
        "the map without core's row",
        left_out(folders(), &diagram_rows(&cut)),
    );
    assert_eq!(missing, ["core"]);
    let cut: String = architecture
        .lines()
        .filter(|l| !l.starts_with("| `spherecut-agent` |"))
        .collect::<Vec<_>>()
        .join("\n");
    let missing = must_fail(
        "the table without agent's row",
        left_out(packages(), &table_rows(&cut)),
    );
    assert_eq!(missing, ["spherecut-agent"]);
}
