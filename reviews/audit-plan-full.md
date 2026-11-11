# Full Spectrum Audit: notchling bug hunt

**Date**: 2026-11-12
**Mode**: Repo Health (bug hunt, backend and frontend)
**Source**: `main` at `3886764`, working tree clean
**Lenses Run**: 10/10, from one full read of every source file (`src/**`, `ui/*.slint`).
The individual lens skill prompts were not loaded; each section below holds what that
lens found, not the lens's full checklist output.
**Lenses Skipped**: none

Baseline before any change: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
and `cargo test` (119 tests) all passed. After the fixes: all three pass, 121 tests.

---

## Master Verdict: APPROVE WITH CHANGES

The code is careful: fail-safe hook mode, user-only pipe, secrets kept out of errors, good
unit coverage of the pure logic. Six real bugs were found, all in how features interact
(shortcuts with card kinds, fullscreen hiding with requests, alerts with settings). All six
are fixed, one commit each. None was verified in the running app; they are covered by
compile, clippy and unit tests only.

## Verdict by Lens

| # | Lens | Verdict | Critical | Recommendations |
|---|------|---------|----------|-----------------|
| 1 | Design Engineer | APPROVE | 0 | 0 |
| 2 | UX Engineer | APPROVE WITH CHANGES | 2 (fixed) | 2 |
| 3 | Frontend Engineer | APPROVE WITH CHANGES | 1 (fixed) | 1 |
| 4 | Backend Engineer | APPROVE WITH CHANGES | 3 (fixed) | 2 |
| 5 | Structural Engineer | APPROVE | 0 | 0 |
| 6 | Repo Maintainer | APPROVE | 0 | 0 |
| 7 | Production Readiness | APPROVE WITH CHANGES | 1 (fixed) | 2 |
| 8 | DX Engineer | APPROVE | 0 | 1 |
| 9 | Performance Engineer | APPROVE | 0 | 1 |
| 10 | Accessibility Engineer | APPROVE WITH CHANGES | 0 | 1 |

## All Critical Issues (Must Fix)

All fixed.

| # | Issue | Lens(es) | File(s) | Impact | Fix |
|---|-------|----------|---------|--------|-----|
| 1 | Ctrl+Alt+Y / Ctrl+Alt+N act on question and reply cards | UX, Frontend | `ui/notch.slint` `answer()`, `src/main.rs` `answer_with_shortcuts` | The key sent Allow or Deny for a card the hook cannot use it on, so the card was dismissed; the keys were also taken from other apps while such a card showed | `efd8730`: `answer()` only acts on a permission card; keys claimed only for that kind, re-checked when the kind changes |
| 2 | Shortcuts stay claimed while the notch hides behind a fullscreen app | UX, Prod | `src/main.rs` `hide_during_fullscreen`, `src/window.rs` | The game or presentation in front never received those key combinations while a request waited | `18db6db`: released on hide, claimed again on show |
| 3 | A usage threshold alerts twice in one window | Backend | `src/usage/thresholds.rs` `crossed` | Reset times jitter around the minute (04:59:59.9, then 05:00:00.1); truncating to the minute made two windows | `a1a881f`: key the window by the nearest minute; failing test first |
| 4 | Saving the settings repeats a usage alert | Backend, UX | `src/main.rs` `apply_settings` | Any save rebuilt `Thresholds` and forgot what had alerted | `463fdc5`: `Thresholds::set_percents` keeps the alerted state |
| 5 | Keep-awake released while Claude works on a reply typed on the notch | Backend | `src/main.rs` `serve_hooks` | `Stop` releases keep-awake; the reply restarts Claude with no `UserPromptSubmit`, so the PC could sleep mid-turn and the working time did not show | `10a806e`: sending a reply marks the session as working |
| 6 | A question stalls Claude while the notch is hidden | Backend, Prod | `src/main.rs` `serve_hooks`, `src/requests.rs` | `AskUserQuestion` waits on its `PreToolUse` hook; behind a fullscreen app (including a fullscreen terminal) the card is invisible, so Claude waited the whole permission wait (120 s by default) | `b353029`: questions are handed back at once and a toast says input is needed |

## Cross-Cutting Concerns

