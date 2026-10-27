# notchling build plan

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
  Commit `chore: scaffold notchling project`.

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

- [x] **2.0** `config.rs`: TOML at `%APPDATA%\notchling\config.toml`, defaults when
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
- [x] **3.3** UI: reset countdown, tokens, burn rate, "estimated" badge.
  Commit `feat: show claude local usage`.

## Phase 4: Claude usage, account source

- [x] **4.1** Read the OAuth token, parse 5-hour and weekly % and reset times. Token never
  appears in logs or `Debug` output. Commit `feat: claude account usage`.
- [x] **4.2** Dropped: the account source works with Claude Code's own login (V4), so
  no pasted claude.ai session token and no Credential Manager. When that login is
  expired the local estimate covers it. No commit of its own.
- [x] **4.3** Merge sources: account % first, local for details, estimated badge on
  fallback, with a test. Commit `feat: merge claude usage sources`.

## Phase 5: ChatGPT, Gemini, provider selector

- [x] **5.1** Codex parser, per V2. Commit `feat: codex usage source`.
- [x] **5.2** Gemini parser: requests per day against a configured daily limit, always
  estimated. The day runs midnight to midnight Pacific time, when Google's daily quotas
  reset; the limit is `gemini_daily_requests` in the config (default 1000). Commit `feat: gemini usage source`.
- [x] **5.3** Provider selector in the right-click menu and config, accent color per
  provider. Commit `feat: provider selector`.

## Phase 6: Extras

- [x] **6.0** Panic hook, added after an unexplained exit in a Phase 5 screen test: every
  panic goes to `%LOCALAPPDATA%\notchling\crash.log`, and a panic that stops the notch
  also shows a message box. Hook mode writes the note only, so it never blocks Claude
  Code. Commit `feat: show and record panics`.
- [x] **6.1** Status dot from each provider's public status JSON every 5 minutes; verify
  URLs at this step. Only the shown provider is checked, and right away on a switch.
  The hover row shows an amber "Degraded" or red "Outage" dot; a healthy status shows
  nothing. URLs are in FINDINGS. Commit `feat: provider status dot`.
- [x] **6.2** Alerts at 80% and 95%, once per usage window.
  Any provider with a known share alerts, shown or not; amber below the last threshold,
  red at it. Thresholds are `usage_alerts` in the config. Only the main window (5-hour,
  30-day or daily) alerts, not the weekly one. Commit `feat: usage threshold alerts`.
- [x] **6.3** Hide during fullscreen apps via `SHQueryUserNotificationState`.
  Checked every second; busy, D3D fullscreen and presentation mode hide the window
  natively, since hiding Slint's last window would end its event loop.
  Commit `feat: hide during fullscreen`.
- [x] **6.4** Settings window: monitor picker, thresholds, hook timeout, account source
  toggle. Opened from the right-click menu; also edits the Gemini daily limit. Saving
  moves the notch and resets the alerts at once; the usage sources read their settings
  from the config on every poll. A longer hook timeout needs `--install-hooks` again.
  Commit `feat: settings window`.
- [x] **6.5** Windows toast only while the notch is hidden.
  Done, needs-input and usage alerts become toasts while a fullscreen app hides the
  notch. Toasts need an app identity, so the first one registers
  `HKCU\Software\Classes\AppUserModelId\notchling` (remove it on uninstall). Windows'
  own "do not disturb during games" may send them straight to the notification centre.
  Commit `feat: toast fallback when hidden`.

## Phase 7: Release

- [x] **7.1** README (install, hooks, uninstall), release build, size and RAM check, local
  tag `v0.1.0`. Commit `docs: readme and v0.1.0`.
  Result: 12.97 MB exe; 35.1 MB working set and 13.6 MB private when idle, 0.18% CPU
  over 30 s. Startup peaks at 97.9 MB while the first scan reads the last day of Claude
  Code logs whole. The live hook test from Phase 2 is still open.
- [x] **7.2** Read Claude Code logs line by line instead of whole files. Startup peak
  97.9 MB to 35.2 MB working set; idle unchanged at about 35 MB, and the first scan
  went from 174 ms to 96 ms. Commit `fix: read claude logs line by line`.
