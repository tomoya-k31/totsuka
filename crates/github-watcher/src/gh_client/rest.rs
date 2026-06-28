use super::{IssueUpdate, PrUpdate, ReleaseUpdate, RepoSlug};
use crate::error::WatcherError;
use chrono::{DateTime, Utc};
use reqwest::{header, Client, Response};
use serde::Deserialize;
use totsuka_core::Secret;

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn next_link(resp: &Response) -> Option<String> {
    let link = resp.headers().get(header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let part = part.trim();
        if part.ends_with("rel=\"next\"") {
            let lt = part.find('<')?;
            let gt = part.find('>')?;
            return Some(part[lt + 1..gt].to_string());
        }
    }
    None
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &Client,
    url: &str,
    token: &Secret<String>,
) -> Result<(T, Option<String>), WatcherError> {
    let resp = client
        .get(url)
        .bearer_auth(token.expose())
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(WatcherError::Internal(format!("REST {s}: {body}")));
    }
    let next = next_link(&resp);
    let body: T = resp.json().await?;
    Ok((body, next))
}

#[derive(Deserialize)]
struct IssueRow {
    node_id: String,
    number: u64,
    state: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>, // present iff this row is actually a PR
}

pub(crate) async fn list_issues(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<IssueUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/issues?since={}&state=all&per_page=100",
        repo.owner,
        repo.repo,
        rfc3339(since),
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<IssueRow>, _) = get_json(client, &url, token).await?;
        for r in rows {
            if r.pull_request.is_some() {
                continue;
            }
            out.push(IssueUpdate {
                node_id: r.node_id,
                repo: repo.clone(),
                number: r.number,
                updated_at: r.updated_at,
                state: r.state,
            });
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct PrRow {
    node_id: String,
    number: u64,
    head: PrHead,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    merged_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct PrHead {
    #[serde(rename = "ref")]
    ref_: String,
}

pub(crate) async fn list_prs(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<PrUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/pulls?state=all&sort=updated&direction=desc&per_page=100",
        repo.owner, repo.repo,
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<PrRow>, _) = get_json(client, &url, token).await?;
        for r in &rows {
            if r.updated_at <= since {
                continue;
            }
            out.push(PrUpdate {
                node_id: r.node_id.clone(),
                repo: repo.clone(),
                number: r.number,
                head_ref: r.head.ref_.clone(),
                body: r.body.clone(),
                merged: r.merged_at.is_some(),
                merged_at: r.merged_at,
                updated_at: r.updated_at,
            });
        }
        // Early exit: descending sort means once we see a row at or before `since`, stop.
        let saw_old = rows.iter().any(|r| r.updated_at <= since);
        match next {
            Some(n) if !saw_old => url = n,
            _ => break,
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ReleaseRow {
    node_id: String,
    tag_name: String,
    published_at: Option<DateTime<Utc>>,
}

pub(crate) async fn list_releases(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<ReleaseUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/releases?per_page=100",
        repo.owner, repo.repo,
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<ReleaseRow>, _) = get_json(client, &url, token).await?;
        for r in rows {
            let Some(p) = r.published_at else {
                continue; // draft
            };
            if p <= since {
                continue;
            }
            out.push(ReleaseUpdate {
                node_id: r.node_id,
                repo: repo.clone(),
                tag: r.tag_name,
                published_at: p,
            });
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}
