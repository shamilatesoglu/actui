//! Pull request reads over GitHub's GraphQL API.

use crate::github::{ensure_ok, Cond, Github};
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

    /// Reads everything the PR view shows about one pull request.
    pub async fn pr_detail(&self, pr: &PrRef) -> Result<PrDetail> {
        let (owner, name) = pr.repo.split_once('/').context("repo is not owner/name")?;
        let data = self
            .graphql(
                DETAIL_QUERY,
                serde_json::json!({ "owner": owner, "name": name, "number": pr.number }),
            )
            .await?;
        parse_detail(data)
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

const DETAIL_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { nameWithOwner mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod pullRequest(number: $number) { id title body url isDraft baseRefName headRefName headRefOid mergeStateStatus isMergeQueueEnabled mergeQueueEntry { position } author { login } headRepository { nameWithOwner } labels(first: 30) { nodes { name } } reviewRequests(first: 30) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { slug } } } } latestOpinionatedReviews(first: 50) { nodes { author { login } state } } reviews(last: 100) { nodes { author { login } state body submittedAt } } comments(last: 100) { nodes { databaseId author { login } body createdAt } } reviewThreads(first: 100) { nodes { id isResolved isOutdated path line startLine diffSide comments(first: 50) { nodes { databaseId author { login } body createdAt } } } } commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { __typename ... on CheckRun { name status conclusion detailsUrl checkSuite { workflowRun { databaseId } } } ... on StatusContext { context state targetUrl } } } } } } } } } }";

/// A GraphQL connection. Its nodes can be `null` (an item the viewer may not
/// see), and those are left out.
#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<Option<T>>,
}

impl<T> Nodes<T> {
    fn items(self) -> impl Iterator<Item = T> {
        self.nodes.into_iter().flatten()
    }
}

