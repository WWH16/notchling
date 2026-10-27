# UI and feature audit

Audit of the notch UI (`ui/notch.slint`) and its features, done after step 7.8. Based on
the source and on screenshots of the live notch. The Impeccable detector only covers web
UI, so it did not run.

## Score: 12/20, Acceptable

| # | Dimension | Score | Key finding |
|---|---|---|---|
| 1 | Accessibility | 1 | No accessible roles or labels, no keyboard path, hover-only detail panel |
| 2 | Performance | 3 | Lean (about 35 MB); the alert dot's pulse redraws every frame while an alert shows |
| 3 | Theming | 3 | `Theme` holds the tokens, but several hex values are hard-coded in components |
| 4 | Windows conformance | 2 | A permission card can catch a click meant for the window behind it |
| 5 | Adaptivity | 3 | Per-monitor DPI and all four edges work; Windows text size is ignored |

The design system is coherent: one black shape that morphs, provider colours with
meaning, native Segoe UI type and a native context menu. State never relies on colour
alone; every value is also shown as text.

## Findings

### P1: fix before relying on it

1. **Accidental Allow.** A card can appear under the cursor while the user clicks a tab
   or title bar at the top of the screen, and that click approves the request.
   `PillButton` acts on the first click. Fix: ignore clicks on Allow and Deny for about
   500 ms after a card appears, and show that time passing on the button.
2. **Long commands hide their overflow.** The 104 px `Flickable` has no scrollbar, fade
   or "more" hint, so a command can be approved without being read in full, although
   the README promises the full command. Fix: a bottom fade plus "+N lines" when the
   command overflows.
3. **No accessibility.** No `accessible-role` or `accessible-label` anywhere; buttons
   are a `Rectangle` with a `TouchArea`. Narrator reads nothing, and a request cannot be
   answered without a mouse. Fix: roles and labels on the pill, card and buttons, plus
   an optional global shortcut for Allow and Deny.

### P2: next pass

4. **Hover opens by accident.** The panel opens after 120 ms of hover, and the top edge
   is where browser tabs and menus are. Fix: click the pill to toggle the panel, or
   raise the hover delay to about 300 ms.
5. **The pill covers content on a side edge,** such as an app's sidebar list. Fix: an
   optional tuck mode that leaves a thin sliver until hovered.
6. **Windows "Show animations" is ignored.** The pulse, the morphs and the glide always
   run. Fix: read `SPI_GETCLIENTAREAANIMATION` and fall back to plain fades.
7. **The upright hover panel may clip.** At 210 px, "Claude · Degraded · ESTIMATED"
   needs about 179 px against about 174 px available. This is estimated from text
   widths, not observed.
8. **10 px labels at fixed pixel sizes** are hard to read at 100% scale and ignore the
   Windows text size setting.

### P3: polish

9. Move hard-coded colours into `Theme`: `#161618`, `#3a3a3c`, `#e5e5ea` and the
   button states.
10. Show "2 more" instead of "+2" in the card header.
11. Fall back to Consolas when Cascadia Mono is not installed.

## Feature ideas, by value

1. **Live Claude activity in the idle pill**, such as "Claude · working 3m" per session.
   The keep-awake tracking already follows prompt-submit and stop events.
2. **"Always allow"** on the permission card, matching Claude Code's "don't ask again",
   if the PermissionRequest hook accepts permission updates. Check the hook API first.
3. **Limit forecast**: "at this rate, limit reached around 4:10 PM", from the burn rate
   that is already computed.
4. **"Start at sign-in"** toggle in Settings, using the `HKCU\...\Run` key instead of a
   manual `shell:startup` shortcut.
5. **All providers at once** in the hover panel, as small rings, instead of switching
   through the menu.
6. **Double-click** to send the notch back to the top centre.

## Plan

Phase 8 of [PLAN.md](PLAN.md), one commit per step.
