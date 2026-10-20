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
    }
}
