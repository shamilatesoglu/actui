//! State for the modal overlays: the confirm prompt, the artifacts browser, the
//! deployment-review picker, the branch/tag picker, and the dispatch form. These
//! are plain data plus view-local logic; none reference `App`. The few methods
//! the `App` reducer / input handlers call are `pub(crate)`.

use crate::github::{Annotation, Artifact, PendingDeployment, WfInput, WfInputKind, Workflow};
use ratatui::widgets::ListState;
use std::collections::HashSet;

use super::fuzzy;

pub enum PendingAction {
    Cancel { repo: String, run_id: u64, label: String },
    Rerun { repo: String, run_id: u64, label: String },
    RerunFailed { repo: String, run_id: u64, label: String },
    RerunJob { repo: String, job_id: u64, label: String },
    Approve { repo: String, run_id: u64, label: String },
}

impl PendingAction {
    pub fn prompt(&self) -> String {
        match self {
            PendingAction::Cancel { label, .. } => format!("Cancel run?  {label}"),
            PendingAction::Rerun { label, .. } => format!("Re-run all jobs?  {label}"),
            PendingAction::RerunFailed { label, .. } => format!("Re-run failed jobs?  {label}"),
            PendingAction::RerunJob { label, .. } => format!("Re-run job?  {label}"),
            PendingAction::Approve { label, .. } => format!("Approve run?  {label}"),
        }
    }
}

/// Browser over a run's artifacts (open with `A`).
pub struct ArtifactsView {
    pub repo: String,
    pub run_id: u64,
    pub items: Vec<Artifact>,
    pub state: ListState,
    pub loaded: bool,
}

/// Review picker for environment deployments gating a run (open with `a`).
pub struct ApprovalView {
    pub repo: String,
    pub run_id: u64,
    pub items: Vec<PendingDeployment>,
    /// Indices (into `items`) the user has marked to act on.
    pub selected: HashSet<usize>,
    pub state: ListState,
    pub loaded: bool,
    /// Optional review comment, and whether we're currently editing it.
    pub comment: String,
    pub editing_comment: bool,
}

impl ApprovalView {
    /// Whether the highlighted environment can be approved by this user.
    pub(crate) fn can_approve(&self, idx: usize) -> bool {
        self.items.get(idx).is_some_and(|p| p.current_user_can_approve)
    }
    /// Environment ids the user selected and is allowed to act on.
    pub fn chosen_ids(&self) -> Vec<u64> {
        self.selected
            .iter()
            .filter(|&&i| self.can_approve(i))
            .filter_map(|&i| self.items.get(i).map(|p| p.environment.id))
            .collect()
    }
}

/// Severity of a check-run annotation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnnLevel {
    Failure,
    Warning,
    Notice,
}

impl AnnLevel {
    fn from_api(level: Option<&str>) -> Self {
        match level {
            Some("failure") => AnnLevel::Failure,
            Some("notice") => AnnLevel::Notice,
            // GitHub's default bucket (and our fallback for an absent level).
            _ => AnnLevel::Warning,
        }
    }
    /// Triage order: failures first, then warnings, then notices.
    pub fn rank(self) -> u8 {
        match self {
            AnnLevel::Failure => 0,
            AnnLevel::Warning => 1,
            AnnLevel::Notice => 2,
        }
    }
}

/// One annotation flattened for display: which job it came from, where, and what.
pub struct AnnotationItem {
    pub job_id: u64,
    pub job_name: String,
    pub level: AnnLevel,
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    /// The producing tool/check label, when GitHub provides one (e.g. "rustc").
    pub title: Option<String>,
    pub message: String,
}

impl AnnotationItem {
    pub fn new(job_id: u64, job_name: &str, a: &Annotation) -> Self {
        Self {
            job_id,
            job_name: job_name.to_string(),
            level: AnnLevel::from_api(a.annotation_level.as_deref()),
            path: a.path.clone(),
            start_line: a.start_line,
            end_line: a.end_line.max(a.start_line),
            title: a.title.clone().filter(|t| !t.is_empty()),
            message: a.message.clone(),
        }
    }
    /// `path:line`, or `path:start-end` when the annotation spans a line range.
    pub fn location(&self) -> String {
        if self.end_line > self.start_line {
            format!("{}:{}-{}", self.path, self.start_line, self.end_line)
        } else {
            format!("{}:{}", self.path, self.start_line)
        }
    }
    /// First line of the (possibly multi-line) message — for the compact row and
    /// as the query when jumping into the logs. Falls back to the title.
    pub fn summary(&self) -> &str {
        let first = self.message.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            self.title.as_deref().unwrap_or("")
        } else {
            first
        }
    }
}

/// Failure-annotations browser for a run (open with `v`).
pub struct AnnotationsView {
    pub repo: String,
    pub run_id: u64,
    pub items: Vec<AnnotationItem>,
    pub state: ListState,
    pub loaded: bool,
}

impl AnnotationsView {
    pub fn selected(&self) -> Option<&AnnotationItem> {
        self.items.get(self.state.selected()?)
    }
    /// Annotation counts by severity, for the status bar.
    pub fn counts(&self) -> (usize, usize, usize) {
        let (mut f, mut w, mut n) = (0, 0, 0);
        for it in &self.items {
            match it.level {
                AnnLevel::Failure => f += 1,
                AnnLevel::Warning => w += 1,
                AnnLevel::Notice => n += 1,
            }
        }
        (f, w, n)
    }
    /// Whether the annotations span more than one job (drives showing job tags).
    pub fn multi_job(&self) -> bool {
        self.items
            .iter()
            .map(|i| i.job_id)
            .collect::<HashSet<_>>()
            .len()
            > 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    Tag,
}

pub struct RefItem {
    pub name: String,
    pub kind: RefKind,
}

/// Branch/tag picker for the dispatch ref field (open with Space / → on it).
pub struct RefPicker {
    pub repo: String,
    pub items: Vec<RefItem>,
    /// Indices into `items` after the filter is applied.
    pub view: Vec<usize>,
    pub state: ListState,
    pub filter: String,
    pub loaded: bool,
}

impl RefPicker {
    pub(crate) fn recompute(&mut self) {
        let q = self.filter.to_lowercase();
        self.view = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, r)| q.is_empty() || fuzzy(&r.name, &q))
            .map(|(i, _)| i)
            .collect();
        let sel = if self.view.is_empty() { None } else { Some(0) };
        self.state.select(sel);
    }
    pub fn selected_ref(&self) -> Option<&RefItem> {
        let i = self.state.selected()?;
        self.items.get(*self.view.get(i)?)
    }
}

