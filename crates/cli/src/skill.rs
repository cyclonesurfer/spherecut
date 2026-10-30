//! `spherecut agents install-skill` (a1-plan section 8): the skill
//! compiled into the agent crate, written to Spherecut's data folder and
//! linked where Claude Code (`~/.claude/skills`) and Codex, Gemini CLI and
//! Copilot CLI (`~/.agents/skills`) look for skills. Nothing already at
//! either name is replaced, and nothing else in those folders is touched.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use spherecut_cli::contract::Exit;
use spherecut_core::brand;
use spherecut_core::fs::TempFile;

/// Where the skill goes, and the names it is linked from.
#[derive(Debug)]
pub struct Places {
    /// `$XDG_DATA_HOME/spherecut/skills/spherecut`.
    pub folder: PathBuf,
    /// `~/.claude/skills/spherecut` and `~/.agents/skills/spherecut`.
    pub links: [PathBuf; 2],
}

impl Places {
    /// As `HOME` and `XDG_DATA_HOME` say (`~/.local/share` when it is
    /// unset or, as the XDG spec has it, relative).
    pub fn from_env() -> Result<Places> {
        let absolute = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let home = absolute("HOME").context("HOME isn't set to an absolute path")?;
        let data = absolute("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        Ok(Places::new(&home, &data))
    }

    fn new(home: &Path, data: &Path) -> Places {
        let name = brand::APP_SLUG;
        Places {
            folder: data.join(name).join("skills").join(name),
            links: [
                home.join(".claude/skills").join(name),
                home.join(".agents/skills").join(name),
            ],
        }
    }
}

/// What is at a link's name.
enum At {
    Nothing,
    /// A symlink to the skill's folder.
    Ours,
    /// Anything else, a symlink elsewhere included.
    Other,
}

fn at(link: &Path, folder: &Path) -> io::Result<At> {
    match std::fs::symlink_metadata(link) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(At::Nothing),
        Err(e) => Err(e),
        Ok(m) if m.file_type().is_symlink() && std::fs::read_link(link)? == folder => Ok(At::Ours),
        Ok(_) => Ok(At::Other),
    }
}

/// Write the skill and link it, or with `dry_run` say what that would
/// do, a line a step on `out`. [`Exit::Failed`] when something else is
/// at a link's name, which is left as it is, or a link can't be made;
/// each link is tried either way.
pub fn install(places: &Places, dry_run: bool, out: &mut impl Write) -> Result<Exit> {
    let file = places.folder.join("SKILL.md");
    let skill = spherecut_agent::SKILL.as_bytes();
    let current = std::fs::read(&file).is_ok_and(|bytes| bytes == skill);
    if current {
        writeln!(out, "{} is up to date", file.display())?;
    } else if dry_run {
        writeln!(out, "would write {}", file.display())?;
    } else {
        std::fs::create_dir_all(&places.folder)
            .with_context(|| format!("creating {}", places.folder.display()))?;
        let mut tmp =
            TempFile::create(&file).with_context(|| format!("writing {}", file.display()))?;
        tmp.file_mut().write_all(skill)?;
        tmp.persist_replace()
            .with_context(|| format!("writing {}", file.display()))?;
        writeln!(out, "wrote {}", file.display())?;
    }
    let mut exit = Exit::Ok;
    for link in &places.links {
        match place(link, &places.folder, dry_run) {
            Ok(Placed::Done(said)) => writeln!(out, "{said}")?,
            Ok(Placed::LeftAlone(said)) => {
                exit = Exit::Failed;
                writeln!(out, "{said}")?;
            }
            Err(e) => {
                exit = Exit::Failed;
                writeln!(out, "couldn't link {}: {e:#}", link.display())?;
            }
        }
    }
    Ok(exit)
}

/// What [`place`] did about a link.
enum Placed {
    Done(String),
    LeftAlone(String),
}

