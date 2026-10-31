//! Undo history (M2 plan section 4.3): labelled steps, each the edits as
//! applied and their inverse, with who made it and the revisions before
//! and after.

use serde::{Deserialize, Serialize};

use super::Project;
use super::edit::{Edit, EditError, Ids};

/// Who made a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    /// Pat, in the GUI or the CLI.
    User,
    /// An agent, by the name it gave (A2).
    Agent { name: String },
}

/// One undo step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// What the Edit menu says: "Undo Move key".
    pub label: String,
    pub origin: Origin,
    /// As applied, with new ids filled in: what redo applies.
    pub edits: Vec<Edit>,
    /// What undo applies.
    pub inverse: Vec<Edit>,
    /// The project's revision before and after the step was made.
    pub before: u64,
    pub after: u64,
}

/// A project's undo steps, oldest first, with a cursor: the steps below
/// it are done, the ones above it undone (redo applies them again).
/// Written as `{"steps": [...], "cursor": n, "limit": n}`, for A2's
/// saved history; reading refuses a cursor past the steps or more steps
/// than the limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "HistoryRepr")]
pub struct History {
    steps: Vec<Step>,
    cursor: usize,
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRepr {
    steps: Vec<Step>,
    cursor: usize,
    limit: usize,
}

impl TryFrom<HistoryRepr> for History {
    type Error = String;

    fn try_from(r: HistoryRepr) -> Result<History, String> {
        if r.limit == 0 || r.steps.len() > r.limit {
            return Err(format!(
                "a history of {} steps with a limit of {}",
                r.steps.len(),
                r.limit
            ));
        }
        if r.cursor > r.steps.len() {
            return Err(format!(
                "a history's cursor {} is past its {} steps",
                r.cursor,
                r.steps.len()
            ));
        }
        Ok(History {
            steps: r.steps,
            cursor: r.cursor,
            limit: r.limit,
        })
    }
}

/// How many steps a history keeps; the oldest go first.
pub const HISTORY_LIMIT: usize = 1000;

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

impl History {
    pub fn new() -> History {
        History::with_limit(HISTORY_LIMIT)
    }

    pub fn with_limit(limit: usize) -> History {
        History {
            steps: Vec::new(),
            cursor: 0,
            limit: limit.max(1),
        }
    }

    /// Apply `edits` to `project` as one step, dropping any undone steps
    /// above the cursor. On error nothing changes.
    pub fn record(
        &mut self,
        project: &mut Project,
        label: impl Into<String>,
        origin: Origin,
        edits: &[Edit],
    ) -> Result<&Step, EditError> {
        let before = project.revision();
        let applied = project.apply(edits)?;
        self.steps.truncate(self.cursor);
        self.steps.push(Step {
            label: label.into(),
            origin,
            edits: applied.edits,
            inverse: applied.inverse,
            before,
            after: project.revision(),
        });
        if self.steps.len() > self.limit {
            let extra = self.steps.len() - self.limit;
            self.steps.drain(..extra);
        }
        self.cursor = self.steps.len();
        Ok(&self.steps[self.cursor - 1])
    }

    /// Undo the step below the cursor, as a new revision. `Ok(None)` when
    /// there is nothing to undo; on error nothing changes.
    pub fn undo(&mut self, project: &mut Project) -> Result<Option<&Step>, EditError> {
        let Some(i) = self.cursor.checked_sub(1) else {
            return Ok(None);
        };
        project.apply_with(&self.steps[i].inverse, Ids::Restore)?;
        self.cursor = i;
        Ok(Some(&self.steps[i]))
    }

    /// Redo the step above the cursor, as a new revision, with the ids it
    /// had. `Ok(None)` when there is nothing to redo.
    pub fn redo(&mut self, project: &mut Project) -> Result<Option<&Step>, EditError> {
        let i = self.cursor;
        let Some(step) = self.steps.get(i) else {
            return Ok(None);
        };
        project.apply_with(&step.edits, Ids::Restore)?;
        self.cursor = i + 1;
        Ok(Some(&self.steps[i]))
    }

    /// The step undo would undo.
    pub fn undo_step(&self) -> Option<&Step> {
        self.cursor.checked_sub(1).map(|i| &self.steps[i])
    }

    /// The step redo would redo.
    pub fn redo_step(&self) -> Option<&Step> {
        self.steps.get(self.cursor)
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// End a gesture (a drag) as one step: `project` goes back to how it
    /// was when the gesture started, then `edits` (the whole change, start
    /// to end) are recorded. No edits, no step.
    pub fn finish(
        &mut self,
        gesture: Gesture,
        project: &mut Project,
        edits: &[Edit],
    ) -> Result<Option<&Step>, EditError> {
        *project = gesture.before;
        if edits.is_empty() {
            return Ok(None);
        }
        self.record(project, gesture.label, gesture.origin, edits)
            .map(Some)
    }
}

/// A change in progress, such as a drag: each frame shows the project
/// with the gesture's edits so far, and when it ends it becomes one undo
/// step ([`History::finish`]), or nothing ([`Gesture::cancel`]).
#[derive(Debug, Clone)]
pub struct Gesture {
    before: Project,
    label: String,
    origin: Origin,
}

impl Gesture {
    pub fn start(project: &Project, label: impl Into<String>, origin: Origin) -> Gesture {
        Gesture {
            before: project.clone(),
            label: label.into(),
            origin,
        }
    }

    /// Show `edits` (the whole change since the start, not since the last
    /// frame) in `project`. On error `project` is left as it was, so the
    /// last good frame stays.
    pub fn update(&self, project: &mut Project, edits: &[Edit]) -> Result<(), EditError> {
        let mut p = self.before.clone();
        if !edits.is_empty() {
            p.apply(edits)?;
        }
        *project = p;
        Ok(())
    }

    /// Put `project` back as it was when the gesture started.
    pub fn cancel(self, project: &mut Project) {
        *project = self.before;
    }

    /// The project as it was when the gesture started.
    pub fn before(&self) -> &Project {
        &self.before
    }
}
