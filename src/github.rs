//! GitHub REST API client and data models for Actions.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const API: &str = "https://api.github.com";

/// Shared rate-limit accounting, updated from response headers.
#[derive(Default)]
struct RateState {
    rate: Option<RateLimit>,
    /// When set and in the future, we are backing off (primary/secondary limit).
    pause_until: Option<Instant>,
}

/// Result of a conditional (ETag) request.
pub enum Cond<T> {
    /// The resource changed; here is the fresh value.
    Modified(T),
    /// `304 Not Modified` — caller should keep what it already has.
    NotModified,
}

/// Resolve a GitHub token: $GITHUB_TOKEN / $GH_TOKEN, else `gh auth token`.
pub fn resolve_token() -> Result<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("failed to run `gh auth token` (is the GitHub CLI installed and on PATH?)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`gh auth token` failed; run `gh auth login` or set GITHUB_TOKEN.\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        return Err(anyhow!("empty token from `gh auth token`"));
    }
    Ok(tok)
}

#[derive(Clone)]
pub struct Github {
    client: Client,
    /// Bearer token, re-resolvable if it expires mid-session (applied per request).
    token: Arc<Mutex<String>>,
    /// Per-key ETag cache for conditional requests.
    etags: Arc<Mutex<HashMap<String, String>>>,
    /// Last assembled repo list, returned when `/user/repos` is unchanged.
    repos_cache: Arc<Mutex<Vec<Repo>>>,
    /// Largest `X-Poll-Interval` (seconds) GitHub has asked us to honor; 0 = unset.
    poll_interval: Arc<AtomicU64>,
    rate_state: Arc<Mutex<RateState>>,
}