#[derive(Deserialize)]
struct DetailData {
    repository: RepoNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoNode {
    name_with_owner: String,
    merge_commit_allowed: bool,
    squash_merge_allowed: bool,
    rebase_merge_allowed: bool,
    viewer_default_merge_method: String,
    pull_request: DetailNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailNode {
    id: String,
    title: String,
    body: String,
    url: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
    merge_state_status: String,
    is_merge_queue_enabled: bool,
    merge_queue_entry: Option<QueueEntry>,
    author: Option<Login>,
    /// `null` once the fork is deleted.
    head_repository: Option<RepoName>,
    labels: Nodes<LabelNode>,
    review_requests: Nodes<ReviewRequestNode>,
    latest_opinionated_reviews: Nodes<LatestReviewNode>,
    reviews: Nodes<ReviewNode>,
    comments: Nodes<CommentNode>,
    review_threads: Nodes<ThreadNode>,
    commits: Nodes<DetailCommit>,
}

#[derive(Deserialize)]
struct QueueEntry {
    position: u32,
}

#[derive(Deserialize)]
struct LabelNode {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequestNode {
    /// `null` for a reviewer kind this doesn't read (a mannequin).
    requested_reviewer: Option<Requested>,
}

/// A user has a `login`, a team a `slug`.
#[derive(Deserialize)]
struct Requested {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
struct LatestReviewNode {
    author: Option<Login>,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewNode {
    author: Option<Login>,
    state: String,
    body: String,
    /// `null` while the review is still a draft.
    submitted_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentNode {
    database_id: u64,
    author: Option<Login>,
    body: String,
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    start_line: Option<u32>,
    diff_side: String,
    comments: Nodes<CommentNode>,
}

#[derive(Deserialize)]
struct DetailCommit {
    commit: DetailCommitChecks,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailCommitChecks {
    status_check_rollup: Option<ContextRollup>,
}

#[derive(Deserialize)]
struct ContextRollup {
    contexts: Nodes<ContextNode>,
}

/// A check run or a commit status; the fields of the other kind are absent.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextNode {
    #[serde(rename = "__typename")]
    kind: String,
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
    check_suite: Option<CheckSuite>,
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckSuite {
    workflow_run: Option<WorkflowRun>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRun {
    database_id: u64,
}

fn login(author: Option<Login>) -> String {
    author.map_or_else(|| "ghost".into(), |a| a.login)
}

fn verdict(state: &str) -> Verdict {
    match state {
        "APPROVED" => Verdict::Approved,
        "CHANGES_REQUESTED" => Verdict::ChangesRequested,
        "DISMISSED" => Verdict::Dismissed,
        _ => Verdict::Commented,
    }
}

fn comment(node: CommentNode) -> Comment {
    Comment {
        id: node.database_id,
        author: login(node.author),
        body: node.body,
        created_at: node.created_at,
    }
}

fn check(node: ContextNode) -> Check {
    let link = |url: Option<String>| {
        url.filter(|u| !u.is_empty()).map_or(CheckTarget::Nothing, CheckTarget::Link)
    };
    if node.kind == "CheckRun" {
        let state = match (node.status.as_deref(), node.conclusion.as_deref()) {
            (Some("COMPLETED"), Some("SUCCESS")) => CheckState::Passed,
            (Some("COMPLETED"), Some("SKIPPED")) => CheckState::Skipped,
            (Some("COMPLETED"), Some("NEUTRAL" | "STALE")) => CheckState::Neutral,
            (Some("COMPLETED"), _) => CheckState::Failed,
            _ => CheckState::Pending,
        };
        let run = node.check_suite.and_then(|s| s.workflow_run);
        let target = match run {
            Some(run) => CheckTarget::ActionsRun { run_id: run.database_id },
            None => link(node.details_url),
        };
        Check { name: node.name.unwrap_or_default(), state, target }
    } else {
        let state = match node.state.as_deref() {
            Some("SUCCESS") => CheckState::Passed,
            Some("FAILURE" | "ERROR") => CheckState::Failed,
            _ => CheckState::Pending,
        };
        Check { name: node.context.unwrap_or_default(), state, target: link(node.target_url) }
    }
}

/// The merge methods the repo allows, the viewer's default first.
fn merge_methods(repo: &RepoNode) -> Vec<MergeMethod> {
    let default = match repo.viewer_default_merge_method.as_str() {
        "SQUASH" => MergeMethod::Squash,
        "REBASE" => MergeMethod::Rebase,
        _ => MergeMethod::Merge,
    };
    let allowed = [
        (MergeMethod::Merge, repo.merge_commit_allowed),
        (MergeMethod::Squash, repo.squash_merge_allowed),
        (MergeMethod::Rebase, repo.rebase_merge_allowed),
    ];
    let mut methods: Vec<MergeMethod> =
        allowed.iter().filter(|(_, on)| *on).map(|(m, _)| *m).collect();
    if let Some(at) = methods.iter().position(|m| *m == default) {
        methods.remove(at);
        methods.insert(0, default);
    }
    methods
}

/// Turns the detail query's `data` into a `PrDetail`.
pub(crate) fn parse_detail(data: serde_json::Value) -> Result<PrDetail> {
    let data: DetailData = serde_json::from_value(data).context("bad pull request response")?;
    let repo = data.repository;
    let merge_methods = merge_methods(&repo);
    let pr = repo.pull_request;

    // A reviewer asked again is waiting on their review, not on their old
    // verdict, so the request wins.
    let requested: Vec<String> = pr
        .review_requests
        .items()
        .filter_map(|r| r.requested_reviewer)
        .filter_map(|r| r.login.or(r.slug))
        .collect();
    let mut reviewers: Vec<Reviewer> = pr
        .latest_opinionated_reviews
        .items()
        .map(|r| Reviewer { login: login(r.author), verdict: verdict(&r.state) })
        .filter(|r| !requested.contains(&r.login))
        .collect();
    reviewers
        .extend(requested.into_iter().map(|login| Reviewer { login, verdict: Verdict::Requested }));

    let checks = pr
        .commits
        .items()
        .next()
        .and_then(|c| c.commit.status_check_rollup)
        .map(|r| r.contexts.items().map(check).collect())
        .unwrap_or_default();

    let reviews = pr
        .reviews
        .items()
        .filter_map(|r| {
            Some(Review {
                author: login(r.author),
                verdict: verdict(&r.state),
                body: r.body,
                // A review without a time is its author's unsent draft.
                submitted_at: r.submitted_at?,
            })
        })
        .collect();

    let threads = pr
        .review_threads
        .items()
        .map(|t| ReviewThread {
            id: t.id,
            resolved: t.is_resolved,
            outdated: t.is_outdated,
            path: t.path,
            line: t.line,
            start_line: t.start_line,
            side: if t.diff_side == "LEFT" { Side::Left } else { Side::Right },
            comments: t.comments.items().map(comment).collect(),
        })
        .collect();

    let queue = match (pr.is_merge_queue_enabled, pr.merge_queue_entry) {
        (false, _) => QueueState::NoQueue,
        (true, None) => QueueState::NotQueued,
        (true, Some(entry)) => QueueState::Queued { position: entry.position },
    };

    Ok(PrDetail {
        node_id: pr.id,
        title: pr.title,
        body: pr.body,
        author: login(pr.author),
        url: pr.url,
        head: HeadRef {
            branch: pr.head_ref_name,
            // A deleted fork has no repository; the branch is shown as the PR's own.
            repo: pr.head_repository.map_or(repo.name_with_owner, |r| r.name_with_owner),
            oid: pr.head_ref_oid,
        },
        base_branch: pr.base_ref_name,
        draft: pr.is_draft,
        labels: pr.labels.items().map(|l| l.name).collect(),
        merge_status: MergeStatus::from_api(&pr.merge_state_status),
        reviewers,
        checks,
        comments: pr.comments.items().map(comment).collect(),
        reviews,
        threads,
        merge_methods,
        queue,
    })
}

const REST_URL: &str = "https://api.github.com";

/// Files per page of the changed-files list, which is the most GitHub gives.
const FILES_PER_PAGE: usize = 100;
/// GitHub lists at most 3000 files, so 30 pages is all there is.
const FILES_MAX_PAGES: u32 = 30;

#[derive(Deserialize)]
struct RawFile {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    additions: u32,
    deletions: u32,
    patch: Option<String>,
}

impl From<RawFile> for PrFile {
    fn from(f: RawFile) -> Self {
        PrFile {
            path: f.filename,
            previous_path: f.previous_filename,
            status: FileStatus::parse(&f.status),
            additions: f.additions,
            deletions: f.deletions,
            patch: f.patch,
        }
    }
}

impl Github {
    /// The files a PR changes, with their diffs. The first page is conditional,
    /// so reading an unchanged PR again costs no rate limit.
    pub async fn pr_files(&self, pr: &PrRef) -> Result<Vec<PrFile>> {
        let url = format!("{REST_URL}/repos/{}/pulls/{}/files", pr.repo, pr.number);
        let query = |page: u32| vec![("per_page", FILES_PER_PAGE.to_string()), ("page", page.to_string())];
        let key = format!("prfiles:{}", pr.label());
        let first = match self.cond_get::<serde_json::Value>(&key, url.clone(), &query(1)).await? {
            Cond::Modified(page) => {
                self.pr_files_cache.lock().unwrap().insert(key.clone(), page.clone());
                page
            }
            Cond::NotModified => self.pr_files_cache.lock().unwrap().get(&key).cloned().unwrap_or_default(),
        };
        let first: Vec<RawFile> = serde_json::from_value(first).context("bad files response")?;
        let mut files: Vec<PrFile> = first.into_iter().map(PrFile::from).collect();
        let mut page = 1;
        let mut last_len = files.len();
        while last_len == FILES_PER_PAGE && page < FILES_MAX_PAGES {
            page += 1;
            let req = self.client.get(&url).query(&query(page));
            let raw: Vec<RawFile> = ensure_ok(self.send(req).await?).await?.json().await?;
            last_len = raw.len();
            files.extend(raw.into_iter().map(PrFile::from));
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_a_patch_parses_with_none() {
        let raw: Vec<RawFile> = serde_json::from_str(
            r#"[{"filename":"a.png","status":"added","additions":0,"deletions":0},
                {"filename":"b.rs","previous_filename":"c.rs","status":"renamed","additions":1,"deletions":2,
                 "patch":"@@ -1 +1 @@\n-x\n+y"}]"#,
        )
        .unwrap();
        let files: Vec<PrFile> = raw.into_iter().map(PrFile::from).collect();
        assert!(files[0].patch.is_none());
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[1].previous_path.as_deref(), Some("c.rs"));
        assert_eq!(files[1].status, FileStatus::Renamed);
    }

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

    fn detail_fixture() -> serde_json::Value {
        serde_json::from_str(
            r#"{"repository":{
            "nameWithOwner":"org/api","mergeCommitAllowed":true,"squashMergeAllowed":true,"rebaseMergeAllowed":false,
            "viewerDefaultMergeMethod":"SQUASH",
            "pullRequest":{
                "id":"PR_kwDOG0NboM8AAAABJglhyg","title":"Add apps","body":"Body\r\ntext",
                "url":"https://github.com/org/api/pull/7","isDraft":false,
                "baseRefName":"release","headRefName":"main","headRefOid":"bac990c",
                "mergeStateStatus":"BLOCKED","isMergeQueueEnabled":true,
                "mergeQueueEntry":{"position":2},
                "author":{"login":"ana"},
                "headRepository":{"nameWithOwner":"ana/api"},
                "labels":{"nodes":[{"name":"bug"}]},
                "reviewRequests":{"nodes":[
                    {"requestedReviewer":{"__typename":"Team","slug":"core"}},
                    {"requestedReviewer":{"__typename":"User","login":"cy"}},
                    {"requestedReviewer":null}]},
                "latestOpinionatedReviews":{"nodes":[
                    {"author":{"login":"bob"},"state":"CHANGES_REQUESTED"},
                    {"author":{"login":"cy"},"state":"APPROVED"},
                    {"author":{"login":"eve"},"state":"APPROVED"}]},
                "reviews":{"nodes":[
                    {"author":{"login":"bob"},"state":"CHANGES_REQUESTED","body":"No.",
                     "submittedAt":"2026-10-20T08:00:00Z"},
                    {"author":null,"state":"PENDING","body":"","submittedAt":null}]},
                "comments":{"nodes":[
                    {"databaseId":41,"author":{"login":"ana"},"body":"ping",
                     "createdAt":"2026-10-20T09:00:00Z"}]},
                "reviewThreads":{"nodes":[{
                    "id":"PRRT_1","isResolved":false,"isOutdated":true,
                    "path":"src/lib.rs","line":null,"startLine":null,"diffSide":"RIGHT",
                    "comments":{"nodes":[
                        {"databaseId":42,"author":{"login":"bob"},"body":"why?",
                         "createdAt":"2026-10-20T08:30:00Z"}]}}]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                    {"__typename":"CheckRun","name":"Tests","status":"COMPLETED",
                     "conclusion":"FAILURE","detailsUrl":"https://github.com/org/api/actions/runs/555/job/9",
                     "checkSuite":{"workflowRun":{"databaseId":555}}},
                    {"__typename":"CheckRun","name":"External","status":"IN_PROGRESS",
                     "conclusion":null,"detailsUrl":"https://ci.example/1","checkSuite":{"workflowRun":null}},
                    {"__typename":"StatusContext","context":"legacy/ci","state":"SUCCESS",
                     "targetUrl":"https://ci.example/2"}]}}}}]}
            }}}"#,
        )
        .unwrap()
    }