- [x] **7.3** Feedback round after v0.1.0: Claude shows a % instead of tokens (the
  account source refreshes Claude Code's expired login, see FINDINGS); reset times as
  clock times, the weekly reset too; rings for each limit; hover opens and closes
  after a short delay and the window region changes once per morph, not every frame;
  drag the notch along the top edge, remembered as `position`.
  Commit `feat: rings, reset times, drag and login refresh`.
- [x] **7.4** Fixes from live use: a request answered in Claude Code's own dialog no
  longer stays on the notch (background `PostToolUse`, `PostToolUseFailure` and
  `PermissionDenied` hooks settle it, and `Stop` clears the session); a Windows caption
  no longer appears over the notch after the right-click menu. Needs
  `--install-hooks` again. Commit `fix: settle requests answered in claude code`.
- [x] **7.5** The title bar still showed after the right-click menu: Windows paints it
  on activation whatever the styles say. The notch now skips that painting in its own
  window procedure, and the menu belongs to a hidden helper window. Checked on the live
  notch with the menu open and after it closed.
  Commit `fix: stop windows painting a title bar over the notch`.
- [x] **7.6** Drag the notch anywhere, onto any display. Dropped within 32 px of the
  top or bottom edge it sticks there with its flat side against the edge; elsewhere it
  floats as a fully rounded pill. In the lower half it grows upward. Saved as
  `monitor`, `position` and `position_y`. Commit `feat: drag the notch anywhere`.
- [x] **7.7** No floating: let go, the notch sticks to the edge nearest the cursor,
  top, bottom, left or right, flat side against it, and grows away from it. While
  dragged it is a round pill. Saved as `edge` and `position`; an old `position_y` is
  read once to pick top or bottom. Commit `feat: stick the notch to the nearest edge`.
- [x] **7.8** Let go, the notch glides onto the edge (260 ms, easing out) from where it
  was dropped, morphing its shape and corners on the way. On the left or right edge it
  stands upright: CPU, GPU and the usage ring stacked in the idle pill, and a tall hover
  panel. Requests and alerts stay wide. Commit `feat: glide onto the edge, upright on
  the sides`.
- [x] **7.9** Follow the account the Claude desktop app is signed in to. Switching
  accounts there left `.credentials.json` alone, so the notch kept showing the old
  account. The account source now reads the desktop app's own login first (read only,
  never refreshed) and falls back to Claude Code's; a switch is noticed on the next
  30 s poll instead of after 5 minutes. Checked live after a switch: 9% on the new
  account. Commit `fix: follow the claude desktop app's account`.

## Phase 8: Audit fixes and features (see [AUDIT.md](AUDIT.md))

- [x] **8.1** Harden the permission card: an arming delay on Allow and Deny, the
  overflow fade with "+N lines", accessible roles and labels, and a global shortcut.
  Allow and Deny wait 500 ms, filling up meanwhile. Ctrl+Alt+Y and Ctrl+Alt+N are
  opt-in (`request_shortcuts`), claimed only while a card shows and ignored while the
  notch hides. Checked offscreen with the software renderer: the sweep, the armed
  buttons and "+7 lines" on a 14-line command. Commit `feat: harden the permission
  card`.
- [x] **8.2** Click the pill to toggle the hover panel, and a longer hover delay.
  A click opens the panel and keeps it open; the next click closes it until the
  pointer leaves. Hover opens after 300 ms instead of 120 ms. A drag now starts past
  Windows' drag threshold, so a click with a shaky hand stays a click. Commit
  `feat: click the notch to open its panel`.
- [x] **8.3** Follow Windows "Show animations". Read through
  `SPI_GETCLIENTAREAANIMATION` every 2 s. Off, the morphs, corner easing, bar growth,
  alert pulse and glide stop; the fades stay. Checked offscreen: 400 ms into a hover
  the panel is mid-morph with animations and full size without. Commit
  `feat: follow windows' show animations setting`.
- [x] **8.4** Tuck mode on the side edges; check and fix the upright panel width.
  Opt-in `tuck`: on a side edge the idle notch is a 6 x 96 px sliver with a mark in
  the ring's colour, and unfolds while pointed at. The upright panel is 230 px wide
  instead of 210; offscreen, "Claude · Degraded" and the ESTIMATED badge now fit with
  room to spare. Commit `feat: tuck the notch into side edges`.