impl Github {
    pub fn new(token: &str) -> Result<Self> {
        // Auth is applied per request (see `send`) so the token can be refreshed
        // mid-session; only the static headers live on the client.
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("actui"));
        let client = Client::builder()
            .default_headers(headers)
            .gzip(true)
            .build()?;
        Ok(Self {
            client,
            token: Arc::new(Mutex::new(token.to_string())),
            etags: Arc::new(Mutex::new(HashMap::new())),
            repos_cache: Arc::new(Mutex::new(Vec::new())),
            poll_interval: Arc::new(AtomicU64::new(0)),
            rate_state: Arc::new(Mutex::new(RateState::default())),
        })
    }

    /// Send a request with shared rate-limit accounting: refuses while backing
    /// off, attaches the current token, records `X-RateLimit-*`/`X-Poll-Interval`
    /// headers, transparently re-resolves the token once on `401`, and converts a
    /// primary/secondary rate limit into a back-off + error.
    async fn send(&self, req: RequestBuilder) -> Result<Response> {
        if let Some(d) = self.pause_remaining() {
            return Err(anyhow!("backing off; retry in {}s", d.as_secs()));
        }
        let retry = req.try_clone();
        let resp = self.send_authed(req).await?;
        // Token may have expired — re-resolve once and replay the request.
        if resp.status() == StatusCode::UNAUTHORIZED {
            if let Some(retry) = retry {
                if self.refresh_token().await {
                    let resp = self.send_authed(retry).await?;
                    self.account(&resp)?;
                    return Ok(resp);
                }
            }
        }
        self.account(&resp)?;
        Ok(resp)
    }

    /// Attach the current bearer token and send.
    async fn send_authed(&self, req: RequestBuilder) -> Result<Response> {
        let token = self.token.lock().unwrap().clone();
        Ok(req.header(AUTHORIZATION, format!("Bearer {token}")).send().await?)
    }

    /// Re-resolve the token (e.g. after `gh` refreshed it). Returns true on change.
    async fn refresh_token(&self) -> bool {
        match tokio::task::spawn_blocking(resolve_token).await {
            Ok(Ok(tok)) => {
                *self.token.lock().unwrap() = tok;
                true
            }
            _ => false,
        }
    }

    /// Record rate headers and honor poll-interval; back off and error on a limit.
    fn account(&self, resp: &Response) -> Result<()> {
        self.record_rate(resp.headers());
        if let Some(secs) = resp
            .headers()
            .get("x-poll-interval")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            self.poll_interval.fetch_max(secs, Ordering::Relaxed);
        }
        let status = resp.status();
        if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
            let h = resp.headers();
            let remaining_zero =
                h.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()) == Some("0");
            let has_retry = h.contains_key("retry-after");
            if status == StatusCode::TOO_MANY_REQUESTS || remaining_zero || has_retry {
                self.note_rate_limited(h);
                return Err(anyhow!("rate limited; backing off"));
            }
        }
        Ok(())
    }

    /// Authenticated user login.
    pub async fn whoami(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct U {
            login: String,
        }
        let resp = self.send(self.client.get(format!("{API}/user"))).await?;
        let u: U = ensure_ok(resp).await?.json().await?;
        Ok(u.login)
    }

    /// Minimum seconds GitHub asked us to wait between polls (0 if never told).
    pub fn poll_interval_secs(&self) -> u64 {
        self.poll_interval.load(Ordering::Relaxed)
    }

    /// Latest known rate-limit snapshot (from response headers).
    pub fn rate(&self) -> Option<RateLimit> {
        self.rate_state.lock().unwrap().rate.clone()
    }

    /// Seconds we should keep backing off for, if a limit was hit; None = clear.
    pub fn pause_remaining(&self) -> Option<Duration> {
        let until = self.rate_state.lock().unwrap().pause_until?;
        let now = Instant::now();
        (until > now).then(|| until - now)
    }

    /// Update the cached rate snapshot from `X-RateLimit-*` headers.
    fn record_rate(&self, headers: &reqwest::header::HeaderMap) {
        let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u32>().ok());
        if let (Some(remaining), Some(limit)) =
            (get("x-ratelimit-remaining"), get("x-ratelimit-limit"))
        {
            self.rate_state.lock().unwrap().rate = Some(RateLimit { limit, remaining });
        }
    }

    /// We were rate-limited (primary or secondary): back off until `Retry-After`
    /// (relative secs) or `X-RateLimit-Reset` (epoch), defaulting to 60s.
    fn note_rate_limited(&self, headers: &reqwest::header::HeaderMap) {
        let hv = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let secs = backoff_secs(hv("retry-after"), hv("x-ratelimit-reset"), now);
        let until = Instant::now() + Duration::from_secs(secs);
        let mut st = self.rate_state.lock().unwrap();
        st.pause_until = Some(match st.pause_until {
            Some(prev) if prev > until => prev,
            _ => until,
        });
    }

    /// A conditional GET: sends `If-None-Match` from the cache under `key`,
    /// stores the new ETag, records rate-limit headers, honors `X-Poll-Interval`,
    /// backs off on `403`/`429`, and returns `NotModified` on `304`.
    async fn cond_get<T: DeserializeOwned>(
        &self,
        key: &str,
        url: String,
        query: &[(&str, String)],
    ) -> Result<Cond<T>> {
        let prev = self.etags.lock().unwrap().get(key).cloned();
        let mut req = self.client.get(url);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(tag) = prev {
            req = req.header(IF_NONE_MATCH, tag);
        }
        // `send` records rate/poll-interval headers and backs off on a limit.
        let resp = self.send(req).await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Cond::NotModified);
        }
        let resp = ensure_ok(resp).await?;
        if let Some(tag) = resp.headers().get(ETAG).and_then(|v| v.to_str().ok()) {
            let mut etags = self.etags.lock().unwrap();
            // Bound the cache: per-run job ETags accumulate over a long session.
            if etags.len() > 2000 {
                etags.retain(|k, _| !k.starts_with("jobs:"));
            }
            etags.insert(key.to_string(), tag.to_string());
        }
        Ok(Cond::Modified(resp.json::<T>().await?))
    }

    /// The last repo list returned by `list_repos` (used on a `304`).
    pub fn cached_repos(&self) -> Vec<Repo> {
        self.repos_cache.lock().unwrap().clone()
    }

    /// Repos the user owns or is an org member of (paginated, conditional on page 1).
    pub async fn list_repos(&self) -> Result<Cond<Vec<Repo>>> {
        let q = |page: u32| {
            vec![
                ("per_page", "100".to_string()),
                ("page", page.to_string()),
                ("affiliation", "owner,organization_member".to_string()),
                ("sort", "pushed".to_string()),
            ]
        };
        // Page 1 drives the conditional check; if unchanged, assume the set is too.
        let first: Vec<Repo> = match self
            .cond_get("repos:1", format!("{API}/user/repos"), &q(1))
            .await?
        {
            Cond::NotModified => return Ok(Cond::NotModified),
            Cond::Modified(v) => v,
        };
        let mut repos = first;
        if repos.len() == 100 {
            for page in 2..=10u32 {
                // A failing later page shouldn't discard the repos we already have.
                let batch: Vec<Repo> = match self
                    .send(self.client.get(format!("{API}/user/repos")).query(&q(page)))
                    .await
                    .and_then(|r| r.error_for_status().map_err(Into::into))
                {
                    Ok(r) => match r.json().await {
                        Ok(b) => b,
                        Err(_) => break,
                    },
                    Err(_) => break,
                };
                let n = batch.len();
                repos.extend(batch);
                if n < 100 {
                    break;
                }
            }
        }
        *self.repos_cache.lock().unwrap() = repos.clone();
        Ok(Cond::Modified(repos))
    }

    /// Most recent workflow runs for a repo (conditional).
    pub async fn list_runs(&self, full_name: &str, per_page: u32) -> Result<Cond<Vec<Run>>> {
        #[derive(Deserialize)]
        struct Resp {
            workflow_runs: Vec<Run>,
        }
        let key = format!("runs:{full_name}");
        let url = format!("{API}/repos/{full_name}/actions/runs");
        match self
            .cond_get::<Resp>(&key, url, &[("per_page", per_page.to_string())])
            .await?
        {
            Cond::Modified(r) => Ok(Cond::Modified(r.workflow_runs)),
            Cond::NotModified => Ok(Cond::NotModified),
        }
    }

    pub async fn list_jobs(&self, full_name: &str, run_id: u64) -> Result<Cond<Vec<Job>>> {
        #[derive(Deserialize)]
        struct Resp {
            jobs: Vec<Job>,
        }
        let key = format!("jobs:{run_id}");
        let url = format!("{API}/repos/{full_name}/actions/runs/{run_id}/jobs");
        match self.cond_get::<Resp>(&key, url, &[]).await? {
            Cond::Modified(r) => Ok(Cond::Modified(r.jobs)),
            Cond::NotModified => Ok(Cond::NotModified),
        }
    }

    pub async fn list_workflows(&self, full_name: &str) -> Result<Vec<Workflow>> {
        #[derive(Deserialize)]
        struct Resp {
            workflows: Vec<Workflow>,
        }
        let resp = self
            .send(
                self.client
                    .get(format!("{API}/repos/{full_name}/actions/workflows"))
                    .query(&[("per_page", "100")]),
            )
            .await?;
        let resp: Resp = ensure_ok(resp).await?.json().await?;
        Ok(resp.workflows)
    }

    pub async fn dispatch(
        &self,
        full_name: &str,
        workflow_id: u64,
        git_ref: &str,
        inputs: HashMap<String, String>,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body {
            r#ref: String,
            #[serde(skip_serializing_if = "HashMap::is_empty")]
            inputs: HashMap<String, String>,
        }
        let resp = self
            .send(
                self.client
                    .post(format!(
                        "{API}/repos/{full_name}/actions/workflows/{workflow_id}/dispatches"
                    ))
                    .json(&Body {
                        r#ref: git_ref.to_string(),
                        inputs,
                    }),
            )
            .await?;
        check(resp).await
    }

    /// Inspect a workflow's YAML for its `workflow_dispatch` inputs so the UI
    /// can render a typed form instead of asking for raw JSON.
    pub async fn workflow_inputs(
        &self,
        full_name: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<WfDispatch> {
        #[derive(Deserialize)]
        struct Contents {
            content: String,
            encoding: String,
        }
        let resp = self
            .send(
                self.client
                    .get(format!("{API}/repos/{full_name}/contents/{path}"))
                    .query(&[("ref", git_ref)]),
            )
            .await?;
        let c: Contents = ensure_ok(resp).await?.json().await?;
        if c.encoding != "base64" {
            return Err(anyhow!("unexpected content encoding: {}", c.encoding));
        }
        use base64::Engine;
        let cleaned: String = c.content.split_whitespace().collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(cleaned)
            .context("decoding workflow file")?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(parse_dispatch(&text))
    }

    pub async fn cancel(&self, full_name: &str, run_id: u64) -> Result<()> {
        let resp = self
            .send(self.client.post(format!("{API}/repos/{full_name}/actions/runs/{run_id}/cancel")))
            .await?;
        check(resp).await
    }

    pub async fn rerun(&self, full_name: &str, run_id: u64) -> Result<()> {
        let resp = self
            .send(self.client.post(format!("{API}/repos/{full_name}/actions/runs/{run_id}/rerun")))
            .await?;
        check(resp).await
    }

    pub async fn rerun_failed(&self, full_name: &str, run_id: u64) -> Result<()> {
        let resp = self
            .send(self.client.post(format!(
                "{API}/repos/{full_name}/actions/runs/{run_id}/rerun-failed-jobs"
            )))
            .await?;
        check(resp).await
    }

    /// Re-run a single job (and any jobs that depend on it).
    pub async fn rerun_job(&self, full_name: &str, job_id: u64) -> Result<()> {
        let resp = self
            .send(self.client.post(format!("{API}/repos/{full_name}/actions/jobs/{job_id}/rerun")))
            .await?;
        check(resp).await
    }

    /// Approve a run waiting for approval (e.g. a first-time contributor's PR).
    pub async fn approve(&self, full_name: &str, run_id: u64) -> Result<()> {
        let resp = self
            .send(self.client.post(format!("{API}/repos/{full_name}/actions/runs/{run_id}/approve")))
            .await?;
        check(resp).await
    }

    /// Environments whose deployment is gated, awaiting a required reviewer.
    pub async fn pending_deployments(
        &self,
        full_name: &str,
        run_id: u64,
    ) -> Result<Vec<PendingDeployment>> {
        let resp = self
            .send(self.client.get(format!(
                "{API}/repos/{full_name}/actions/runs/{run_id}/pending_deployments"
            )))
            .await?;
        let items: Vec<PendingDeployment> = ensure_ok(resp).await?.json().await?;
        Ok(items)
    }

    /// Approve (or reject) the gated deployments for the given environment ids.
    pub async fn review_deployments(
        &self,
        full_name: &str,
        run_id: u64,
        env_ids: &[u64],
        state: &str,
        comment: &str,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            environment_ids: &'a [u64],
            state: &'a str,
            comment: &'a str,
        }
        let resp = self
            .send(
                self.client
                    .post(format!(
                        "{API}/repos/{full_name}/actions/runs/{run_id}/pending_deployments"
                    ))
                    .json(&Body { environment_ids: env_ids, state, comment }),
            )
            .await?;
        check(resp).await
    }

    /// All branch names for a repo (paginated).
    pub async fn list_branches(&self, full_name: &str) -> Result<Vec<String>> {
        self.list_named(&format!("repos/{full_name}/branches")).await
    }

    /// All tag names for a repo (paginated).
    pub async fn list_tags(&self, full_name: &str) -> Result<Vec<String>> {
        self.list_named(&format!("repos/{full_name}/tags")).await
    }

    /// Fetch every page of a `{ name }`-shaped list endpoint (branches/tags),
    /// up to a sane cap so a huge repo can't spin forever.
    async fn list_named(&self, sub: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Named {
            name: String,
        }
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let resp = self
                .send(
                    self.client
                        .get(format!("{API}/{sub}"))
                        .query(&[("per_page", "100".to_string()), ("page", page.to_string())]),
                )
                .await?;
            let batch: Vec<Named> = ensure_ok(resp).await?.json().await?;
            let n = batch.len();
            out.extend(batch.into_iter().map(|b| b.name));
            if n < 100 || page >= 10 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Organizations the authenticated user belongs to (paginated).
    pub async fn list_orgs(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Org {
            login: String,
        }
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let resp = self
                .send(
                    self.client
                        .get(format!("{API}/user/orgs"))
                        .query(&[("per_page", "100".to_string()), ("page", page.to_string())]),
                )
                .await?;
            let batch: Vec<Org> = ensure_ok(resp).await?.json().await?;
            let n = batch.len();
            out.extend(batch.into_iter().map(|o| o.login));
            if n < 100 || page >= 5 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Self-hosted runners registered to an org. Requires org-admin (or the
    /// `manage_runners:org` scope); the caller surfaces a `403` as "no access".
    pub async fn list_org_runners(&self, org: &str) -> Result<Vec<Runner>> {
        #[derive(Deserialize)]
        struct Resp {
            runners: Vec<Runner>,
        }
        let resp = self
            .send(
                self.client
                    .get(format!("{API}/orgs/{org}/actions/runners"))
                    .query(&[("per_page", "100")]),
            )
            .await?;
        let resp: Resp = ensure_ok(resp).await?.json().await?;
        Ok(resp.runners)
    }

    /// Artifacts produced by a run.
    pub async fn list_artifacts(&self, full_name: &str, run_id: u64) -> Result<Vec<Artifact>> {
        #[derive(Deserialize)]
        struct Resp {
            artifacts: Vec<Artifact>,
        }
        let resp = self
            .send(
                self.client
                    .get(format!("{API}/repos/{full_name}/actions/runs/{run_id}/artifacts"))
                    .query(&[("per_page", "100")]),
            )
            .await?;
        let resp: Resp = ensure_ok(resp).await?.json().await?;
        Ok(resp.artifacts)
    }

    /// Download an artifact's zip (follows the redirect to the blob).
    pub async fn download_artifact(&self, full_name: &str, artifact_id: u64) -> Result<Vec<u8>> {
        let resp = self
            .send(
                self.client
                    .get(format!("{API}/repos/{full_name}/actions/artifacts/{artifact_id}/zip")),
            )
            .await?;
        Ok(ensure_ok(resp).await?.bytes().await?.to_vec())
    }

    /// Check-run annotations for a job — the file:line errors/warnings GitHub
    /// distils from a job's output (the red boxes shown on a PR). `check_run_url`
    /// is the job's own `check_run_url`; the annotations hang off `…/annotations`.
    pub async fn annotations(&self, check_run_url: &str) -> Result<Vec<Annotation>> {
        let resp = self
            .send(self.client.get(format!("{check_run_url}/annotations")))
            .await?;
        Ok(ensure_ok(resp).await?.json().await?)
    }

    /// Plain-text logs for a single job (follows the redirect to the log blob).
    pub async fn job_logs(&self, full_name: &str, job_id: u64) -> Result<String> {
        let resp = self
            .send(self.client.get(format!("{API}/repos/{full_name}/actions/jobs/{job_id}/logs")))
            .await?;
        Ok(ensure_ok(resp).await?.text().await?)
    }

    /// One-shot snapshot used at startup (the `/rate_limit` endpoint itself does
    /// not consume quota). Also seeds the shared rate state for the header.
    pub async fn rate_limit(&self) -> Result<RateLimit> {
        #[derive(Deserialize)]
        struct Resp {
            resources: Res,
        }
        #[derive(Deserialize)]
        struct Res {
            core: RateLimit,
        }
        let resp = self.send(self.client.get(format!("{API}/rate_limit"))).await?;
        let resp: Resp = ensure_ok(resp).await?.json().await?;
        self.rate_state.lock().unwrap().rate = Some(resp.resources.core.clone());
        Ok(resp.resources.core)
    }
}

/// Ensure a response is a success, otherwise surface GitHub's own error
/// `message` field (like `check`) instead of reqwest's generic status error,
/// which discards the body. Returns the response on success for chaining.
async fn ensure_ok(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow!("GitHub API {status}: {}", first_line(&body)))
}

/// Like `ensure_ok` but for mutating endpoints that return no body of interest.
async fn check(resp: Response) -> Result<()> {
    ensure_ok(resp).await.map(|_| ())
}

/// Seconds to back off after a rate-limit response: prefer `Retry-After`
/// (relative seconds), else `X-RateLimit-Reset` (epoch) minus now, defaulting
/// to 60s. Clamped to 1..=3600 so a bad/absent header can neither hammer the
/// API nor stall the poller for hours.
fn backoff_secs(retry_after: Option<&str>, reset_epoch: Option<&str>, now_epoch: u64) -> u64 {
    retry_after
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            reset_epoch
                .and_then(|s| s.parse::<u64>().ok())
                .map(|reset| reset.saturating_sub(now_epoch))
        })
        .unwrap_or(60)
        .clamp(1, 3600)
}

