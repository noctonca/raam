# Fixer brief template

Fill in the brackets. Keep the measured findings exact: a fixer can't
check "looks a bit high", but it can check "cap centre 516.5, icon centre
520.5".

```
You are fixing visual QA findings in the egui design system of a 1280×800
touch photo frame (Mali-400, 1 px = 1 dp). Pixel perfection is the bar.
Repo [the raam checkout], crate [crate path]. FIRST read docs/UX.md
fully: it is the standard. Match the surrounding code's style and comment
density.

Scope: [files and functions this fixer owns]. Another agent is editing
[other files/functions] at the same time; don't touch those. If the build
breaks in code you didn't touch, wait 30 s and retry. Re-read a shared
file right before each edit. Don't commit.

Iterate with the desktop host: `cargo build --profile quick -p raam`, then
`target/quick/raam --theme dark|light --page <page> --exact
[--scroll PX] --screenshot /path.png`. Look at every screenshot with the
Read tool. Measure with `uv run -q --with pillow python
.claude/skills/ux-qa/scripts/qa.py` (ink, glyphs, zoom, contrast,
sections, diff; the docstring has usage).
Put your captures in [scratchpad]/[round dir]/. Verify every fix by
measurement in BOTH themes, not by eye alone.

Findings:
1. [measurement → rule broken → the root cause, if known]
2. ...
N. Anything else you find against UX.md's checklist on these pages, in
   both themes, including further down the page.

Fix at the root: in the kit when a kit component is wrong, not by
nudging one page. When a finding has more than one right answer and none
is clearly better, choose one and say so in your report; don't stop to
ask.

List anything only the real frame can confirm (colour and contrast on the
panel, thin text with the shader boost, finger hits, press and hold,
animation smoothness), but don't try to check it.

Final report, concise: for each finding, what changed (file:line) with
measurements before and after; what you didn't fix and why; the new
findings you fixed; the frame-only list. `cargo build --profile quick -p raam`
must pass without warnings.
```

For round two onwards, send only the new findings to the same fixer with
`SendMessage`. It resumes with its context, so skip the setup text.
