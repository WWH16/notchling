//! Claude usage from both sources: the account's exact limits first, Claude Code's
//! local logs for token details, and the local estimate when the account fails.

use std::time::{Duration, Instant};

use super::claude_account::ClaudeAccount;
use super::claude_local::ClaudeLocal;
use super::{Provider, Source, Usage};
use crate::config::Config;

/// How often the account endpoint is asked. It is unofficial, so it is asked rarely;
/// the local logs keep the token details current in between.
const ACCOUNT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Reads the local logs on every poll and the account endpoint every
/// [`ACCOUNT_INTERVAL`].
#[derive(Debug)]
pub struct Claude {
    local: ClaudeLocal,
    account: ClaudeAccount,
    /// When the account was last asked, and its answer if that worked.
    account_checked: Option<(Instant, Option<Usage>)>,
}

impl Claude {
    /// Reads the current Windows user's Claude Code logs and login.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        Ok(Self {
            local: ClaudeLocal::try_new()?,
            account: ClaudeAccount::try_new()?,
            account_checked: None,
        })
    }

    /// The account's latest limits, asking again once [`ACCOUNT_INTERVAL`] has passed.
    /// A failed request counts as asked, so a broken endpoint is not hammered. Turned
    /// off in the config, nothing is asked and the local estimate shows.
    fn account(&mut self) -> Option<&Usage> {
        if !Config::load().claude_account() {
            self.account_checked = None;
            return None;
        }
        let due = self
            .account_checked
            .as_ref()
            .is_none_or(|(checked, _)| checked.elapsed() >= ACCOUNT_INTERVAL);
        if due {
            self.account_checked = Some((Instant::now(), self.account.fetch().ok()));
        }
        self.account_checked.as_ref()?.1.as_ref()
    }
}

impl Source for Claude {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn read(&mut self) -> anyhow::Result<Usage> {
        let local = self.local.read();
        merge(self.account(), local)
    }
}

/// The account's limits with whatever token details the local logs gave, or the local
/// estimate alone when the account gave no answer.
fn merge(account: Option<&Usage>, local: anyhow::Result<Usage>) -> anyhow::Result<Usage> {
    let Some(account) = account else {
        return local;
    };
    let local = local.unwrap_or_default();
    Ok(Usage {
        tokens: local.tokens,
        tokens_by_model: local.tokens_by_model,
        burn_rate: local.burn_rate,
        ..account.clone()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};

    use super::*;

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    fn local() -> Usage {
        Usage {
            used: None,
            resets_at: Some(time("2026-10-27T06:00:00Z")),
            weekly_used: None,
            tokens: Some(2100),
            tokens_by_model: BTreeMap::from([("claude-opus-5-5".to_owned(), 2100)]),
            burn_rate: Some(35.0),
            estimated: true,
        }
    }

    fn account() -> Usage {
        Usage {
            used: Some(0.41),
            resets_at: Some(time("2026-10-27T05:00:00Z")),
            weekly_used: Some(0.07),
            ..Usage::default()
        }
    }

    #[test]
    fn account_limits_come_first_with_local_token_details() {
        let merged = merge(Some(&account()), Ok(local())).expect("both sources worked");

        assert_eq!(merged.used, Some(0.41));
        assert_eq!(merged.resets_at, Some(time("2026-10-27T05:00:00Z")));
        assert_eq!(merged.weekly_used, Some(0.07));
        assert_eq!(merged.tokens, Some(2100));
        assert_eq!(merged.tokens_by_model, local().tokens_by_model);
        assert_eq!(merged.burn_rate, Some(35.0));
        assert!(!merged.estimated, "exact limits carry no badge");
    }

    #[test]
    fn without_the_account_the_local_estimate_shows() {
        let merged = merge(None, Ok(local())).expect("the local source worked");
        assert_eq!(merged, local());
        assert!(merged.estimated, "the fallback carries the badge");
    }

    #[test]
    fn either_source_alone_is_enough() {
        let merged =
            merge(Some(&account()), Err(anyhow::anyhow!("no logs"))).expect("the account worked");
        assert_eq!(merged, account());

        let err = merge(None, Err(anyhow::anyhow!("no logs"))).expect_err("both failed");
        assert_eq!(err.to_string(), "no logs");
    }
}
