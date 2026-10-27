# ai-notch build plan

Implementation plan for [SPEC.md](SPEC.md). Each numbered step ends in one commit.

Status: `[x]` done, `[ ]` not done. Updated at the end of every step.

## Rules for every step

- Load the `/beautiful-rust:rust-best-practices` skill before writing or changing any
  Rust code, and follow it. Its final checklist must pass before each commit.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass before
  each commit.
- One commit per step, Conventional Commits messages.
- Author is the repository owner only. No `Co-Authored-By` trailers, no "Generated
  with" lines.
- Local git only. No remote.

## Phase 0: Setup (done)

- [x] **0.1** Install Rust (rustup, `stable-x86_64-pc-windows-msvc`). No commit.
- [x] **0.2** Git identity: global config (John Ansley Rocel). No commit.
- [x] **0.3** `cargo new`, own git repo, `.gitignore`, `docs/SPEC.md`, `docs/PLAN.md`.
  Commit `chore: scaffold ai-notch project`.

## Phase 0.5: Early checks (done, see [FINDINGS.md](FINDINGS.md))

Run before any UI work. If a check fails, update Phases 2-5 before writing their code.

- [x] **V1** Hooks in the desktop app's Code tab. Back up `~/.claude/settings.json`, add a
  temporary hook that appends each event to a log file, trigger a permission prompt, a
  finished task and a "needs input" notification, save the payload formats as synthetic
  test fixtures, then remove the temporary hook.
- [x] **V2** Codex: run one Codex prompt and check the session file for `rate_limits` fields.
  At planning time `~/.codex/sessions` existed but held no session files.
- [x] **V3** Gemini: map where `~/.gemini/tmp` stores sessions and token counts.
- [x] **V4** Claude account usage: one request to the endpoint behind Claude Code's
  `/usage` screen, using the token in `~/.claude/.credentials.json`. Read the token
  only; never print, log or refresh it (a refresh could sign Claude Code out).

Record results in `docs/FINDINGS.md`. Commit
`docs: record hook payloads and data source findings`.

## Phase 1: Notch window and system stats (done)

- [x] **1.1** Slint, `build.rs`, lints table in `Cargo.toml`, release profile (LTO, strip).
  Commit `chore: add slint and build setup`.
- [ ] **1.2** Moved to Phase 2 (step 2.0): no Phase 1 feature has a setting yet. The
  first setting is the hook timeout.
- [x] **1.3** `ui/notch.slint`: black notch hanging from the top edge, idle and hover
  states, ~200 ms ease-out morph, color tokens. Request and alert states are built in
  2.4 and 2.5 with their data. No embedded font: Segoe UI digits are already tabular.
  Commit `feat: notch pill ui with morph states`.
- [x] **1.4** `window.rs`: no frame, transparent, topmost, hidden from taskbar and Alt+Tab,
  never steals focus, top-center placement, DPI-correct.
  Commit `feat: notch window placement and styles`.
- [x] **1.5** Window region follows the shape (Slint `changed` handlers, no polling); hover
  comes from Slint; clicks outside the shape pass through to the windows below.
  Commit `feat: hover detection and click-through`.
- [x] **1.6** `system.rs`: CPU and RAM via `sysinfo`, every second.
  Commit `feat: cpu and ram stats`.
- [x] **1.7** `gpu.rs`: GPU % and VRAM via PDH, Task Manager aggregation rule, unit test.
  Commit `feat: gpu usage via pdh`.
- [x] **1.8** Wire stats into the UI: CPU and GPU when idle, RAM and VRAM on hover.
  Right-click menu with Quit, software renderer.
  Commit `feat: right-click quit menu and software renderer`.

Phase check: release build uses about 40 MB RAM or less and under 1% CPU when idle.
Result: 27 MB working set, 10.5 MB private, 0.12% CPU, 9.2 MB exe, with Slint's
software renderer. The OpenGL renderer used 95 MB and 0.94% CPU for the same look.

## Phase 2: Claude Code hooks (built; live test pending)

- [x] **2.0** `config.rs`: TOML at `%APPDATA%\ai-notch\config.toml`, defaults when
  missing; first field is the hook timeout. Commit `feat: config file`.
- [x] **2.1** `protocol.rs`: pipe messages (permission request, stop, notification, prompt
  submitted) as JSON lines, round-trip tests. Capture a real `Stop` payload first. Commit `feat: hook pipe protocol`.
- [x] **2.2** `pipe.rs`: named pipe restricted to the current Windows user, rejects remote
  clients, first-instance only. Integration test.
  Commit `feat: secure named pipe server and client`.
