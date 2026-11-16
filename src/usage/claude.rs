//! Claude usage from both sources: the account's exact limits first, Claude Code's
//! local logs for token details, and the local estimate when the account fails.

use std::time::{Duration, Instant};

use super::claude_account::ClaudeAccount;
use super::claude_local::ClaudeLocal;
use super::{Provider, Source, Usage};

/// How often the account endpoint is asked while Claude Code rests. It is unofficial,
/// so it is asked rarely; the local logs keep the token details current in between.
const ACCOUNT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How often it is asked while the local logs show tokens being spent, so the share on
/// the notch follows the work within about a minute.
const ACTIVE_ACCOUNT_INTERVAL: Duration = Duration::from_secs(60);

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
    /// The local logs' token count then.
    tokens: Option<u64>,
    /// The answer, if the request worked.
    usage: Option<Usage>,
}

impl AccountCheck {
    /// Whether to ask again `elapsed` after this check: once [`ACCOUNT_INTERVAL`] has
    /// passed, someone else has signed in, or tokens were spent since an answer that is
    /// [`ACTIVE_ACCOUNT_INTERVAL`] old. A failed request waits the full interval, so a
    /// broken endpoint is not hammered.
    fn is_stale(&self, elapsed: Duration, signed_in: Option<&str>, tokens: Option<u64>) -> bool {
        let spent_since = self.usage.is_some() && self.tokens != tokens;
        elapsed >= ACCOUNT_INTERVAL
            || self.signed_in.as_deref() != signed_in
            || (spent_since && elapsed >= ACTIVE_ACCOUNT_INTERVAL)
    }
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

    /// The account's latest limits, asked again when [`AccountCheck::is_stale`] says
    /// so. `tokens` is what the local logs count now.
    fn account(&mut self, tokens: Option<u64>) -> Option<&Usage> {
        let signed_in = self.account.signed_in();
        let due = self.account_checked.as_ref().is_none_or(|checked| {
            checked.is_stale(checked.at.elapsed(), signed_in.as_deref(), tokens)
        });
        if due {
            self.account_checked = Some(AccountCheck {
                at: Instant::now(),
                signed_in,
                tokens,
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
        let tokens = local.as_ref().ok().and_then(|usage| usage.tokens);
        merge(self.account(tokens), local)
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
    fn the_account_is_asked_sooner_while_tokens_are_spent() {
        let check = |usage: Option<Usage>| AccountCheck {
            at: Instant::now(),
            signed_in: Some("me".to_owned()),
            tokens: Some(100),
            usage,
        };
        let answered = check(Some(account()));
        let minute = ACTIVE_ACCOUNT_INTERVAL;
        assert!(!answered.is_stale(minute, Some("me"), Some(100)), "resting");
        assert!(answered.is_stale(minute, Some("me"), Some(250)), "working");
        assert!(
            !answered.is_stale(minute / 2, Some("me"), Some(250)),
            "asked half a minute ago"
        );
        assert!(answered.is_stale(ACCOUNT_INTERVAL, Some("me"), Some(100)));
        assert!(answered.is_stale(Duration::ZERO, Some("you"), Some(100)));
        assert!(
            !check(None).is_stale(minute, Some("me"), Some(250)),
            "a failed request waits the full interval"
        );
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