    #[test]
    fn detail_parses_reviewers_checks_threads_and_queue() {
        let d = parse_detail(detail_fixture()).unwrap();
        assert_eq!(d.merge_status, MergeStatus::Blocked);
        assert_eq!(blocked_reason(&d).as_deref(), Some("changes requested"));
        assert_eq!(d.head.repo, "ana/api");
        assert_eq!(d.base_branch, "release");
        assert_eq!(d.labels, ["bug"]);
        assert_eq!(
            d.checks.iter().map(|c| (c.name.as_str(), c.state, c.target.clone())).collect::<Vec<_>>(),
            [
                ("Tests", CheckState::Failed, CheckTarget::ActionsRun { run_id: 555 }),
                ("External", CheckState::Pending, CheckTarget::Link("https://ci.example/1".into())),
                ("legacy/ci", CheckState::Passed, CheckTarget::Link("https://ci.example/2".into())),
            ]
        );
        let reviewers: Vec<_> = d.reviewers.iter().map(|r| (r.login.as_str(), r.verdict)).collect();
        assert_eq!(
            reviewers,
            [
                ("bob", Verdict::ChangesRequested),
                ("eve", Verdict::Approved),
                ("core", Verdict::Requested),
                ("cy", Verdict::Requested),
            ],
            "a team shows as its slug, and a new request replaces an old verdict"
        );
        assert_eq!(d.threads.len(), 1);
        assert!(d.threads[0].line.is_none() && d.threads[0].outdated);
        assert_eq!(d.threads[0].comments[0].id, 42);
        assert_eq!(d.comments[0].id, 41);
        assert_eq!(d.reviews.len(), 1, "a pending review is its author's draft");
        assert_eq!(d.merge_methods, [MergeMethod::Squash, MergeMethod::Merge]);
        assert_eq!(d.queue, QueueState::Queued { position: 2 });
    }

    #[test]
    fn a_ghost_author_and_a_deleted_fork_still_parse() {
        let mut data = detail_fixture();
        let pr = &mut data["repository"]["pullRequest"];
        pr["author"] = serde_json::Value::Null;
        pr["headRepository"] = serde_json::Value::Null;
        pr["mergeQueueEntry"] = serde_json::Value::Null;
        let d = parse_detail(data).unwrap();
        assert_eq!(d.author, "ghost");
        assert_eq!(d.head.repo, "org/api", "falls back to the PR's own repo");
        assert_eq!(d.queue, QueueState::NotQueued);
    }
}
