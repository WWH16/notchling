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
/// [`ACCOUNT_INTERVAL`], or at once after the user switches accounts.
#[derive(Debug)]
pub struct Claude {
    local: ClaudeLocal,
    account: ClaudeAccount,
    account_checked: Option<AccountCheck>,
}

/// The last time the account endpoint was asked.
#[derive(Debug)]
struct AccountCheck {
    at: Instant,
    /// Who was signed in then, per [`ClaudeAccount::signed_in`].
    signed_in: Option<String>,
    /// The answer, if the request worked.
    usage: Option<Usage>,
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

    /// The account's latest limits, asking again once [`ACCOUNT_INTERVAL`] has passed
    /// or someone else has signed in. A failed request counts as asked, so a broken
    /// endpoint is not hammered. Turned off in the config, nothing is asked and the
    /// local estimate shows.
    fn account(&mut self) -> Option<&Usage> {
        if !Config::load().claude_account() {
            self.account_checked = None;
            return None;
        }
        let signed_in = self.account.signed_in();
        let due = self.account_checked.as_ref().is_none_or(|checked| {
            checked.at.elapsed() >= ACCOUNT_INTERVAL || checked.signed_in != signed_in
        });
        if due {
            self.account_checked = Some(AccountCheck {
                at: Instant::now(),
                signed_in,
                usage: self.account.fetch().ok(),
            });
        }
        self.account_checked.as_ref()?.usage.as_ref()
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
    use crate::usage::Limit;

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    fn local() -> Usage {
        Usage {
            used: None,
            resets_at: Some(time("2026-10-27T06:00:00Z")),
            limits: Vec::new(),
            via: None,
            tokens: Some(2100),
            tokens_by_model: BTreeMap::from([("claude-opus-5-5".to_owned(), 2100)]),
            burn_rate: Some(35.0),
            estimated: true,
        }
    }

    fn weekly() -> Limit {
        Limit {
            group: None,
            name: "Weekly (all models)".to_owned(),
            used: 0.07,
            resets_at: None,
        }
    }

    fn account() -> Usage {
        Usage {
            used: Some(0.41),
            resets_at: Some(time("2026-10-27T05:00:00Z")),
            limits: vec![weekly()],
            ..Usage::default()
        }
    }

    #[test]
    fn account_limits_come_first_with_local_token_details() {
        let merged = merge(Some(&account()), Ok(local())).expect("both sources worked");

        assert_eq!(merged.used, Some(0.41));
        assert_eq!(merged.resets_at, Some(time("2026-10-27T05:00:00Z")));
        assert_eq!(merged.limits, [weekly()]);
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
