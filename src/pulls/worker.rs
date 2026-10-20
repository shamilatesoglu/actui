//! Runs the PR commands against GitHub on the async runtime.

use crate::app::{DataMsg, PrCommand, PrMsg};
use crate::github::Github;
use crate::pulls::model::PrGroup;
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
        PrCommand::FetchDetail(pr) => {
            let (gh, tx) = (gh.clone(), tx.clone());
            tokio::spawn(async move {
                let result = gh.pr_detail(&pr).await.map(Box::new).map_err(|e| e.to_string());
                let _ = tx.send(DataMsg::Pr(PrMsg::Detail { pr, result }));
            });
        }
    }
}