/// Link `link` to `folder` unless something is at its name.
fn place(link: &Path, folder: &Path, dry_run: bool) -> Result<Placed> {
    let (shown, target) = (link.display(), folder.display());
    Ok(
        match at(link, folder).with_context(|| format!("reading {shown}"))? {
            At::Ours => Placed::Done(format!("{shown} links to {target} already")),
            At::Other => Placed::LeftAlone(format!(
                "left {shown} alone: something else is there. Move it, or link it to \
                 {target} yourself"
            )),
            At::Nothing if dry_run => Placed::Done(format!("would link {shown} to {target}")),
            At::Nothing => {
                if let Some(parent) = link.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                std::os::unix::fs::symlink(folder, link)
                    .with_context(|| format!("linking {shown}"))?;
                Placed::Done(format!("linked {shown} to {target}"))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for one test, removed when dropped.
    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn dir(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("spherecut-skill-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn run(places: &Places, dry_run: bool) -> (Exit, String) {
        let mut out = Vec::new();
        let exit = install(places, dry_run, &mut out).unwrap();
        (exit, String::from_utf8(out).unwrap())
    }

    #[test]
    fn writes_links_and_is_idempotent() {
        let d = dir("fresh");
        let places = Places::new(&d.0.join("home"), &d.0.join("data"));
        // A dry run changes nothing.
        let (exit, said) = run(&places, true);
        assert_eq!(exit, Exit::Ok);
        assert_eq!(said.matches("would").count(), 3, "{said}");
        assert!(!d.0.join("data").exists() && !d.0.join("home").exists());

        let (exit, said) = run(&places, false);
        assert_eq!(exit, Exit::Ok, "{said}");
        let file = places.folder.join("SKILL.md");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            spherecut_agent::SKILL
        );
        for link in &places.links {
            assert_eq!(std::fs::read_link(link).unwrap(), places.folder);
            assert_eq!(
                std::fs::read_to_string(link.join("SKILL.md")).unwrap(),
                spherecut_agent::SKILL
            );
        }
        // Again, nothing to do.
        let (exit, said) = run(&places, false);
        assert_eq!(exit, Exit::Ok);
        assert!(
            said.contains("up to date") && said.matches("already").count() == 2,
            "{said}"
        );
        // An older skill is replaced.
        std::fs::write(&file, "old").unwrap();
        let (_, said) = run(&places, false);
        assert!(said.starts_with("wrote"), "{said}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            spherecut_agent::SKILL
        );
    }

    #[test]
    fn leaves_what_is_there_alone() {
        let d = dir("taken");
        let places = Places::new(&d.0.join("home"), &d.0.join("data"));
        let [claude, agents] = &places.links;
        // A folder of the user's own at one name, a link elsewhere at the
        // other.
        std::fs::create_dir_all(claude).unwrap();
        std::fs::write(claude.join("SKILL.md"), "mine").unwrap();
        std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&d.0, agents).unwrap();
        for dry_run in [true, false] {
            let (exit, said) = run(&places, dry_run);
            assert_eq!(exit, Exit::Failed);
            assert_eq!(said.matches("left").count(), 2, "{said}");
        }
        assert_eq!(
            std::fs::read_to_string(claude.join("SKILL.md")).unwrap(),
            "mine"
        );
        assert_eq!(std::fs::read_link(agents).unwrap(), d.0);
        // The skill itself was still written.
        assert!(places.folder.join("SKILL.md").is_file());
    }

    #[test]
    fn a_link_that_cant_be_made_doesnt_stop_the_other() {
        let d = dir("blocked");
        let places = Places::new(&d.0.join("home"), &d.0.join("data"));
        let [claude, agents] = &places.links;
        // ~/.claude is a file, so nothing can go under it.
        let dot_claude = claude.parent().unwrap().parent().unwrap();
        std::fs::create_dir_all(dot_claude.parent().unwrap()).unwrap();
        std::fs::write(dot_claude, "a file").unwrap();
        let (exit, said) = run(&places, false);
        assert_eq!(exit, Exit::Failed);
        assert!(said.contains("couldn't link"), "{said}");
        assert_eq!(std::fs::read_to_string(dot_claude).unwrap(), "a file");
        assert_eq!(std::fs::read_link(agents).unwrap(), places.folder);
    }
}
