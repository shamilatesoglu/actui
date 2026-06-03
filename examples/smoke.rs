//! Headless check of the GitHub plumbing (no TUI). Run: cargo run --example smoke
#![allow(dead_code)] // only exercises a subset of the github module
#[path = "../src/github.rs"]
mod github;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = github::resolve_token()?;
    let gh = github::Github::new(&token)?;
    let user = gh.whoami().await?;
    println!("user: {user}");
    let rl = gh.rate_limit().await?;
    println!("rate: {}/{}", rl.remaining, rl.limit);
    let repos = match gh.list_repos().await? {
        github::Cond::Modified(r) => r,
        github::Cond::NotModified => gh.cached_repos(),
    };
    println!("repos: {}", repos.len());
    let mut total_runs = 0;
    for r in repos.iter().take(5) {
        match gh.list_runs(&r.full_name, 5).await {
            Ok(github::Cond::Modified(runs)) => {
                total_runs += runs.len();
                if let Some(run) = runs.first() {
                    println!(
                        "  {} -> {} runs (latest: {} #{} [{:?}])",
                        r.full_name,
                        runs.len(),
                        run.workflow_name(),
                        run.run_number,
                        run.state()
                    );
                } else {
                    println!("  {} -> 0 runs", r.full_name);
                }
            }
            Ok(github::Cond::NotModified) => println!("  {} -> 304", r.full_name),
            Err(e) => println!("  {} -> err {e}", r.full_name),
        }
    }
    println!("sampled runs: {total_runs}");
    Ok(())
}