| Concern | Flagged By | Root Cause | Unified Fix |
|---------|-----------|------------|-------------|
| Fullscreen hiding is known to alerts and to `TaskDone`, but not to other features | UX, Backend, Prod | The hidden state lives in `Alert.hidden`; each feature must remember to read it | Issues 2 and 6 fixed case by case. Still open: a question card already on screen when the notch hides (recommendation 2), and the request sound playing while hidden |
| "Request" means three card kinds, but some code treats it as "permission request" | UX, Frontend | `has-request` is shared by permission, question and reply | Issue 1 fixed. Any new request-wide behaviour should check `request-kind` |

## All Recommendations

Not fixed; none is certain enough or cheap enough to change blind.

| # | Recommendation | Lens | File(s) | Fix |
|---|---------------|------|---------|-----|
| 1 | A reply card that comes forward while the pointer already rests on the notch may still auto-dismiss after 8 s: `changed request-id` resets `reply-touched`, and `changed hovered` does not fire again. Needs checking in the live app | UX, Frontend | `ui/notch.slint` | Set `reply-touched = hovered` in `changed request-id` |
| 2 | A question card already showing when the notch hides waits out its hook | Backend | `src/main.rs` (marked `ponytail:`) | Call `settle_questions` on the hide transition |
| 3 | A start-up error other than "already running" is invisible in the release exe, which has no console | Prod, DX | `src/main.rs` `main` | Show a message box for errors from `pipe_name`, `Listener::bind` (not the already-running case) and `Notch::new` |
| 4 | With Antigravity, the pill follows the tightest limit; when that switches to a limit with another reset time, the window key changes and a threshold can alert again | Backend | `src/usage/antigravity.rs`, `thresholds.rs` | Key the window per limit, or alert only on a rising share |
| 5 | The settings window edits two usage alerts; a third in `config.toml` is dropped on save | UX | `src/settings.rs` | Keep the alerts beyond the second when saving |
| 6 | Keep-awake is released on `NeedsInput` (elicitation, agent input) and not held again when Claude resumes without a new prompt | Backend | `src/main.rs` | Needs a hook event that marks the resume; none known |
| 7 | The shortcut hint shows on the buttons even when `RegisterHotKey` failed because another app holds the key | UX | `src/window.rs` `claim_shortcuts` | Return whether both keys were claimed and show the hint only then |
| 8 | Every 30 s the Claude source lists every file under `~/.claude/projects` | Perf | `src/usage/claude_local.rs` `find_logs` | Fine at today's sizes; skip folders whose modified time is older than `HISTORY` if it shows in a profile |
| 9 | The notch still has no keyboard path besides the two shortcuts (known, `docs/AUDIT.md` P1.3) | A11y | `ui/notch.slint` | Out of scope for a bug hunt |

---

# Lens Reports

---

## 1. Design Engineer

**Verdict**: APPROVE

No bugs. Colours come from `Theme`, light mode switches at the opacity crossover so no
palette flashes, every animation honours `reduce-motion`, and type sizes follow the Windows
text scale. No visual states were found missing for the three card kinds, the alert, the
tucked sliver and the open panel.

---

## 2. UX Engineer

**Verdict**: APPROVE WITH CHANGES

Journeys walked: permission request (click and shortcut), question (single, multiple,
several questions), reply card (type, dismiss, timeout, open in app), usage alert, drag to
an edge, settings save, fullscreen app in front.

Critical: issues 1, 2 and 4 above. Recommendations 1, 5 and 7.

Interaction consistency: the 500 ms arming delay covers Allow, Deny, "always allow", options
and "Send answers". "Open in app" and "Dismiss" are not armed, which is right: neither
approves anything.

---

## 3. Frontend Engineer

**Verdict**: APPROVE WITH CHANGES

Slint state is driven by a small set of `in` properties set from Rust in one place per card
(`show_requests`, `show_usage`, `show_alert`). Change callbacks are used for timers and for
telling Rust about shape and request changes.

Critical: issue 1. `answer()` was a public function that decided whatever card was in front,
and `request-shown-changed` fired only on `has-request`, not when the front card changed
kind. Recommendation 1.

Type safety: enums cross the Slint boundary through `From` impls in both directions, and
request ids round-trip through `i32` with a test.

---

## 4. Backend Engineer

**Verdict**: APPROVE WITH CHANGES

