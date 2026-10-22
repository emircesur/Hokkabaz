//! GitHub Trigger as a polling node: reads the repository events feed
//! (`GET /repos/{owner}/{repo}/events`) and emits the events the user selected. Unlike
//! n8n's webhook-based trigger it creates nothing on GitHub, so it works behind NAT and
//! on the phone, at the cost of GitHub's feed latency (events can lag by minutes).

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use hk_flow::FlowError;
use hk_flow::description::NodeTypeDescription;

use super::{CREDENTIAL, TRIGGER_TYPE};
use crate::credentials::CredentialTypes;
use crate::describe::{NodeBuilder, Prop, poll_times_prop};
use crate::pathparam::{encode, string_list};
use crate::poll::{PollBatch, PollContext, PolledItem, PollingTrigger};
use crate::template::display;

/// n8n event name -> repository events API `type`.
const EVENTS: &[(&str, &str)] = &[
    ("push", "PushEvent"),
    ("issues", "IssuesEvent"),
    ("issue_comment", "IssueCommentEvent"),
    ("pull_request", "PullRequestEvent"),
    ("pull_request_review", "PullRequestReviewEvent"),
    ("pull_request_review_comment", "PullRequestReviewCommentEvent"),
    ("commit_comment", "CommitCommentEvent"),
    ("create", "CreateEvent"),
    ("delete", "DeleteEvent"),
    ("fork", "ForkEvent"),
    ("gollum", "GollumEvent"),
    ("member", "MemberEvent"),
    ("public", "PublicEvent"),
    ("release", "ReleaseEvent"),
    ("star", "WatchEvent"),
    ("watch", "WatchEvent"),
];

#[derive(Debug)]
pub struct GithubTrigger {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl GithubTrigger {
    pub fn new(creds: &CredentialTypes) -> Self {
        let desc = NodeBuilder::new(TRIGGER_TYPE, "GitHub Trigger")
            .description("Starts the workflow when GitHub events occur (polling)")
            .icon("fa:github")
            .color("#000000")
            .credential(CREDENTIAL, true)
            .polling_trigger()
            .property(Prop::string("owner", "Repository Owner").required())
            .property(Prop::string("repository", "Repository Name").required())
            .property(Prop::string("events", "Events").default("*").description(
                "Comma-separated event names (push, issues, issue_comment, pull_request, \
                 release, create, delete, fork, star, ...) or `*` for all",
            ))
            .property(poll_times_prop())
            .build();
        Self { desc, creds: creds.clone() }
    }
}

fn wanted_types(events: &Value) -> Option<HashSet<&'static str>> {
    let names = string_list(events);
    if names.is_empty() || names.iter().any(|n| n == "*") {
        return None;
    }
    Some(
        EVENTS
            .iter()
            .filter(|(n, _)| names.iter().any(|w| w == n))
            .map(|(_, t)| *t)
            .collect(),
    )
}

#[async_trait]
impl PollingTrigger for GithubTrigger {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn poll(&self, cx: &PollContext, _cursor: Option<&Value>) -> Result<PollBatch, FlowError> {
        let node_err = |m: String| FlowError::Node { node: cx.node().name.clone(), message: m };
        let owner = cx.param_str("owner", "")?;
        let repo = cx.param_str("repository", "")?;
        if owner.is_empty() || repo.is_empty() {
            return Err(node_err("Repository owner and name are required".into()));
        }
        let wanted = wanted_types(&cx.param("events")?.unwrap_or_else(|| Value::from("*")));

        let raw = cx.credential(CREDENTIAL).await?;
        let data = self.creds.with_defaults(CREDENTIAL, &raw);
        let auth = self.creds.apply(CREDENTIAL, &data).map_err(|e| node_err(e.to_string()))?;
        let server = data.get("server").map(display).unwrap_or_default();
        let url = format!(
            "{}/repos/{}/{}/events?per_page=100",
            server.trim_end_matches('/'),
            encode(&owner),
            encode(&repo)
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| node_err(format!("Could not build HTTP client: {}", e.without_url())))?;
        let mut rb = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "hokkabaz");
        for (k, v) in &auth.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let res = rb
            .send()
            .await
            .map_err(|e| node_err(format!("The request failed: {}", e.without_url())))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| node_err(format!("Could not read the response: {}", e.without_url())))?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let detail = body.get("message").map(display).unwrap_or_default();
            return Err(node_err(format!("The service returned an error: {} {detail}", status.as_u16())));
        }

        // newest first on the wire; emit oldest first so downstream sees them in order
        let mut items: Vec<PolledItem> = body
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                let ty = e.get("type").and_then(Value::as_str).unwrap_or_default();
                wanted.as_ref().is_none_or(|w| w.contains(ty))
            })
            .filter_map(|e| Some(PolledItem::new(display(e.get("id")?), e.clone())))
            .collect();
        items.reverse();
        Ok(PollBatch { items, cursor: None })
    }
}
