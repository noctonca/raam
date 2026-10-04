---
name: ux-qa
description: Pixel-level visual and UX QA of the frame's egui UI against the design system's UX.md (Laws of UX turned into rules), run as a lead designer who measures, asks the maintainer on taste calls, has subagents fix, re-verifies every round, and confirms on the real frame. Use it whenever new UI comes online or changes in raam — a new settings screen, a kit component, a gallery page, a frame_ui screen, an overlay or dialog — and whenever the maintainer asks for visual QA, a design review, "pixel perfect", alignment or spacing checks, a UX pass, lawsofux, or "does this look right", even if they don't say QA.
---

# UX QA for the frame

You are the lead UX designer doing visual QA. The bar is pixel perfection,
judged by measurement, not by eye alone. Subagents fix; you find, decide,
brief and verify. The maintainer decides matters of taste, and touches
the frame.

The standard is `docs/UX.md`: the grid, the vertical rhythm, the
centring rules, and each Law of UX turned into rules for this device.
Read it in full before looking at anything. When a decision made during
QA changes a rule, update UX.md in the same round, so the next pass
checks against it.

## Tools

- **Desktop host.** The `raam` binary renders the core's own
  theme/kit/gallery/frame_ui with the core's own painter, so it shows
  the same pixels as the frame: `cargo build --profile quick -p raam`, then
  `target/quick/raam` (release without LTO: the same pixels, a fast
  relink). Its flags are in the `//!` header of
  `src/main.rs`:
  - `--exact`: device pixels;
  - `--page`, `--theme`, `--scroll PX`, `--size WxH`;
  - `--click X,Y`, `--press X,Y`, `--hold X,Y,MS`;
  - `--screenshot f.png`.

  A new screen needs a page in this host, or its own host, before QA can
  start. Adding it is the first job.
- **`scripts/qa.py`**, run as
  `uv run -q --with pillow python .claude/skills/ux-qa/scripts/qa.py <cmd>`.
  Its docstring lists every command:
  - `shoot` and `frame`: capture sets;
  - `ink` and `glyphs`: extents and centres;
  - `zoom`: joins at 3×;
  - `contrast`: WCAG ratio;
  - `sections`: header gaps and proximity;
  - `diff`: compare two capture sets.

  In zsh, wrap it in a function (`qa(){ uv run ... qa.py "$@"; }`). A
  command stored in a variable doesn't word-split, and fails with "no
  such file".
- Keep captures in the scratchpad: `baseline/`, then one directory per
  round. Subagents can save images but may be blocked from writing report
  files, so ask them for their report as their reply.

## The pass

### 1. Baseline

Shoot every affected page in both themes, at scroll 0 and further down
until the page ends. Also take tall captures (`--tall 1900`) for rhythm.

When the change is a diff on top of a reviewed state, also shoot a
**reference**: the same set from a host built at the last reviewed
commit. A `git worktree` at that commit has its own host; `shoot --bin`
selects it. `diff` then shows exactly which areas changed, so nothing
changed is missed and nothing unchanged is reviewed again. For preset
pages, `scripts/goldens.sh show 'set-photos*'` does the same against
the last blessed commit. A brand-new
screen has no reference: review all of it.
Look at every image yourself. Then measure what looks off, and also what
looks fine: most real findings measure 1 to 4 px and aren't visible at
1:1.

Check each page against UX.md's checklist, and specifically:
- left edges on the grid; right edges on the trailing edge;
- text centred to ±0.5 px against its row's icon (`glyphs` gives both
  centres);
- row heights, and section gaps by ink (`sections`), with proximity: the
  gaps inside a group are smaller than the gap between groups;
- one style per role across pages: headers, notes, cards, captions;
- every join at 3× (`zoom`): seams, double borders, notches, clipped
  glyphs;
- contrast of every text/fill pair, in both themes (`contrast` with
  `IMG@X,Y`);
- truncation; empty values show "—", never a blank or a fake 0;
- one icon per concept; an icon that's illegible at its size;
- demo copy against the laws. A demo that contradicts UX.md teaches the
  wrong thing (an error demo once rejected a URL that Postel's Law says
  to fix up).

Write each finding as the measurement, then the rule it breaks. For
example: "Theme row label cap centre y 516.5, icon 520.5: 4 px high
(centring rule)". Find the root cause before briefing: a hand-built row
that bypasses the kit is a different fix from a kit bug.

### 2. Ask before fixing

Some findings have more than one right answer. Ask the maintainer these with
`AskUserQuestion`, putting your recommendation first:
- equal gaps measured by ink or by layout;
- a better glyph, or no icon at all;
- where a new doc lives;
- how far the bar reaches, e.g. dev pages too.

Show the measurement and the trade-off. Don't ask about plain violations
of UX.md; just fix those.

### 3. Brief the fixers

Split the work by file ownership, so parallel fixers don't edit the same
functions. For example: one fixer takes kit.rs plus the pages that ship,
another the dev pages. Brief each with the template in
`references/fixer-brief.md`: measured findings, scope, how to verify, and
what to report. Run them in the background, and do something useful while
they work: prepare the frame, or update UX.md.

When one fixer's report affects another, relay it with `SendMessage`: a
centring helper one fixer writes may belong in the kit that the other
fixer owns.

### 4. Verify every round yourself