Error paths: hook mode prints nothing and exits 0 on every failure, so Claude Code falls back
to its own dialog. The server drops a request when its hook goes away (`peer_closed`). Sources
that fail show "no data" instead of stale numbers.

Resource lifecycle:

| Resource | Acquired | Released | Notes |
|---|---|---|---|
| Pipe instance | `Listener::accept` | `File` drop | A fresh instance replaces each accepted one |
| Security descriptor | `UserOnly::new` | `Drop` (`LocalFree`) | |
| Token handle, snapshot handle | `OwnedHandle` | drop | |
| PDH query | `Query::open` | `Drop` | |
| Power request | `KeepAwake::apply` | `Drop`, `expire` | Issue 5 was a missed acquire |
| Global hotkeys | `claim_shortcuts` | `release_shortcuts` | Issues 1 and 2 were missed releases |
| BCrypt key | `BCryptGenerateSymmetricKey` | `BCryptDestroyKey` before any return | |

Critical: issues 3, 5 and 6. Recommendations 2, 4 and 6.

Locks: `requests`, `alert`, `usages` and `awake` are each held briefly; where two are held
the order is `requests` then `alert`, and `usages` then `alert`. No cycle found.

---

## 5. Structural Engineer

**Verdict**: APPROVE

Dependencies point one way: `main` wires `ui`, `window`, `server`, `usage`, `status`;
`hook` and `server` share only `protocol` and `pipe`; usage sources sit behind the `Source`
trait. `main.rs` is long (about 1500 lines) but is all wiring plus formatting helpers with
tests. No boundary problems that cause bugs.

---

## 6. Repo Maintainer

**Verdict**: APPROVE

Conventions in `CLAUDE.md` hold: fmt, clippy with `-D warnings` and tests are clean,
commits are Conventional Commits without trailers, fixtures are synthetic, `unsafe` blocks
carry `SAFETY` comments, deliberate shortcuts carry `ponytail:` comments. The fixes follow
the same conventions. `docs/PLAN.md` was not changed: these are bug fixes, not plan steps.

---

## 7. Production Readiness

**Verdict**: APPROVE WITH CHANGES

| Failure | Behaviour | OK? |
|---|---|---|
| Notch not running | Hook connects to nothing, exits silently, Claude Code asks itself | Yes |
| Notch crashes mid-request | Pipe closes, hook reads end of stream, exits silently | Yes |
| Hook killed or timed out | Server sees the peer closed within 250 ms and drops the card | Yes |
| Broken `config.toml` | Defaults load; `update` refuses to overwrite | Yes |
| Broken `settings.json` | Install refuses, with a backup on every write | Yes |
| Fullscreen app in front | Alerts become toasts; `TaskDone` settles | Was incomplete: issues 2 and 6 |
| Second instance | Refused through the pipe name, silently | Acceptable |
| Other start-up failure | Silent in release | Recommendation 3 |
| Network down | Status unknown, account falls back to local estimate | Yes |

---

## 8. DX Engineer

**Verdict**: APPROVE

Error messages name the file and say what to do ("fix it first", "run `claude` to sign in
again"). Panics reach `crash.log` and a dialog. The one gap is recommendation 3.

---

## 9. Performance Engineer

**Verdict**: APPROVE

| Work | Interval | Cost |
|---|---|---|
| CPU, RAM, GPU sample | 1 s | Small; GPU counters rest while hidden |
| Fullscreen check | 1 s | One shell call |
| System settings check | 2 s | Two registry reads, one `SystemParametersInfo` |
| Claude logs | 30 s | Directory walk plus appended lines only |
| Codex, Gemini logs | 60 s | Newest file only when changed; today's files |
| Account, Antigravity, status | 5 min | One request or process each |

Animations driven by `animation-tick()` are gated so a still notch does not redraw. Nothing
here is a bug. Recommendation 8.

---

## 10. Accessibility Engineer

**Verdict**: APPROVE WITH CHANGES

Roles and labels exist on the pill, cards, buttons, options and limit bars, and unarmed
buttons report as disabled. Colour is never the only signal. The remaining gap is the known
one: no keyboard path to the notch besides the two opt-in shortcuts (recommendation 9).
Issue 1 mattered here too, since those shortcuts are the only keyboard path and they
misfired on two of three card kinds.
