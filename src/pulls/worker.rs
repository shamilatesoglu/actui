//! Runs the PR commands against GitHub on the async runtime.

use crate::app::{DataMsg, PrAction, PrCommand, PrMsg};
use crate::github::Github;
use crate::pulls::model::{PrGroup, PrRef};
use tokio::sync::mpsc::UnboundedSender;

pub fn run_command(cmd: PrCommand, gh: &Github, tx: &UnboundedSender<DataMsg>) {
    match cmd {
        PrCommand::FetchList => {
            for group in PrGroup::ALL {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    let result = gh.search_prs(group).await.map_err(|e| e.to_string());
                    let _ = tx.send(DataMsg::Pr(PrMsg::List { group, result }));
                });
            }
        }
        PrCommand::FetchRun { repo, run_id } => {
            let (gh, tx) = (gh.clone(), tx.clone());
            tokio::spawn(async move {
                let result = gh.workflow_run(&repo, run_id).await.map(Box::new).map_err(|e| e.to_string());
                let _ = tx.send(DataMsg::Pr(PrMsg::Run(result)));
            });
        }
        PrCommand::FetchFiles(pr) => {
            let (gh, tx) = (gh.clone(), tx.clone());
            tokio::spawn(async move {
                let result = gh.pr_files(&pr).await.map_err(|e| e.to_string());
                let _ = tx.send(DataMsg::Pr(PrMsg::Files { pr, result }));
            });
        }
        PrCommand::Act { pr, action } => {
            let (gh, tx) = (gh.clone(), tx.clone());
            tokio::spawn(async move {
                let kind = action.kind();
                let result = act(&gh, &pr, action).await.map_err(|e| e.to_string());
                let _ = tx.send(DataMsg::Pr(PrMsg::ActDone { pr, kind, result }));
            });
        }
        PrCommand::FetchDetail(pr) => {
            let (gh, tx) = (gh.clone(), tx.clone());
            tokio::spawn(async move {
                let result = gh.pr_detail(&pr).await.map(Box::new).map_err(|e| e.to_string());
                let _ = tx.send(DataMsg::Pr(PrMsg::Detail { pr, result }));
            });
        }
    }
}

async fn act(gh: &Github, pr: &PrRef, action: PrAction) -> anyhow::Result<()> {
    match action {
        PrAction::Comment { body } => gh.comment_on_pr(pr, &body).await,
        PrAction::SetDraft { node_id, draft } => gh.set_draft(&node_id, draft).await,
        PrAction::UpdateBranch { head_oid } => gh.update_branch(pr, &head_oid).await,
        PrAction::Merge { method, head_oid, delete_branch } => {
            gh.merge_pr(pr, method, &head_oid).await?;
            // The merge is done by now, so a failure here says so.
            if let Some(branch) = delete_branch {
                gh.delete_branch(&pr.repo, &branch)
                    .await
                    .map_err(|e| anyhow::anyhow!("Merged, but deleting the branch failed: {e}"))?;
            }
            Ok(())
        }
        PrAction::Enqueue { node_id, head_oid } => gh.enqueue_pr(&node_id, &head_oid).await,
        PrAction::Dequeue { node_id } => gh.dequeue_pr(&node_id).await,
        PrAction::LineComment { head_oid, comment } => gh.post_line_comment(pr, &head_oid, &comment).await,
        PrAction::Reply { comment_id, body } => gh.reply_to_thread(pr, comment_id, &body).await,
        PrAction::SubmitReview { node_id, submission } => gh.submit_review(&node_id, &submission).await,
        PrAction::ResolveThread { thread_id, resolved } => gh.resolve_thread(&thread_id, resolved).await,
        PrAction::RerunFailed { run_ids } => {
            // Every run gets its turn; the first failure is the one reported.
            let mut first_error = None;
            for run_id in run_ids {
                if let Err(e) = gh.rerun_failed(&pr.repo, run_id).await {
                    first_error.get_or_insert(e);
                }
            }
            first_error.map_or(Ok(()), Err)
        }
    }
}
