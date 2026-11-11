//! The docs held to the code (F091): `docs/perf.md` has a paragraph for
//! every benchmark, each of its targets names Tier 2 tests that exist,
//! and its links to `hw-reports/` resolve. Each check also runs on an
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
