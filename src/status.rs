//! Whether a provider has an ongoing incident, from its public status page.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use ureq::Agent;

use crate::http;
use crate::usage::Provider;

/// How often the shown provider's status page is checked.
const STATUS_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Statuspage summaries; a couple of hundred bytes each.
const CLAUDE_STATUS: &str = "https://status.claude.com/api/v2/status.json";
const OPENAI_STATUS: &str = "https://status.openai.com/api/v2/status.json";

/// Google has no Gemini status page, only the Cloud incident feed (about 35 KB gzipped).
const GOOGLE_INCIDENTS: &str = "https://status.cloud.google.com/incidents.json";

/// The Cloud products the Gemini CLI depends on.
const GEMINI_PRODUCTS: [&str; 2] = ["Gemini Code Assist", "Vertex Gemini API"];

/// A provider's health, as its status page reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Operational,
    /// Minor incidents or maintenance.
    Degraded,
    /// A major outage.
    Down,
}

/// Checks the status of whichever provider the notch shows, now and then every
/// [`STATUS_INTERVAL`], on a background thread. Only the shown provider is checked, so
/// Google's larger feed is fetched only while Gemini is on screen.
///
/// Send the newly shown provider through the returned sender to check it right away.
/// `notify` runs on the background thread, so it should hand the result to the UI thread.
pub fn spawn(
    mut shown: Provider,
    notify: impl Fn(Provider, anyhow::Result<Health>) + Send + 'static,
) -> Sender<Provider> {
    let (switch, switched) = mpsc::channel();
    thread::spawn(move || {
        let agent = http::agent();
        loop {
            notify(shown, fetch(&agent, shown));
            match switched.recv_timeout(STATUS_INTERVAL) {
                Ok(provider) => shown = provider,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            // Only the last of several quick switches matters.
            while let Ok(provider) = switched.try_recv() {
                shown = provider;
            }
        }
    });
    switch
}

fn fetch(agent: &Agent, provider: Provider) -> anyhow::Result<Health> {
    let url = match provider {
        Provider::Claude => CLAUDE_STATUS,
        Provider::ChatGpt => OPENAI_STATUS,
        Provider::Gemini => GOOGLE_INCIDENTS,
    };
    let body = agent
        .get(url)
        .call()
        .with_context(|| format!("asking {url}"))?
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    match provider {
        Provider::Gemini => parse_google(&body),
        Provider::Claude | Provider::ChatGpt => parse_statuspage(&body),
    }
}

#[derive(Debug, Deserialize)]
struct Summary {
    status: Indicator,
}

#[derive(Debug, Deserialize)]
struct Indicator {
    indicator: String,
}

/// Reads a Statuspage `status.json` summary.
fn parse_statuspage(body: &str) -> anyhow::Result<Health> {
    let summary: Summary = serde_json::from_str(body).context("unexpected status format")?;
    Ok(match summary.status.indicator.as_str() {
        "none" => Health::Operational,
        "major" | "critical" => Health::Down,
        // "minor", "maintenance", and anything new.
        _ => Health::Degraded,
    })
}

#[derive(Debug, Deserialize)]
struct Incident {
    /// Missing or null while the incident is ongoing.
    end: Option<String>,
    severity: Option<String>,
    status_impact: Option<String>,
    #[serde(default)]
    affected_products: Vec<Product>,
}

#[derive(Debug, Deserialize)]
struct Product {
    title: String,
}

/// The worst ongoing incident affecting the Gemini CLI in Google's Cloud incident feed.
fn parse_google(body: &str) -> anyhow::Result<Health> {
    let incidents: Vec<Incident> =
        serde_json::from_str(body).context("unexpected incident feed format")?;
    let worst = incidents
        .iter()
        .filter(|incident| incident.end.is_none())
        .filter(|incident| {
            incident
                .affected_products
                .iter()
                .any(|product| GEMINI_PRODUCTS.contains(&product.title.as_str()))
        })
        .filter_map(|incident| match incident.status_impact.as_deref() {
            Some("SERVICE_INFORMATION") => None,
            Some("SERVICE_OUTAGE") => Some(Health::Down),
            _ if incident.severity.as_deref() == Some("high") => Some(Health::Down),
            _ => Some(Health::Degraded),
        })
        .max_by_key(|health| match health {
            Health::Operational => 0,
            Health::Degraded => 1,
            Health::Down => 2,
        });
    Ok(worst.unwrap_or(Health::Operational))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUSPAGE: &str = include_str!("../tests/fixtures/status/statuspage.json");
    const GOOGLE: &str = include_str!("../tests/fixtures/status/google_incidents.json");

    #[test]
    fn statuspage_indicator_maps_to_health() {
        assert_eq!(parse_statuspage(STATUSPAGE).ok(), Some(Health::Operational));
        let with = |indicator: &str| {
            let body =
                format!(r#"{{"status": {{"indicator": "{indicator}", "description": "x"}}}}"#);
            parse_statuspage(&body).ok()
        };
        assert_eq!(with("minor"), Some(Health::Degraded));
        assert_eq!(with("maintenance"), Some(Health::Degraded));
        assert_eq!(with("major"), Some(Health::Down));
        assert_eq!(with("critical"), Some(Health::Down));
        assert!(parse_statuspage("<html>").is_err());
    }

    #[test]
    fn only_ongoing_gemini_incidents_count() {
        // The fixture has an ended Gemini outage, an ongoing outage elsewhere, an ongoing
        // Gemini notice, and an ongoing Gemini disruption.
        assert_eq!(parse_google(GOOGLE).ok(), Some(Health::Degraded));
        assert_eq!(parse_google("[]").ok(), Some(Health::Operational));
        assert!(parse_google("{}").is_err());
    }
}