fn first_line(s: &str) -> String {
    // Try to surface the API "message" field, else first line.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            return m.to_string();
        }
    }
    s.lines().next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// workflow_dispatch inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum WfInputKind {
    Text,
    Number,
    Boolean,
    Choice(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct WfInput {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub default: String,
    pub kind: WfInputKind,
}

pub enum WfDispatch {
    /// The workflow has no `workflow_dispatch` trigger; can't be dispatched.
    NotDispatchable,
    /// Dispatchable, with these declared inputs (possibly empty).
    Inputs(Vec<WfInput>),
}

fn yaml_str(v: Option<&serde_yaml::Value>) -> String {
    match v {
        Some(serde_yaml::Value::String(s)) => s.clone(),
        Some(serde_yaml::Value::Bool(b)) => b.to_string(),
        Some(serde_yaml::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_dispatch(text: &str) -> WfDispatch {
    use serde_yaml::Value;
    let Ok(root) = serde_yaml::from_str::<Value>(text) else {
        return WfDispatch::NotDispatchable;
    };
    let Some(map) = root.as_mapping() else {
        return WfDispatch::NotDispatchable;
    };
    // YAML parses the bare key `on` as the boolean `true`, so check both.
    let on = map.get("on").or_else(|| map.get(Value::Bool(true)));
    let Some(on) = on else {
        return WfDispatch::NotDispatchable;
    };

    // The value under `workflow_dispatch` (a mapping with `inputs:` when present).
    // For the string/list forms there are no inputs, so we point at `on` itself
    // (its `.as_mapping()` is None → empty input set).
    let wd: Option<&Value> = match on {
        Value::String(s) => (s == "workflow_dispatch").then_some(on),
        Value::Sequence(seq) => seq
            .iter()
            .any(|v| v.as_str() == Some("workflow_dispatch"))
            .then_some(on),
        Value::Mapping(m) => match m.get("workflow_dispatch") {
            Some(v) => Some(v),
            None => return WfDispatch::NotDispatchable,
        },
        _ => None,
    };
    let Some(wd) = wd else {
        return WfDispatch::NotDispatchable;
    };

    let mut out = Vec::new();
    if let Some(inputs) = wd.as_mapping().and_then(|m| m.get("inputs")).and_then(|v| v.as_mapping()) {
        for (k, v) in inputs {
            let Some(name) = k.as_str() else { continue };
            let m = v.as_mapping();
            let get = |key: &str| m.and_then(|mm| mm.get(key));
            let typ = get("type").and_then(|x| x.as_str()).unwrap_or("string");
            let kind = match typ {
                "boolean" => WfInputKind::Boolean,
                "number" => WfInputKind::Number,
                "choice" => {
                    let opts = get("options")
                        .and_then(|o| o.as_sequence())
                        .map(|s| s.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    WfInputKind::Choice(opts)
                }
                _ => WfInputKind::Text,
            };
            out.push(WfInput {
                name: name.to_string(),
                description: get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                required: get("required").and_then(|x| x.as_bool()).unwrap_or(false),
                default: yaml_str(get("default")),
                kind,
            });
        }
    }
    WfDispatch::Inputs(out)
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub full_name: String,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Run {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_title: String,
    #[serde(default)]
    pub head_branch: Option<String>,
    pub run_number: u64,
    pub event: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub run_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub actor: Option<Actor>,
    pub repository: RunRepo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunRepo {
    pub full_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Actor {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub status: String,
    /// Job page on github.com (may be empty on older API responses).
    #[serde(default)]
    pub html_url: String,
    /// Check-run API URL for this job; the source of its failure annotations.
    #[serde(default)]
    pub check_run_url: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Per-step status/timing — updates live while the job runs.
    #[serde(default)]
    pub steps: Vec<Step>,
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.status != "completed"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
}

/// One environment awaiting a deployment review for a run.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingDeployment {
    pub environment: EnvRef,
    /// Whether the authenticated user is allowed to approve this one.
    #[serde(default)]
    pub current_user_can_approve: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvRef {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
}

/// One check-run annotation: a file:line problem (or warning/notice) GitHub
/// extracted from a job's output.
#[derive(Debug, Clone, Deserialize)]
pub struct Annotation {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub start_line: u64,
    #[serde(default)]
    pub end_line: u64,
    /// "failure" | "warning" | "notice" (absent on some responses).
    #[serde(default)]
    pub annotation_level: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: String,
}

/// A self-hosted runner registered to an org.
#[derive(Debug, Clone, Deserialize)]
pub struct Runner {
    pub name: String,
    #[serde(default)]
    pub os: String,
    /// "online" | "offline".
    #[serde(default)]
    pub status: String,
    /// True while the runner is executing a job.
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub labels: Vec<RunnerLabel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerLabel {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub size_in_bytes: u64,
    #[serde(default)]
    pub expired: bool,
}

/// Run lifecycle, normalized from status + conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running,
    Success,
    Failure,
    Cancelled,
    Skipped,
    Other,
}

impl Run {
    pub fn state(&self) -> RunState {
        match self.status.as_str() {
            "queued" | "pending" | "waiting" | "requested" => RunState::Queued,
            "in_progress" => RunState::Running,
            "completed" => match self.conclusion.as_deref() {
                Some("success") => RunState::Success,
                Some("failure") | Some("timed_out") | Some("startup_failure") => RunState::Failure,
                Some("cancelled") => RunState::Cancelled,
                Some("skipped") | Some("neutral") | Some("stale") => RunState::Skipped,
                Some("action_required") => RunState::Queued,
                _ => RunState::Other,
            },
            _ => RunState::Other,
        }
    }

    /// True when the run is held awaiting approval — either a fork-PR approval
    /// (`action_required`) or an environment deployment review (`waiting`).
    pub fn needs_approval(&self) -> bool {
        matches!(self.status.as_str(), "waiting" | "action_required")
            || (self.status == "completed"
                && self.conclusion.as_deref() == Some("action_required"))
    }

    pub fn title(&self) -> &str {
        if !self.display_title.is_empty() {
            &self.display_title
        } else if let Some(n) = &self.name {
            n
        } else {
            "(untitled)"
        }
    }

    pub fn workflow_name(&self) -> &str {
        self.name.as_deref().unwrap_or("workflow")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dispatch_inputs() {
        // Note: YAML parses the bare `on:` key as boolean true.
        let yaml = r#"
name: CI
on:
  workflow_dispatch:
    inputs:
      environment:
        type: choice
        options: [dev, staging, prod]
        default: staging
      verbose:
        type: boolean
        default: true
      message:
        description: A note
        required: true
jobs: {}
"#;
        let WfDispatch::Inputs(inputs) = parse_dispatch(yaml) else {
            panic!("expected dispatchable");
        };
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name, "environment");
        match &inputs[0].kind {
            WfInputKind::Choice(opts) => assert_eq!(opts, &["dev", "staging", "prod"]),
            _ => panic!("expected choice"),
        }
        assert_eq!(inputs[0].default, "staging");
        assert!(matches!(inputs[1].kind, WfInputKind::Boolean));
        assert_eq!(inputs[1].default, "true");
        assert!(inputs[2].required);
        assert!(matches!(inputs[2].kind, WfInputKind::Text));
    }

    #[test]
    fn dispatchable_with_no_inputs() {
        let yaml = "on: [push, workflow_dispatch]\njobs: {}\n";
        assert!(matches!(parse_dispatch(yaml), WfDispatch::Inputs(v) if v.is_empty()));
    }

    #[test]
    fn not_dispatchable() {
        assert!(matches!(parse_dispatch("on: push\njobs: {}\n"), WfDispatch::NotDispatchable));
        assert!(matches!(
            parse_dispatch("on:\n  push:\n    branches: [main]\njobs: {}\n"),
            WfDispatch::NotDispatchable
        ));
    }

    #[test]
    fn parses_check_run_annotations() {
        // The shape GitHub returns from `…/check-runs/{id}/annotations`.
        let json = r#"[
          {
            "path": "src/main.rs",
            "start_line": 42,
            "end_line": 42,
            "annotation_level": "failure",
            "title": "rustc",
            "message": "error[E0382]: borrow of moved value: `x`"
          },
          {
            "path": ".github/workflows/ci.yml",
            "start_line": 7,
            "end_line": 7,
            "message": "no level field here"
          }
        ]"#;
        let anns: Vec<Annotation> = serde_json::from_str(json).unwrap();
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].path, "src/main.rs");
        assert_eq!(anns[0].start_line, 42);
        assert_eq!(anns[0].annotation_level.as_deref(), Some("failure"));
        // A missing `annotation_level` deserializes to None (we bucket it as a warning).
        assert!(anns[1].annotation_level.is_none());
    }

    #[test]
    fn backoff_prefers_retry_after_then_reset_then_default() {
        // Retry-After wins even when a reset epoch is also present.
        assert_eq!(backoff_secs(Some("30"), Some("9999999999"), 1000), 30);
        // No Retry-After: fall back to reset-epoch minus now.
        assert_eq!(backoff_secs(None, Some("1120"), 1000), 120);
        // Neither header present: the 60s default.
        assert_eq!(backoff_secs(None, None, 1000), 60);
    }

    #[test]
    fn backoff_clamps_and_saturates() {
        // A reset already in the past saturates to 0, then clamps up to the 1s floor.
        assert_eq!(backoff_secs(None, Some("500"), 1000), 1);
        // An absurd reset clamps to the 1h ceiling.
        assert_eq!(backoff_secs(None, Some("999999999"), 0), 3600);
        // A garbage Retry-After is ignored and falls through to the default.
        assert_eq!(backoff_secs(Some("soon"), None, 0), 60);
    }
}
