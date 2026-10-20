//! Merging a PR, or adding it to or taking it out of the merge queue.

use super::{Loadable, PrAction, PrCommand};
use crate::app::{App, Command};
use crate::pulls::model::{MergeMethod, MergeStatus, PrDetail, QueueState};
use crossterm::event::{KeyCode, KeyEvent};

/// The box that picks how to merge.
pub struct MergeDialog {
    /// The methods the repo allows, the viewer's default first.
    pub methods: Vec<MergeMethod>,
    pub sel: usize,
    /// Whether to delete the branch after merging. `None` when the branch is
    /// in a fork, which this token can't delete from.
    pub delete_branch: Option<bool>,
}

impl MergeDialog {
    /// A dialog for a PR in the repo `repo`.
    fn new(detail: &PrDetail, repo: &str) -> Self {
        MergeDialog {
            methods: detail.merge_methods.clone(),
            sel: 0,
            delete_branch: (detail.head.repo == repo).then_some(false),
        }
    }

    fn move_sel(&mut self, delta: i32) {
        self.sel = (self.sel as i32 + delta).clamp(0, self.methods.len() as i32 - 1) as usize;
    }

    fn method(&self) -> Option<MergeMethod> {
        self.methods.get(self.sel).copied()
    }
}

impl App {
    /// `m` on an open PR: opens the merge box, or asks about the merge queue
    /// when the base branch has one.
    pub(super) fn start_merge(&mut self) {
        let Some(detail) = self.ready_detail() else { return };
        let (node_id, head_oid) = (detail.node_id.clone(), detail.head.oid.clone());
        if detail.draft {
            self.set_status("A draft can't be merged. Mark it ready for review first", true);
            return;
        }
        if detail.merge_status == MergeStatus::Conflicts {
            self.set_status("The branch has conflicts with its base. Resolve them first", true);
            return;
        }
        let queue = detail.queue;
        let label = self.prs.open.as_ref().map(|v| v.pr.label()).unwrap_or_default();
        match queue {
            QueueState::NoQueue => self.open_merge_dialog(),
            QueueState::NotQueued => {
                self.ask_pr(PrAction::Enqueue { node_id, head_oid }, format!("Add {label} to the merge queue?"));
            }
            QueueState::Queued { .. } => {
                self.ask_pr(PrAction::Dequeue { node_id }, format!("Remove {label} from the merge queue?"));
            }
        }
    }

    fn open_merge_dialog(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Loadable::Ready(detail) = &view.detail else { return };
        if detail.merge_methods.is_empty() {
            self.set_status("The repo allows no merge method", true);
            return;
        }
        view.merge_dialog = Some(MergeDialog::new(detail, &view.pr.repo));
    }

    /// Keys while the merge box is open; it takes all of them.
    pub(super) fn key_merge_dialog(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(dialog) = &mut view.merge_dialog else { return };
        match key.code {
            KeyCode::Esc => view.merge_dialog = None,
            KeyCode::Char('j') | KeyCode::Down => dialog.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => dialog.move_sel(-1),
            KeyCode::Char('d') => {
                if let Some(on) = &mut dialog.delete_branch {
                    *on = !*on;
                }
            }
            KeyCode::Enter => self.send_merge(),
            _ => {}
        }
    }

    fn send_merge(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(dialog) = view.merge_dialog.take() else { return };
        let (Some(method), Loadable::Ready(detail)) = (dialog.method(), &view.detail) else { return };
        let delete_branch = (dialog.delete_branch == Some(true)).then(|| detail.head.branch.clone());
        let action = PrAction::Merge { method, head_oid: detail.head.oid.clone(), delete_branch };
        let pr = view.pr.clone();
        self.pending.push(Command::Pr(PrCommand::Act { pr, action }));
        self.set_status("Merging…", false);
    }
}
