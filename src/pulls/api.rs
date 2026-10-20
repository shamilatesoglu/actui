//! Pull request reads over GitHub's GraphQL API.

use crate::github::{ensure_ok, Github};
use crate::pulls::model::*;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

/// Returns the `data` of a GraphQL response. Errors next to data are partial
/// failures (a SAML-protected org in a search, say) and the data is still
/// worth showing. Without data, the first error's message is returned.
fn parse_graphql<T: DeserializeOwned>(body: &str) -> Result<T> {
    let resp: GraphqlResponse<T> = serde_json::from_str(body).context("bad GraphQL response")?;
    match (resp.data, resp.errors.into_iter().next()) {
        (Some(data), _) => Ok(data),
        (None, Some(first)) => Err(anyhow!(first.message)),
        (None, None) => Err(anyhow!("GraphQL response has no data")),
    }
}

impl Github {
    pub(crate) async fn graphql<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let req = self
            .client
            .post(GRAPHQL_URL)
            .json(&serde_json::json!({ "query": query, "variables": variables }));
        let body = ensure_ok(self.send(req).await?).await?.text().await?;
        parse_graphql(&body)
    }

    /// Lists the open pull requests of one group, most recently updated first.
    pub async fn search_prs(&self, group: PrGroup) -> Result<Vec<PrSummary>> {
        let data = self
            .graphql(SEARCH_QUERY, serde_json::json!({ "q": group.query() }))
            .await?;
        parse_search(data)
    }
}

const SEARCH_QUERY: &str = "query($q: String!) { search(query: $q, type: ISSUE, first: 50) { nodes {     ... on PullRequest { id number title url isDraft updatedAt reviewDecision     author { login } repository { nameWithOwner }     commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } } } } }";

#[derive(Deserialize)]
struct SearchData {
    search: SearchNodes,
}

#[derive(Deserialize)]
struct SearchNodes {
    nodes: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    id: String,
    number: u64,
    title: String,
    is_draft: bool,
    updated_at: DateTime<Utc>,
    review_decision: Option<String>,
    /// `null` when the author's account was deleted.
    author: Option<Login>,
    repository: RepoName,
    commits: CommitNodes,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoName {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct CommitNodes {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: CommitChecks,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitChecks {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
}

fn checks_state(rollup: Option<&Rollup>) -> ChecksState {
    match rollup.map(|r| r.state.as_str()) {
        None => ChecksState::NoChecks,
        Some("SUCCESS") => ChecksState::Passing,
        Some("FAILURE" | "ERROR") => ChecksState::Failing,
        Some(_) => ChecksState::Pending,
    }
}

fn review_decision(decision: Option<&str>) -> ReviewDecision {
    match decision {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::NotRequired,
    }
}

impl From<PrNode> for PrSummary {
    fn from(node: PrNode) -> Self {
        let rollup = node
            .commits
            .nodes
            .first()
            .and_then(|c| c.commit.status_check_rollup.as_ref());
        PrSummary {
            pr: PrRef { repo: node.repository.name_with_owner, number: node.number },
            node_id: node.id,
            title: node.title,
            author: node.author.map_or_else(|| "ghost".into(), |a| a.login),
            updated_at: node.updated_at,
            draft: node.is_draft,
            checks: checks_state(rollup),
            review: review_decision(node.review_decision.as_deref()),
        }
    }
}

/// Turns search results into summaries. Nodes that aren't pull requests
/// come back as `{}` and are skipped. Any other node that doesn't parse is
/// an error, so a pull request never drops out of the list silently.
fn parse_search(data: serde_json::Value) -> Result<Vec<PrSummary>> {
    let data: SearchData = serde_json::from_value(data).context("bad search response")?;
    let mut prs = Vec::new();
    for node in data.search.nodes {
        if node.as_object().is_some_and(|o| o.is_empty()) {
            continue;
        }
        let node: PrNode = serde_json::from_value(node)
            .map_err(|e| anyhow!("bad pull request in search: {e}"))?;
        prs.push(PrSummary::from(node));
    }
    Ok(prs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_errors_become_the_error_message() {
        let body = r#"{"data":null,"errors":[{"message":"Field 'nope' doesn't exist"}]}"#;
        let err = parse_graphql::<serde_json::Value>(body).unwrap_err();
        assert_eq!(err.to_string(), "Field 'nope' doesn't exist");
    }

    #[test]
    fn data_next_to_errors_is_returned() {
        let body = r#"{"data":{"n":1},"errors":[{"message":"SAML enforcement"}]}"#;
        let data = parse_graphql::<serde_json::Value>(body).unwrap();
        assert_eq!(data["n"], 1);
    }

    #[test]
    fn a_node_that_fails_to_parse_is_an_error() {
        let data = serde_json::json!({"search":{"nodes":[{"id":"PR_1","number":"twelve"}]}});
        let err = parse_search(data).unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("bad pull request in search: "), "{msg}");
        assert!(msg.contains("twelve"), "says what was wrong: {msg}");
    }

    #[test]
    fn search_results_become_summaries() {
        let data = serde_json::json!({"search":{"nodes":[
            {"id":"PR_1","number":12,"title":"Fix login","url":"https://github.com/org/api/pull/12",
             "isDraft":true,"updatedAt":"2026-10-20T09:00:00Z","reviewDecision":"CHANGES_REQUESTED",
             "author":{"login":"ana"},"repository":{"nameWithOwner":"org/api"},
             "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}},
            {"id":"PR_2","number":3,"title":"Docs","url":"https://github.com/me/tool/pull/3",
             "isDraft":false,"updatedAt":"2026-10-19T09:00:00Z","reviewDecision":null,
             "author":null,"repository":{"nameWithOwner":"me/tool"},
             "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}},
            {}
        ]}});
        let prs = parse_search(data).unwrap();
        assert_eq!(prs.len(), 2, "non-PR nodes are skipped");
        assert_eq!(prs[0].pr, PrRef { repo: "org/api".into(), number: 12 });
        assert!(prs[0].draft);
        assert_eq!(prs[0].checks, ChecksState::Failing);
        assert_eq!(prs[0].review, ReviewDecision::ChangesRequested);
        assert_eq!(prs[1].author, "ghost");
        assert_eq!(prs[1].checks, ChecksState::NoChecks);
        assert_eq!(prs[1].review, ReviewDecision::NotRequired);
    }
}
