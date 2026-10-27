# Phase 0.5 findings

Checked on 2026-10-26 on the development machine. Synthetic samples of every format
live in `tests/fixtures/`. No real payloads, tokens or prompts are committed.

## V1: Claude Code hooks in the desktop app (confirmed)

- Hooks from `.claude/settings.local.json` fire inside the Claude desktop app's Code
  tab. The file watcher picked up the new hooks mid-session; no restart was needed.
- Hook commands run through Git Bash on this machine (Git is installed). Quote the
  exe path, since the user profile path contains spaces.
- `PermissionRequest` fires before the dialog. Each one is followed by a
  `Notification` with `notification_type: "permission_prompt"`, which arrives only
  after the `PermissionRequest` hook returns. The notch must ignore that duplicate
  notification type and show its own request card instead.
- The temporary probe was a PowerShell script and blocked each prompt for about 5 to 6
  seconds, because of PowerShell startup. The real hook is a native exe, which keeps
  this delay small.
- Exiting 0 with no output gave the normal permission flow every time.

### Input fields seen

Common to every event: `session_id`, `transcript_path`, `cwd`, `hook_event_name`,
`prompt_id`, `scratchpad_dir`.

- `PermissionRequest`: `tool_name` (`Bash`, `PowerShell`, ...), `tool_input`
  (`command`, `description` for shell tools; other tools carry their own fields),
  `permission_mode`, `effort` (`{"level": ...}`), `permission_suggestions` (list of
  `{type: "addRules", rules, behavior, destination}`).
- `Notification`: `message`, `notification_type` (`permission_prompt`,
  `idle_prompt`, `agent_needs_input`, ...), optional `title`.
- `UserPromptSubmit`: `prompt`, `permission_mode`, `session_title`. It also fires
  for messages the user sends while Claude is mid-turn.
- `Stop`: `stop_hook_active`, `last_assistant_message`, `permission_mode`, `effort`,
  `background_tasks`, `session_crons` (captured in step 2.1).

### Output (from the hooks docs)

Allow:

```json
{"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}}
```

Deny, with a reason shown to Claude:

```json
{"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": "Denied from ai-notch"}}}
```

No decision: exit 0 with no stdout. Exit code 2 is not a deny on this event.

Hook `timeout` is in seconds; command hooks default to 600. The installer sets an
explicit timeout slightly above the notch's own wait, so the notch always gives up first.

## V2: Codex CLI (confirmed, exact %)

- Sessions: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl.zst`. The files are
  **zstd-compressed**, so the parser needs the `zstd` crate.
- `event_msg` lines with `payload.type == "token_count"` carry `payload.rate_limits`:
  `primary` / `secondary` objects with `used_percent`, `window_minutes` and
  `resets_at` (Unix seconds), plus `plan_type`.
- This account reports `plan_type: "go"` (ChatGPT Go, not Pro) with a single
  43200-minute (30-day) primary window and no secondary window.
- Latest session on this machine: 2026-10-15. The notch shows the last known value
  with its age.
- `~/.codex/hooks.json` exists, so Codex has its own hook system. That matters for
  the later ChatGPT approval-forwarding idea and is out of scope for now.

## V3: Gemini CLI (confirmed, estimate only)

- Sessions: `~/.gemini/tmp/<project>/chats/session-*.jsonl`.
- Lines with `type: "gemini"` carry `model`, `timestamp` and
  `tokens: {input, output, cached, thoughts, tool, total}`.
- `~/.gemini/tmp/<project>/logs.json` lists user prompts (`sessionId`, `messageId`,
  `type`, `message`, `timestamp`).
- No quota or limit field exists anywhere, so the Gemini % stays an estimate against a
  configured daily request limit.
- Latest CLI session on this machine: 2026-06-18.
- `~/.gemini` also holds Antigravity data, which is out of scope.

## V4: Claude account usage (confirmed, exact %)

- `GET https://api.anthropic.com/api/oauth/usage` with
  `Authorization: Bearer <claudeAiOauth.accessToken>` and
  `anthropic-beta: oauth-2025-04-20` returned 200.
- Token source: `~/.claude/.credentials.json`, key `claudeAiOauth`. It also holds
  `expiresAt` (ms), `subscriptionType` (`pro` here) and a `refreshToken`. ai-notch
  never uses the refresh token. An expired token means "estimated" until Claude Code
  refreshes it.
- Use the `limits` array: `kind` (`session`, `weekly_all`), `percent`, `resets_at`
  (ISO 8601 or null), `is_active`. `five_hour` and `seven_day` carry the same data as
  `utilization` and are the fallback.
- `seven_day_breakdown.rows` splits weekly usage by `claude_code`, `chat`, `cowork`
  and `other`, which works for the hover view.
- Many other keys are internal codenames. Ignore them, and parse with every field
  optional.
- The claude.ai session-token fallback (step 4.2) may be unnecessary. Revisit before
  building it.
- Checked again at step 4.1 (2026-10-27): the token in `.credentials.json` had expired
  hours earlier while Claude Code kept working in the desktop app's Code tab. The
  desktop app does not seem to refresh that file; the terminal CLI does. So the exact %
  is only there while the CLI login is fresh, and the local estimate covers the rest.
  Without a token the endpoint answers 401.

## Plan changes

- 2.3: the hook output format is confirmed as above.
- 2.5: ignore `Notification` events whose type is `permission_prompt`.
- 4.2: dropped at Phase 4.
- 5.1: add the `zstd` dependency; Codex gives an exact %, not an estimate.
- Plans: Claude Pro, ChatGPT Go. Update SPEC.md wording when Phase 5 starts.
