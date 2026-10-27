# ai-notch

A "Dynamic Island" style notch for Windows 11. A small black pill hangs from the top
edge of the screen and shows:

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

- **Hover** the pill to see RAM, VRAM, tokens, burn rate, the reset countdown and any
  incident on the provider's status page.
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
`PermissionRequest`, `Notification`, `Stop` and `UserPromptSubmit` events. Your other
settings and hooks are left as they are, and running it twice adds nothing twice.
Claude Code picks up the change without a restart.

How a permission request works:

- The notch shows the tool and the **full** command or input, with Allow, Deny and
  "Open in app".
- ai-notch never approves anything by itself. If the notch is not running, if you do
  not answer within the permission wait (120 s by default), or if anything goes wrong,
  Claude Code shows its normal dialog.
- Requests from several sessions queue up on the notch.

The notch talks to the hooks over a named pipe that only your Windows user can open.

While Claude Code is working, ai-notch keeps the PC from going to sleep. It lets go
when Claude finishes or waits for you, and after two hours at the latest.

## Usage data

| Provider | Source | Exact or estimated |
|---|---|---|
| Claude | The usage endpoint behind Claude Code's `/usage` screen, signed in with Claude Code's own login | Exact 5-hour and weekly % |
| Claude | Claude Code's logs in `%USERPROFILE%\.claude\projects` | Tokens and burn rate; estimated reset time when the endpoint is unavailable |
| ChatGPT | Codex CLI sessions in `%USERPROFILE%\.codex\sessions` | Exact, as of your last Codex turn |
| Gemini | Gemini CLI sessions in `%USERPROFILE%\.gemini\tmp` | Estimated: requests today against the daily limit you set |

About the Claude account source:

- It reads the access token from `%USERPROFILE%\.claude\.credentials.json`. It never
  refreshes or changes the login, so it cannot sign Claude Code out, and the token is
  never logged.
- The endpoint is unofficial and may change. When it fails, the notch falls back to
  the local logs and shows an "ESTIMATED" badge.
- The token in that file is refreshed by the Claude Code CLI. If you only use Claude
  Code in the desktop app, the token can stay expired and the notch shows the
  estimate until you run `claude` in a terminal.
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
| `claude_account` | `true` | Read exact Claude usage from the account endpoint |
| `monitor` | primary display | Display the notch sits on, as Windows names it: `monitor = '\\.\DISPLAY2'` (single quotes, so the backslashes stay as they are) |

A longer permission wait reaches Claude Code only after running `--install-hooks`
again, since the hook entry carries its own timeout. A config file with a typo is
ignored, and the settings window will not overwrite it.

## Network and files

ai-notch sends no telemetry. It connects to:

- `api.anthropic.com`, for the Claude account usage (only when that source is on);
- the public status pages `status.claude.com`, `status.openai.com` and
  `status.cloud.google.com`, for the provider shown, every 5 minutes.

It writes:

- `%APPDATA%\ai-notch\config.toml`, when you save settings or pick a provider;
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
private) and under 0.2% CPU when idle. Right after starting it briefly uses more,
around 100 MB, while it reads the last day of Claude Code logs. Slint's software
renderer is used, since the OpenGL renderer took three times the memory for the same
look.

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
