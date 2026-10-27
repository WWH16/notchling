# ai-notch

A "Dynamic Island" style notch for Windows 11. A small black pill sits against an
edge of the screen, the top one to start with, and shows:

- CPU and GPU load, plus RAM and VRAM on hover;
- how much of your Claude, ChatGPT (Codex) or Gemini usage limit is used, when it
  resets, and the provider's status;
- Claude Code permission requests, which you can allow or deny without switching
  windows;
- a short alert when Claude finishes a task or needs your input.

One `.exe`, no installer and no background service. Written in Rust with
[Slint](https://slint.dev).

## Build

You need the Rust toolchain for Windows (`stable-x86_64-pc-windows-msvc`).

```powershell
cargo build --release
```

The program is `target\release\ai-notch.exe`. Copy it to a folder where it will stay,
for example `%LOCALAPPDATA%\Programs\ai-notch\`. The Claude Code hooks point at the
exe's path, so moving it later means installing the hooks again.

## Run

Start `ai-notch.exe`. Only one copy runs at a time.

The pill shows CPU, GPU and a ring with the share of the selected provider's limit.
The ring turns amber at 80% and red at 95%.

- **Hover** the pill to see RAM, VRAM, and the provider's limits as rings with the
  clock time each one resets ("resets 2:20 PM", "resets Fri 7:00 AM"), plus tokens,
  burn rate and any incident on its status page.
- **Drag** it with the left button anywhere, on any display. Let go and it glides
  onto the screen edge nearest the cursor (top, bottom, left or right), flat side
  against it, and expands away from that edge. On the left or right edge it stands
  upright, with its readings stacked. It stays put next time.
- **Right-click** it to pick the provider shown (Claude, ChatGPT or Gemini), open the
  settings, or quit.
- **Click** a "Done" or "Needs your input" alert to bring that Claude Code window to
  the front.

The notch hides while a fullscreen app, game or presentation is in front. Alerts
then arrive as Windows notifications instead.

It does not start with Windows by itself. To start it at sign-in, put a shortcut to
the exe in `shell:startup` (type that into the Run dialog, Win+R).

## Claude Code hooks

Permission requests and task alerts come from Claude Code hooks. To add them:

```powershell
ai-notch.exe --install-hooks
```

This backs up `%USERPROFILE%\.claude\settings.json` to
`settings.json.ai-notch-backup-<time>` in the same folder, then adds ai-notch to the
`PermissionRequest`, `Notification`, `Stop` and `UserPromptSubmit` events, plus
`PostToolUse`, `PostToolUseFailure` and `PermissionDenied` as background hooks that
never slow a tool call down. Your other settings and hooks are left as they are, and
running it twice adds nothing twice. Claude Code picks up the change without a
restart. After updating ai-notch, run it again so new events are added.

How a permission request works:

- The notch shows the tool and the **full** command or input, with Allow, Deny and
  "Open in app". A command too long for the card fades out at the bottom with
  "+N lines"; scroll to read the rest.
- Allow and Deny ignore clicks for half a second after a card appears, filling up
  meanwhile, so a click meant for the window behind cannot answer it.
- With the shortcuts turned on in the settings, Ctrl+Alt+Y allows and Ctrl+Alt+N
  denies the request on the notch. The keys are only taken while a card shows.
- ai-notch never approves anything by itself. If the notch is not running, if you do
  not answer within the permission wait (120 s by default), or if anything goes wrong,
  Claude Code shows its normal dialog.
- Requests from several sessions queue up on the notch.
- If you answer in Claude Code instead, the card goes away by itself once the tool has
  run, failed or been denied, or when that session's turn ends.

The notch talks to the hooks over a named pipe that only your Windows user can open.

While Claude Code is working, ai-notch keeps the PC from going to sleep. It lets go
when Claude finishes or waits for you, and after two hours at the latest.

## Usage data

| Provider | Source | Exact or estimated |
|---|---|---|
| Claude | The usage endpoint behind Claude Code's `/usage` screen, signed in with the Claude desktop app's login or Claude Code's own | Exact 5-hour and weekly %, with their reset times |
| Claude | Claude Code's logs in `%USERPROFILE%\.claude\projects` | Tokens and burn rate; an estimated reset time when the endpoint is unavailable |
| ChatGPT | Codex CLI sessions in `%USERPROFILE%\.codex\sessions` | Exact, as of your last Codex turn |
| Gemini | Gemini CLI sessions in `%USERPROFILE%\.gemini\tmp` | Estimated: requests today against the daily limit you set |

About the Claude account source:

- It follows the account the Claude desktop app is signed in to: it reads the app's
  login, which Windows keeps encrypted for your user, and never refreshes or writes
  it. Switch accounts in the app and the notch follows within 30 seconds.
- Without the desktop app it uses the login in `%USERPROFILE%\.claude\.credentials.json`.
  Only the Claude Code CLI refreshes that file. So when the stored token has expired,
  ai-notch refreshes it the same way Claude Code does (`platform.claude.com`, Claude
  Code's client id) and writes the new tokens back, leaving every other field as it
  was. The CLI then carries on with them.
- If the CLI refreshes at the very same moment, the CLI may ask you to sign in once
  (`claude /login`). The desktop app is not affected.
- Tokens are never logged or shown in errors.
- The endpoint is unofficial and may change. When it fails, the notch falls back to
  the local logs: the ring stays empty, tokens and burn rate still show, and an
  "ESTIMATED" badge appears.
- You can turn this source off in the settings.

Usage alerts appear at 80% and 95% of a limit, once per usage window.

## Settings

Right-click the notch and choose **Settings…**, or edit
`%APPDATA%\ai-notch\config.toml`. Every key is optional:

| Key | Default | Meaning |
|---|---|---|
| `provider` | `"claude"` | Usage shown on the notch: `"claude"`, `"chatgpt"` or `"gemini"` |
| `usage_alerts` | `[80, 95]` | Percentages that raise an alert |
| `hook_timeout_secs` | `120` | How long a permission request waits for you (5 to 580) |
| `gemini_daily_requests` | `1000` | Your Gemini CLI daily request limit |
| `claude_account` | `true` | Read exact Claude usage from the account endpoint, refreshing Claude Code's login when needed |
| `request_shortcuts` | `false` | Ctrl+Alt+Y allows and Ctrl+Alt+N denies the request on the notch |
| `edge` | `"top"` | The screen edge the notch sticks to: `"top"`, `"bottom"`, `"left"` or `"right"`; set by dragging |
| `position` | `0.5` | Where along that edge it sits, from 0 to 1 (left to right, or top to bottom on a side edge); set by dragging |
| `monitor` | primary display | Display the notch sits on, as Windows names it: `monitor = '\\.\DISPLAY2'` (single quotes, so the backslashes stay as they are) |

A longer permission wait reaches Claude Code only after running `--install-hooks`
again, since the hook entry carries its own timeout. A config file with a typo is
ignored, and the settings window will not overwrite it.

## Network and files

ai-notch sends no telemetry. It connects to:

- `api.anthropic.com`, for the Claude account usage, and `platform.claude.com`, to
  refresh Claude Code's login when it has expired (only when that source is on);
- the public status pages `status.claude.com`, `status.openai.com` and
  `status.cloud.google.com`, for the provider shown, every 5 minutes.

It writes:

- `%APPDATA%\ai-notch\config.toml`, when you save settings, pick a provider or move
  the notch;
- `%USERPROFILE%\.claude\.credentials.json`, the new tokens only, when it refreshes
  Claude Code's expired login;
- `%LOCALAPPDATA%\ai-notch\crash.log`, only if it crashes;
- `%USERPROFILE%\.claude\settings.json` and its backups, when you install or remove
  the hooks;
- the registry key `HKCU\Software\Classes\AppUserModelId\ai-notch`, the first time it
  shows a Windows notification. Windows only shows notifications from apps it knows.

## Uninstall

1. Remove the hooks while the exe is still there:

   ```powershell
   ai-notch.exe --uninstall-hooks
   ```

2. Quit ai-notch (right-click, **Quit ai-notch**) and delete the exe, plus its
   shortcut in `shell:startup` if you made one.
3. Delete the settings and the crash log:

   ```powershell
   Remove-Item -Recurse -ErrorAction SilentlyContinue "$env:APPDATA\ai-notch", "$env:LOCALAPPDATA\ai-notch"
   ```

4. Remove the notification registration:

   ```powershell
   reg delete "HKCU\Software\Classes\AppUserModelId\ai-notch" /f
   ```

5. Optionally delete the `settings.json.ai-notch-backup-*` files in
   `%USERPROFILE%\.claude`.

## Footprint

Release build on the development machine: a 13 MB exe, about 35 MB of RAM (14 MB
private) and under 0.2% CPU when idle, startup included. Slint's software renderer is
used, since the OpenGL renderer took three times the memory for the same look.

## Development

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Design notes live in `docs/`: [SPEC.md](docs/SPEC.md) is the original brief,
[PLAN.md](docs/PLAN.md) the build plan and deviations, and
[FINDINGS.md](docs/FINDINGS.md) what the data sources and hooks actually look like.
Test fixtures in `tests/fixtures/` are synthetic.