pub enum DispatchStage {
    SelectWorkflow,
    EditParams,
}

/// A single field in the dispatch form, with its live value.
pub enum FieldKind {
    Text { value: String, numeric: bool },
    Bool(bool),
    Choice { options: Vec<String>, idx: usize },
}

pub struct DispatchField {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub kind: FieldKind,
}

impl DispatchField {
    pub(crate) fn from_input(i: WfInput) -> Self {
        let kind = match i.kind {
            WfInputKind::Boolean => FieldKind::Bool(i.default == "true"),
            WfInputKind::Choice(options) => {
                let idx = options.iter().position(|o| *o == i.default).unwrap_or(0);
                FieldKind::Choice { options, idx }
            }
            WfInputKind::Number => FieldKind::Text { value: i.default, numeric: true },
            WfInputKind::Text => FieldKind::Text { value: i.default, numeric: false },
        };
        Self {
            name: i.name,
            description: i.description,
            required: i.required,
            kind,
        }
    }

    /// The value to submit for this field.
    pub(crate) fn value(&self) -> String {
        match &self.kind {
            FieldKind::Text { value, .. } => value.clone(),
            FieldKind::Bool(b) => b.to_string(),
            FieldKind::Choice { options, idx } => options.get(*idx).cloned().unwrap_or_default(),
        }
    }
}

pub struct DispatchState {
    pub repo: String,
    pub workflows: Vec<Workflow>,
    pub wf_state: ListState,
    pub stage: DispatchStage,
    pub git_ref: String,
    /// The ref the current inputs were fetched at (to detect a changed ref).
    pub fetched_ref: String,
    /// Selected workflow (set when entering the form stage).
    pub workflow_id: u64,
    pub workflow_path: String,
    pub fields: Vec<DispatchField>,
    pub loaded: bool,      // inputs fetched
    pub dispatchable: bool, // has a workflow_dispatch trigger
    pub field_idx: usize,  // 0 = ref, 1..=fields.len() = inputs
}

impl DispatchState {
    pub(crate) fn field_count(&self) -> usize {
        self.fields.len() + 1 // + the ref field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_picker_filters_and_selects() {
        let mut rp = RefPicker {
            repo: "o/r".into(),
            items: vec![
                RefItem { name: "main".into(), kind: RefKind::Branch },
                RefItem { name: "release/1.0".into(), kind: RefKind::Branch },
                RefItem { name: "v1.0.0".into(), kind: RefKind::Tag },
            ],
            view: Vec::new(),
            state: ListState::default(),
            filter: String::new(),
            loaded: true,
        };
        rp.recompute();
        assert_eq!(rp.view.len(), 3);
        assert_eq!(rp.selected_ref().unwrap().name, "main");
        rp.filter = "rel".into();
        rp.recompute();
        assert_eq!(rp.view.len(), 1);
        assert_eq!(rp.selected_ref().unwrap().name, "release/1.0");
    }

    #[test]
    fn annotation_item_classifies_level_and_first_line() {
        let ann = |level: Option<&str>| Annotation {
            path: "src/main.rs".into(),
            start_line: 42,
            end_line: 44,
            annotation_level: level.map(String::from),
            title: Some("rustc".into()),
            message: "error[E0382]: borrow of moved value\n  extra detail line".into(),
        };
        let fail = AnnotationItem::new(7, "build", &ann(Some("failure")));
        assert!(matches!(fail.level, AnnLevel::Failure));
        assert_eq!(fail.start_line, 42);
        // A multi-line span renders as a range; the tool label is kept.
        assert_eq!(fail.location(), "src/main.rs:42-44");
        assert_eq!(fail.title.as_deref(), Some("rustc"));
        // The row/search use only the first line, trimmed.
        assert_eq!(fail.summary(), "error[E0382]: borrow of moved value");
        // An absent level buckets as a warning (GitHub's default).
        assert!(matches!(AnnotationItem::new(7, "x", &ann(None)).level, AnnLevel::Warning));
        // Triage order: failures sort ahead of warnings ahead of notices.
        assert!(AnnLevel::Failure.rank() < AnnLevel::Warning.rank());
        assert!(AnnLevel::Warning.rank() < AnnLevel::Notice.rank());
    }

    #[test]
    fn approval_chosen_ids_skip_unapprovable() {
        let env = |id, name: &str, can| PendingDeployment {
            environment: crate::github::EnvRef { id, name: name.into() },
            current_user_can_approve: can,
        };
        let mut av = ApprovalView {
            repo: "o/r".into(),
            run_id: 1,
            items: vec![env(10, "staging", true), env(20, "prod", false)],
            selected: HashSet::new(),
            state: ListState::default(),
            loaded: true,
            comment: String::new(),
            editing_comment: false,
        };
        av.selected.insert(0);
        av.selected.insert(1); // prod, but user can't approve it
        assert_eq!(av.chosen_ids(), vec![10]);
    }
}