- [x] **8.5** Polish: colours into `Theme`, "2 more", the Consolas fallback, label sizes.
  Every colour and type size is a `Theme` token; nothing is below 10 px (labels 10 to
  11 px, the badge 9 to 10 px). Commands use Cascadia Mono where it is installed, else
  Consolas. Windows' text size (`HKCU\SOFTWARE\Microsoft\Accessibility`,
  `TextScaleFactor`) scales the text and the shapes holding it, and the window is
  placed again when its size changes. A short command now starts at the top of its
  box instead of the middle. Checked offscreen at 100% and 150%. Commit
  `feat: theme tokens and windows text size`.
- [x] **8.6** Live Claude activity in the idle pill. From the keep-awake tracker:
  while sessions work, the pill grows by a dot and the longest session's time ("12m",
  "2 · 1h 05m"); upright, the count stacks under the time as "×2". Ticks every 15 s.
  Commit `feat: show claude's working time on the notch`.
- [ ] **8.7** Limit forecast in the hover panel.
- [ ] **8.8** "Start at sign-in" toggle in Settings.
- [ ] **8.9** Investigate "Always allow" through the hook API; build it if supported.
- [ ] **8.10** All providers in the hover panel; double-click to reset placement.
- [x] **8.11** Redesign the notch, austere: a tinted near-black body that flows out of
  its edge in concave shoulders (14 px, folding away while dragged, included in the
  window region); the provider's logo inside the ring of its limit as the mark; usage
  leads the panel, system readings sit under a hairline. No pulsing or decorative
  dots, no capitals badge ("estimated" instead) and no middle-dot strings, so tokens
  and burn rate, alert texts and the card header are separate pieces. Logos are the
  publishers' paths: Simple Icons 16.36.0 (Claude, Gemini) and LobeHub Icons 1.101.0
  (OpenAI). Checked offscreen in every state and edge. Commit
  `feat: redesign the notch with provider logos`.
- [x] **8.12** The hover panel follows each provider's own limits. `Usage` carries a
  list of named limits, grouped by model where the provider groups them: Claude every
  limit its endpoint reports ("Current session", "Weekly (all models)", per-model
  weekly ones); Codex each window named by its length (5-hour, weekly, monthly);
  Gemini the Antigravity CLI's `/usage` (each model group's 5-hour and weekly limits,
  every 5 minutes), with the Gemini CLI estimate as the fallback. Each limit is a bar,
  green below 80% used, amber to 95%, red above, with its share used and left and its
  reset; the idle ring uses the same colours. The panel sizes to its content, wide
  lines pair limits up. Hover opens the panel at once; a click pins it. Checked
  offscreen for all three providers on the top and left edges. Commit
  `feat: per-provider limits in the hover panel`.