Don't pass on a fixer's report as the result. Re-shoot, `diff` against the
previous round (only the areas you expect should change), look at the
pages, and re-measure what was fixed. Then ask: does the fix hold up
against the other laws?
- Equal visible gaps broke proximity on Settings until list rows counted
  as their full row box.
- A "fitting" icon (the globe) turned out to be already used for another
  concept.

Expect three or four rounds. Each round's brief is only the new
findings, sent to the fixer that has the context (SendMessage resumes
it).

### 5. The frame

The desktop settles pixels. Only the frame settles the panel, the text
weight with the shader boost, fingers, and timing.

1. **Borrow it.** Another Claude session may be using the frame. Ask it
   with `SendMessage` (find it with `ListAgents`), and follow its
   conditions, such as the music volume left at 0 and the previous app
   relaunched and left in front. If nothing else is running, still
   restore the frame afterwards (raam is the home app: force-stop
   relaunches it clean).
2. **Build and install,** with the machine's `.env` loaded (see
   `scripts/env-check.sh`): `cd hosts/android && cargo apk2 build
   --release`, `adb install -r target/release/apk/raam-android.apk`, and
   launch it (`am start -n
   io.github.noctonca.raam/android.app.NativeActivity`).
3. **Capture and compare:** raam has no page props: put the frame on
   the page by hand or with scripted taps (the nav rail's items sit at
   fixed positions), then `adb exec-out screencap -p > shot.png`, and
   `diff` it against the desktop shot at scroll 0.
   - A maximum of about 13 levels, with nothing moved, is rasterisation.
     The layout is identical.
   - A moved box is a real device difference: investigate it.
4. **The maintainer's checks,** asked in one `AskUserQuestion`. The list is in
   `references/device-checks.md`: press states and timing, long holds,
   scroll against taps, legibility in both themes, the smallest target
   hit reliably, and disabled states.
   - If one fails, keep the frame, fix it on the desktop (the host can
     replay touches with `--hold`), reinstall, and ask again.
5. **Hand it back:** clear every prop you set, force-stop your app,
   relaunch what was in front, and check with `dumpsys activity`. Tell
   the other session (a short note).

### 6. Record

- docs/UX.md holds every decision made during the pass.
- The commit message (or the PR body) records the pass:
  - the question and the method;
  - findings with measurements before and after;
  - the frame results;
  - what's open.
- Commit only this work. Stage paths explicitly and check `git show
  --stat`, because another session may have uncommitted changes in the
  same files.
- A lesson that would cost the next pass a round goes under Lessons
  below, such as the root cause of a device bug.
- The pass moves the golden suite's shots: re-bless them
  (`scripts/goldens.sh bless`) in a commit of their own, after the
  work, and run `scripts/web-diff.py`.

## Lessons

Each of these cost at least a round. Check for them on sight.

- **egui's `Align2::*_CENTER` centres the line box, not the glyph.**
  Material Symbols sit in the em above the baseline, so a 24 px icon lands
  1 px low; Roboto text lands up to 4 px off. Use kit's `text_on` (cap
  centre, baseline on a whole pixel) and `icon_on` (em box). A new
  component that centres by hand is a finding.
- **A row built by hand drifts.** Every settings row is a
  `kit::list_item`. If a trailing control is missing, add a `Trailing`
  variant; don't hand-build the row.
- **Allocation is not what people see.** 48 px touch targets add slack
  around small visuals, so gaps that are equal by layout look unequal.
  Components report their visual with `kit::mark_visual`, and headers
  space from it. A list row's visual is its whole row box, because its
  press layer fills it.
- **Fixing one law can break another.** After every spacing change,
  re-run `sections` and look for PROXIMITY flags.
- **A card with the pane's own fill is invisible.** Common Region needs a
  visible boundary. Filled text fields are surfaceContainerHighest, so
  they don't go inside a card.
- **egui's scroll-edge fade** makes a header at the fold look disabled.
  It is off in `theme::style`.
- **egui turns a hold over 0.8 s into a long-touch,** so the release
  never clicks. Only the frame showed this: a long hold on the Theme
  segments did nothing. `max_click_duration` is infinite in
  `theme::install`. Test long holds on every new control.
- **Material Symbols with detail inside** (digits, text) are illegible at
  24 px. Zoom every new glyph at its real size before using it.
- **Adding an icon:** add it to `crates/raam-core/assets/icons.txt` and
  run `tools/build-icons.sh` (it needs the network; it also regenerates
  `icons.rs`). Then check the rebuilt fonts: the other glyphs must be
  unchanged. Remove glyphs that end up unused.
- **`adb exec-out screencap -p`,** not `adb shell screencap`: the shell
  mangles the PNG. Probe props are read afresh at app start, so any left
  set act again at the next launch. Clear them all.
- **An anchored `egui::Area` centres by last frame's size.** The desktop
  shoots each page in a fresh run, so it never shows.
  On the frame, a live page switch (7 toolbar items to 5) left the menu
  off centre. Measure first and use `fixed_pos`. Capture each floating
  element after a live switch too, not only on a fresh start.
- **Fingers land 15 to 25 px above the glyph aimed at** on this panel
  (measured on the keyboard). Dense grids need visible key shapes, and
  targets raised above their visuals. Read the tap log (`tap down ...`)
  when someone says "hard to hit": it shows where the taps really
  landed.
- **`sections` false positives:** coloured body text that starts on the
  content edge (warning or error demo text) can read as a header, and raw
  egui widgets don't report their visual. Check flagged spots by eye.
