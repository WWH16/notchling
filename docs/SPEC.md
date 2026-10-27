# Project: Windows AI Notch (Rust)

Build a "Dynamic Island"-style notch for Windows 11: a small black pill at the top
center of the screen that monitors system load and AI usage (Claude, ChatGPT, Gemini),
and lets me approve Claude Code permission requests without switching windows.

## Stack
- Rust, single .exe (no installer runtime).
- UI: Slint (.slint files for UI, Rust for logic).
- Windows APIs: `windows` crate (named pipes, PDH counters, window styles,
  SetThreadExecutionState).
- CPU/RAM/processes: `sysinfo`.
- GPU utilization (any vendor): PDH counter `\GPU Engine(*)\Utilization Percentage`.
  Optional NVIDIA extras (temp, VRAM) via `nvml-wrapper`.
- Secrets: Windows Credential Manager via `keyring`. Never store tokens in plain files.
- Config: TOML in %APPDATA%\ai-notch\config.toml.

## Window behavior
- Borderless, topmost, transparent background, not in taskbar/Alt+Tab
  (WS_EX_TOOLWINDOW, WS_EX_LAYERED).
- Only the pill receives mouse input; the transparent area around it is click-through.
- Auto-hide when a fullscreen app/game is in the foreground.
- Monitor picker in settings.

## Design
- Solid black pill, no glass, no gradients. White/grey text.
- One accent color per provider: Claude orange, ChatGPT green, Gemini blue.
- Tabular (monospaced) numerals so digits don't jitter.
- Thin progress rings/bars.
- One shape that morphs between states, ~200ms ease-out, no bounce:
  1. Idle: small pill showing CPU %, GPU %, selected AI usage %.
  2. Hover: wider, shows RAM, VRAM, reset countdown, burn rate, provider status dot.
  3. Request: tall card showing tool + FULL command text, Allow / Deny buttons.
  4. Done/Attention: brief green pulse ("Task done") or accent pulse ("Claude needs input").
     Click focuses the Claude window.
- Provider selector: choose which AI the idle pill shows.

## Claude Code integration (hooks)
- The same exe runs in hook mode: `ai-notch.exe --hook`.
- Register it in `~/.claude/settings.json` for these hook events:
  - `PermissionRequest`: forward request to the notch via a local named pipe, wait for
    my click, print the allow/deny decision in the format Claude Code expects.
  - `Stop`: "task done" alert.
  - `Notification`: "Claude needs input" alert.
- Check the current Claude Code hooks documentation for the exact input/output JSON.
  Do not guess the format.
- Safety rules (mandatory):
  - Always show the full command, never just the tool name.
  - Never auto-approve. On timeout or if the notch isn't running, exit with no
    decision so the normal Claude Code dialog appears.
  - Named pipe is local and restricted to the current Windows user.
- Keep-awake: while any Claude Code session is working, call SetThreadExecutionState
  to prevent sleep; release when done.

## Usage data sources
Each provider is a module returning the same struct:
`{ used_percent: Option<f32>, resets_at: Option<DateTime>, tokens, estimated: bool }`.
If the exact source fails, fall back to local data and show an "estimated" badge.
The app must never crash because a source broke.

- Claude
  - Local: parse Claude Code logs in `~/.claude/projects/**/*.jsonl` (tokens per model,
    burn rate, 5-hour window).
  - Account (exact % + reset time, includes claude.ai app usage): unofficial. First
    try the data Claude Code's own `/usage` screen uses, via its existing login.
    Fallback: user pastes a claude.ai session token, stored in Credential Manager.
    Never log tokens.
- ChatGPT (Go plan, no API key)
  - Codex CLI logs in `~/.codex/sessions/`. Recent versions include 5h/weekly
    rate-limit % and reset times; use them if present, otherwise estimate.
  - Skip ChatGPT app usage (no endpoint).
- Gemini (Google AI Plus, no API key)
  - Gemini CLI logs in `~/.gemini/`: request/token counts today vs. known daily limit.
    Always marked estimated.
  - Skip Gemini app usage (no endpoint).

## Extras
- Provider status dot: poll the public status JSON for Claude, OpenAI, and Google
  every few minutes; red dot during incidents.
- Threshold alerts at 80% and 95% (shown on the notch, not as Windows toasts).
- Windows toast fallback only when the notch is hidden (e.g. fullscreen game).

## Build order (ship each step working before the next)
1. Notch window + CPU/GPU/RAM + hover expand + morph animation.
2. Hook mode + named pipe: permission approvals, done/needs-input alerts, keep-awake.
3. Claude usage, local source + reset countdown.
4. Claude usage, account source (exact %).
5. ChatGPT (Codex) and Gemini (CLI) sources + provider selector.
6. Status dot, threshold alerts, fullscreen auto-hide, monitor picker.

## Verify early (before building UI on top of it)
- Do Claude Code hooks from `~/.claude/settings.json` fire in the Claude desktop
  app's Code tab, not just the terminal CLI? Test with a hook that only logs to a file.
- Do my Codex session logs contain rate-limit fields?
- Where exactly does Gemini CLI store usage/session data on this machine?

## Out of scope
Media controls, weather, calendar, clipboard history, history charts (later),
ChatGPT/Gemini approval forwarding (later).