- [x] **8.13** The request card shows what Claude says a call does, the tool input's
  `description` that Claude Code puts above the command ("Read hook fixtures and
  install config"), on its own line under the title. Commit
  `feat: show claude's description on the request card`.
- [x] **8.14** Answer Claude's multiple-choice questions on the notch. A `PreToolUse`
  hook, matched to `AskUserQuestion` only, sends the questions; the card shows one at
  a time ("1 of 2") with its options as buttons, ticks for a multiple-choice one and
  Done. The answers go back as `permissionDecision: "allow"` with `updatedInput`
  echoing the questions plus `answers` (labels joined with ", "), as the hooks docs
  describe. "Open in app", a timeout or no notch leave the question to Claude Code;
  free-text "Other" answers stay there too. Options share the 500 ms arming delay.
  Needs `--install-hooks` again. Commit `feat: answer claude's questions on the notch`.
- [x] **8.15** Reply to Claude from the notch. The Stop hook forwards
  `last_assistant_message` and waits; the card shows Claude's last message, scrolling
  when long, with a reply box. Enter or Send returns the reply as
  `hookSpecificOutput.additionalContext`, which keeps the conversation going without a
  hook error; Claude Code's 8-continuation cap still applies. A card nobody touches
  closes after 8 s and Claude stops as before; Dismiss does the same, and "Open in app"
  also brings that Claude window to the front. A prompt typed in Claude Code closes the
  card. Behind a fullscreen app, Claude stops at once and a toast says so. Clicking the
  box activates the notch window, which otherwise never takes the keyboard.
  Permission requests and questions from other sessions go ahead of waiting reply
  cards. Stop now has the permission wait as its timeout, so this needs
  `--install-hooks` again. Commit `feat: reply to claude from the notch`.
- [x] **8.16** A reply that starts with a slash command runs that skill. Claude Code
  expands slash commands only in prompts typed in the app, so the hook turns
  `/name arguments` into an instruction to invoke skill `name` with the Skill tool,
  then follow it with the arguments, or say no skill has that name. Commit
  `feat: run skills from notch replies`.
- [x] **8.17** CPU and GPU are optional: `show_cpu` (CPU, and RAM in the panel) and
  `show_gpu` (GPU and VRAM), both on by default, as checkboxes in Settings. The idle
  pill sizes itself to what it shows instead of fixed widths, so turning readings off
  shrinks it; the panel drops the hairline when neither shows, and upright it pairs
  CPU with RAM and GPU with VRAM. GPU counters are not read while the GPU is hidden.
  Commit `feat: optional cpu and gpu readings`.
- [x] **8.18** Choose which AI providers to track: `providers`, all three by default, as
  one checkbox each in Settings. An untracked provider is never read, so Antigravity
  stops running while Gemini is off, and it leaves the right-click menu. Unchecking the
  shown provider moves the notch to the next tracked one; with none tracked, the ring
  and limits leave the notch and no status page is checked. Commit
  `feat: choose which ai providers to track`.
- [x] **8.19** Drop the "Exact Claude usage" toggle: tracking Claude is enough, so the
  account endpoint is always asked while Claude is tracked. An old `claude_account` key
  still loads and is dropped on the next save. Commit
  `refactor: drop the exact claude usage toggle`.
- [x] **8.20** Rename to Notchling: crate and exe, pipe, window titles, menu, toast id,
  `%APPDATA%` and `%LOCALAPPDATA%` folders. The old `%APPDATA%\ai-notch\config.toml` is
  copied over the first time, and `--install-hooks` replaces hooks that still name
  `ai-notch.exe`. Commit `refactor: rename to notchling`.

## Phase 9: Ship it on GitHub

Local setup after the rename:

- [ ] **9.1** Build `target\release\notchling.exe`, run `--install-hooks` (the Claude Code
  hooks still name the old `ai-notch.exe`), start it, then delete `%APPDATA%\ai-notch`,
  `%LOCALAPPDATA%\ai-notch` and the old `ai-notch.exe`. No commit.

The exe:

- [ ] **9.2** Icon and file details: a `notchling.ico` (256×256), embedded with
  `winresource` in `build.rs` along with product name, version and copyright. Commit
  `feat: app icon and file details`.
- [ ] **9.3** Link the C runtime statically (`.cargo/config.toml`,
  `target-feature=+crt-static`), so the exe starts on a fresh Windows without the Visual
  C++ runtime. Commit `build: static c runtime`.
- [ ] **9.4** README: say that Windows SmartScreen warns about the unsigned exe and how to
  run it anyway. Commit `docs: smartscreen note`.

Releases:

- [ ] **9.5** CI workflow: `cargo fmt --check`, `clippy -D warnings` and `test` on
  `windows-latest` for every push and pull request. Commit `ci: check every push`.
- [ ] **9.6** Release workflow: a `v*` tag builds `notchling.exe` and attaches it to a
  GitHub Release. Commit `ci: build releases from tags`.
- [ ] **9.7** First release: bump the version, tag `v0.1.0`, push the tag, write the
  release notes. Commit `chore: release 0.1.0`.

The GitHub page:

- [ ] **9.8** Repo About box: description and topics (`windows`, `rust`, `slint`,
  `claude-code`, `dynamic-island`, `system-monitor`). Done on GitHub, no commit.
- [ ] **9.9** README top: one-line pitch, badges (license, release, CI), download link,
  and a demo GIF recorded with ScreenToGif in `docs/`. Commit `docs: readme showcase`.
- [ ] **9.10** Pin the repo on the GitHub profile. No commit.

Getting it seen (never buy or swap stars):

- [ ] **9.11** Post with the GIF: r/ClaudeAI, r/rust, r/Windows11, a Show HN, and Slint's
  showcase. Read each community's self-promotion rules first.
- [ ] **9.12** Pull requests to awesome lists (Claude Code, Slint).
- [ ] **9.13** Label a few "good first issue"s and answer issues quickly.

Later:

- [ ] **9.14** Code signing (Azure Trusted Signing or a certificate) to end the
  SmartScreen warning.
- [ ] **9.15** Installer (Inno Setup) with a Start menu entry and uninstaller, then a
  `winget` listing.

## Phase 10: Agent requests and motion, after Crest's demo

Answering requests:

- [ ] **10.1** Find out what the `PermissionRequest` hook can return: a session or
  permanent allow rule, or only allow and deny. Notes in `docs/FINDINGS.md`. Settles
  8.9 and decides 10.2 and 10.3. No commit unless findings change.
- [ ] **10.2** "Allow the rest of this session" checkbox on the request card: matching
  requests from that session are allowed without a card until it ends.
- [ ] **10.3** Allow ▾ menu: Allow once, Allow this command for the session, Always allow
  (a rule in Claude Code's `settings.json`, if the hook API allows it).
- [ ] **10.4** Show the outcome: after Allow, Deny, a timeout or an answer in the
  terminal, the pill shows "Allowed" or "Denied" with a green or red mark for about
  1.5 s, then goes back to idle.
- [ ] **10.5** Live command in the pill: the running tool and a short argument ("npm test",
  "Edit LoginForm.tsx") from `PreToolUse` and `PostToolUse`, beside the timer.
- [ ] **10.6** Coloured command in the code box: command, flags and quoted strings each
  in their own colour.
- [ ] **10.7** Diff preview for Edit and Write requests: the file and the changed lines,
  added and removed, in the code box.
- [ ] **10.8** Rules: `always_allow` and `never_allow` patterns in the config, applied
  before any card shows, editable in Settings.
- [ ] **10.9** Research: do Codex CLI and Gemini CLI have approval hooks like Claude
  Code's? If so, answer them on the same card.

Motion (each follows Windows "Show animations", and the agent motion has its own
`agent_motion` setting, on by default, as a checkbox in Settings):

- [ ] **10.10** Working glow: while an agent works, a soft light circles the notch's
  outline and breathes, brightening and fading over about 2 s, in the provider's
  colour. Stops when the agent stops; a still outline when motion is off.
- [ ] **10.11** Moving logo: while working, the agent's logo slides along the pill to the
  right and back, or nudges right on each new tool call; it settles when the agent
  stops.
- [ ] **10.12** Request pulse: when Claude asks for something (a permission, a
  question, a reply), the pill gives two or three quick pulses, brighter outline and a
  slight swell, in under half a second, then grows into the card. Follows
  `agent_motion` and "Show animations"; with motion off the card just appears.
- [ ] **10.13** Card transitions: the pill grows into the request card and shrinks back
  into the outcome pill (10.4) with one spring-like motion instead of a cut.

Suggested UI polish:

- [ ] **10.14** The usage ring fills smoothly to a new value instead of jumping.
- [ ] **10.15** A pulse on the ring when a usage alert fires, amber or red as the alert.
- [ ] **10.16** Several pending requests: small dots under the card, one per request,
  instead of only "2 more".
- [ ] **10.17** Keyboard hints on the buttons when shortcuts are on (Ctrl+Alt+Y,
  Ctrl+Alt+N).
- [ ] **10.18** Hover shows each provider's ring side by side in the panel, so all
  tracked limits are visible at once (overlaps 8.10).
- [ ] **10.19** Optional sound when a request arrives, off by default.
- [ ] **10.20** Light theme for the panel and Settings, following Windows' app mode.

## Deviations from SPEC.md

- No `nvml-wrapper`: PDH provides VRAM, and the design shows no GPU temperature.
- `ureq` instead of `reqwest` + `tokio`: a few blocking HTTP calls need no async runtime.
  It uses Windows' own TLS (SChannel) and certificate store, not rustls and ring, with
  gzip for Google's incident feed.
- No `keyring`: step 4.2 was dropped, so notchling stores no secrets of its own.
- The Claude account source refreshes Claude Code's expired login and writes it back
  (step 7.3), instead of only reading it: the desktop app never refreshes that file.
- `chrono` for timestamps: Slint already builds it, so it adds no crate.
- `ruzstd` instead of `zstd` for Codex logs: a pure-Rust decoder, no C build.
- Right-click menu added: needed to quit and switch providers.
- `--install-hooks` added: edits `settings.json` safely with a backup.
- One exe for app and `--hook` mode. If V1 shows stdin/stdout problems with the GUI
  subsystem, split the hook into a second binary in the same crate.
- Start with Windows: not included; add later if wanted.
- Slint software renderer instead of OpenGL (femtovg): same look, a third of the RAM.
- Right-click menu is a native Win32 popup: a Slint popup would be clipped by the
  window region.