- [x] **2.3** `--hook` mode: read the event from stdin, send it to the notch, wait for a
  click up to a configurable timeout (default 120 s), print the exact JSON format from
  V1. If the notch is not running or time runs out, exit with no decision so the normal
  dialog appears. Commit `feat: hook mode`.
- [x] **2.4** Request card: queue across sessions, full command with scrolling, Allow and
  Deny. Commit `feat: permission request card`.
- [x] **2.5** Done and needs-input pulses; click focuses the source window, found by walking
  the hook's parent processes. Commit `feat: task alerts with click-to-focus`.
- [x] **2.6** `awake.rs`: keep the PC awake from prompt submitted until stop or "needs input",
  2-hour safety release (tool calls without permission prompts send no events, so long
  autonomous runs are silent). Commit `feat: keep pc awake while claude works`.
- [x] **2.7** `--install-hooks` / `--uninstall-hooks`: back up `settings.json`, merge or
  remove only our entries, idempotent, tests on the merge.
  Commit `feat: install and uninstall hooks`.

Manual tests: notch closed, timeout, deny, two sessions at once. Every failure case
must show the normal dialog.
Done with the release exe and synthetic payloads: allow, deny, open in app, queue of
two, hook killed mid-wait, 120 s timeout, alerts, click-to-focus, installer on a copy
of settings.json. Still to do: install for real and try it in live Claude Code
sessions (needs the user, since it changes the global settings).

## Phase 3: Claude usage, local source

- [x] **3.1** `usage/mod.rs`: shared `Usage` struct, `Provider` enum, background poller. A
  broken source returns an error and never crashes the app.
  Commit `feat: usage model and poller`.
- [x] **3.2** `claude_local.rs`: incremental reads (per-file offsets), message dedupe,
  tokens per model, 5-hour window, burn rate. Synthetic fixtures only, never real logs.
  Commit `feat: claude code log parser`.
- [ ] **3.3** UI: reset countdown, tokens, burn rate, "estimated" badge.
  Commit `feat: show claude local usage`.

## Phase 4: Claude usage, account source

- [ ] **4.1** Read the OAuth token, parse 5-hour and weekly % and reset times. Token never
  appears in logs or `Debug` output. Commit `feat: claude account usage`.
- [ ] **4.2** Fallback: pasted claude.ai session token stored in Credential Manager.
  Commit `feat: session token fallback`.
- [ ] **4.3** Merge sources: account % first, local for details, estimated badge on
  fallback, with a test. Commit `feat: merge claude usage sources`.

## Phase 5: ChatGPT, Gemini, provider selector

- [ ] **5.1** Codex parser, per V2. Commit `feat: codex usage source`.
- [ ] **5.2** Gemini parser: requests per day against a configured daily limit, always
  estimated. Commit `feat: gemini usage source`.
- [ ] **5.3** Provider selector in the right-click menu and config, accent color per
  provider. Commit `feat: provider selector`.

## Phase 6: Extras

- [ ] **6.1** Status dot from each provider's public status JSON every 5 minutes; verify
  URLs at this step. Commit `feat: provider status dot`.
- [ ] **6.2** Alerts at 80% and 95%, once per usage window.
  Commit `feat: usage threshold alerts`.
- [ ] **6.3** Hide during fullscreen apps via `SHQueryUserNotificationState`.
  Commit `feat: hide during fullscreen`.
- [ ] **6.4** Settings window: monitor picker, thresholds, hook timeout, account source
  toggles, token paste field. Commit `feat: settings window`.
- [ ] **6.5** Windows toast only while the notch is hidden.
  Commit `feat: toast fallback when hidden`.

## Phase 7: Release

- [ ] **7.1** README (install, hooks, uninstall), release build, size and RAM check, local
  tag `v0.1.0`. Commit `docs: readme and v0.1.0`.

## Deviations from SPEC.md

- No `nvml-wrapper`: PDH provides VRAM, and the design shows no GPU temperature.
- `ureq` instead of `reqwest` + `tokio`: a few blocking HTTP calls need no async runtime.
- Right-click menu added: needed to quit and switch providers.
- `--install-hooks` added: edits `settings.json` safely with a backup.
- One exe for app and `--hook` mode. If V1 shows stdin/stdout problems with the GUI
  subsystem, split the hook into a second binary in the same crate.
- Start with Windows: not included; add later if wanted.
- Slint software renderer instead of OpenGL (femtovg): same look, a third of the RAM.
- Right-click menu is a native Win32 popup: a Slint popup would be clipped by the
  window region.
