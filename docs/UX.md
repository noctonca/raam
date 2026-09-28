# UX rules for the frame

This file is the design system's contract (`crates/raam-core/src/theme.rs`,
`kit.rs`, `gallery.rs`, `frame_ui.rs`; its numbers were measured on the
frame). The tokens say *what* the pieces are. This file says *how they're
put together*, and why. Visual QA (the ux-qa skill, in
`.claude/skills/ux-qa`) checks the gallery and the product's screens
against it, and a change that breaks a rule either fixes the rule here or
doesn't land.

The "why" comes from [Laws of UX](https://lawsofux.com). Each law below is
turned into rules for this device, not restated.

## The device

- SNUG frame, 1280×800 at 160 dpi, so **1 px = 1 dp**. Every number here
  is in device pixels. The desktop host's `--exact` shows the same pixels.
- Touch first: **no hover, no cursor** (egui_keyboard for text). A hover
  state must never be the only sign of anything. A Linux frame may have a
  mouse, which is a finger, or a keyboard, which works as
  [Keys](#keys) says.
- Two viewing distances: the slideshow is glanced at from across a room.
  Settings are used at arm's length, standing up, usually rarely.
- Mali-400: every covered pixel is still paid for. Opaque
  settings stop the slideshow drawing underneath.

## The grid

Measured in the detail pane (1280 wide, list pane 360, pane inset 24):

| Edge | x | What sits on it |
|---|---|---|
| Container edge | 384 | a top-level page's title and subtitle, the outer edge of filled containers (notes, cards) |
| Content edge | 400 | section headers, list icons, component rows, the icon inside a note or card |
| Text edge | 440 | list headlines and supporting text (icon 24 + gap 16); a sub page's title and subtitle, after its back arrow on 400 |
| Trailing edge | 1240 | chevrons, switches, trailing controls |

- Two left edges only: **384 for containers and titles, 400 for content**.
  Anything drawn at another x has to justify it.
- Components with a 48 px target narrower than their visual (checkbox,
  icon button) align their **visual** to the content edge, not their
  target. The target may overhang into the padding.
- A container sets its own insets: the text in a filled text field starts
  where M3 puts it (12 after its leading icon), not on 440.
- Notes hug their text, up to the content width: box at 384, icon at 400,
  text at 440, so the text lines up with the list rows around it.
- Sliders take a title line (title left, value right) above the track, and
  share the width of the text fields in their column.

## Vertical rhythm

- Spacing tokens only: 4, 8, 12, 16, 24, 32 (`theme::space`).
- List rows: 56 for one line, 72 for two lines. **Every** row in a list is
  a `kit::list_item`, whatever its trailing control: a hand-built row
  drifts (the Theme row was 4 px high and 4 px short).
- Two-line rows use M3's line boxes: headline 24 (bodyLarge), then
  supporting 20 (bodyMedium), centred as one 44 px block.
- Gaps between sections are measured **by what shows, ink to ink**, not
  by allocations. A 48 px touch target round an 18 px checkbox is empty
  for 15 px either side; measuring from the target made the gaps on one
  page run from 28 to 44. Targets overhang into the gap; they don't widen it.
- What shows: a component's box (button, field, segmented button, note,
  card), a **list row's whole 56/72 rect** (its press layer fills it, so
  that's the surface a touch shows; the same for nav items), a switch's
  32 px track, a chip's 32, a checkbox's 18, an icon's 20 px live area
  (Material Symbols draw inside 20 of their 24), and text from **cap top
  to baseline**. Ascenders and descenders hang into the
  gap, so raw ink reads up to 4 px short of the numbers below.
- Section header: its caps start **24** below whatever shows last before
  it, whether that's a group or the page subtitle's baseline. Its group's
  first visible row starts **16** below its baseline. Always the same,
  on every page, whatever the components.
- Between rows of one group: 8, target to target.
- `kit::section_header` does this by itself: kit components report what
  they show (`kit::mark_visual`). A layout the kit doesn't draw marks its
  own (the gallery's icon grid, `kit::paragraph` for wrapped text), or
  it's measured as if all of it showed. Stock egui widgets don't mark.
- Text is centred on its row by its **cap centre**, not its line box, and
  icons by their em box (Material Symbols sit above the baseline, so
  egui's `Align2::*_CENTER` puts a 24 px icon 1 px low). Tolerance:
  ±0.5 px. Use kit's `text_on` and `icon_on`; never centre by hand.

## Law by law

**Fitts's Law** (target size and distance).
- 48×48 minimum touch target, 8 px between targets. The Touch targets page
  measures this on the frame; 32 and 40 exist only as test rows.
- Controls side by side in a row are 12 apart (the style's
  `item_spacing.x`), and nav items 12 apart, so a focus ring keeps its
  4 px of air (see [Keys](#keys)).
- The whole row is the target for a list item, never just its switch.
- Destructive actions are never next to the thing tapped most often.

**Hick's Law, Choice Overload** (more options, slower decision).
- A segmented button holds 2 to 4 options. Five or more go into a
  picker dialog behind a `Value` row.
- A picker lists at most about 7 options on one screen without scrolling.

**Miller's Law, Chunking** (working memory holds about 7 chunks).
- A settings section holds at most about 7 rows. More means a new section
  or a sub-page.
- Long values are chunked for reading: "2 albums · 227 photos", not
  "227 photos in 2 albums selected".

**Jakob's Law** (people expect it to work like the apps they know).
- Android/M3 conventions: a chevron opens something, a switch acts
  immediately, a segmented button picks one, a filled button is the main
  action. No new meanings for these shapes.
- The value shown in a `Value` row is exactly the label chosen in its
  picker.
- Every control acts on release, however long it's held, and shows its
  press state the whole time. Only moving past touch slop (a scroll)
  cancels. egui's 0.8 s click limit is off in `theme::install` for this;
  so there's no press-and-hold gesture anywhere.

**Law of Proximity** (near things read as a group).
- The gap inside a group is always smaller than the gap between groups:
  8 between targets inside, 24 of visible space between. A section header
  is closer to its own rows (16 below its baseline) than to the previous
  section (24 above its caps).
- Supporting text touches its headline (no gap beyond the line boxes).
- A field's supporting or error text is 4 below the field, with the
  field's text edge.
- Two fills never touch: a press layer, a selected pill or a focus ring
  keeps visible space from its neighbour's (a pressed nav item's layer
  ran into the selected pill below it when the items abutted).

**Law of Similarity** (things that look alike are taken to work alike).
- One style per role on every page. Section headers are `titleSmall` in
  `primary` everywhere: settings, components, probe.
- One icon per concept, and a concept's icon means only that. Two
  different settings never share an icon (Clock and 24-hour clock did).
- The same component has the same size everywhere: sliders and text
  fields in one column share a width.
- Every secret is one component, whatever it guards (a Wi-Fi password,
  the API key): a filled text field with the key icon, dots, and the
  trailing eye that shows it in the clear (`kit::TextField::secret`).
  A shown secret is hidden again when the menu closes.

**Law of Common Region** (a boundary makes a group).
- A card is visible: its fill differs from what it sits on (`kit::card`
  is surfaceContainerHighest on the pane). An invisible card is just
  padding. Filled text fields are that same colour, so they never go in a
  card: section headers group them instead.
- Scroll areas don't fade their edges: a faded header at the fold reads
  as disabled.
- A note sits directly under the row it's about, inside that section.

**Law of Uniform Connectedness.** Segments of a segmented button share
their outline and touch; chips never touch.

**Law of Prägnanz** (people read the simplest shape).
- Corner radii come from `theme::shape` only. Nested shapes: the inner
  radius is the outer minus the padding.
- No 1 px seams, double borders or notches where two shapes meet. Check
  every join at 3× zoom.

**Von Restorff Effect** (the one that differs gets noticed).
- At most one filled (primary) button per screen.
- Saturated colour is kept for state: selected, warning, error. Nothing
  decorative uses `error` or `warning`.

**Serial Position Effect.** The settings used most (photo interval,
albums) go first in their section; rarely touched ones (server) go last.

**Aesthetic-Usability Effect.** Polish is how people judge reliability:
misalignment makes the frame feel broken. It's why this file exists.

**Doherty Threshold** (feedback within 400 ms keeps people engaged).
- The pressed state shows on the frame the touch lands on.
- Anything over 400 ms shows progress. A theme switch measured 94 to
  110 ms to its third frame on the frame, which is inside.

**Tesler's Law** (complexity has to live somewhere).
- The frame carries it, not the person: sensible defaults, discovery,
  derived values. A setting exists only if people really disagree about it.

**Postel's Law** (be liberal in what you accept).
- Input is normalised, not rejected. A server URL typed without
  `http://` gets it added, with the result shown; an error only appears
  when there's nothing sensible to do.

**Cognitive Load, Occam's Razor.** Every element earns its place.
Supporting text says what the setting does in the person's terms, not the
implementation's.

**Paradox of the Active User** (people don't read manuals).
Explanations are inline (supporting text, notes), never in a separate
help page.

**Goal-Gradient Effect, Zeigarnik Effect.** First-time setup shows its
steps and how many are left. An unfinished setup is shown as unfinished,
not hidden.

**Peak-End Rule.** How a flow ends is what's remembered: after setup, the
first photo appears right away, not a blank screen.

**Selective Attention, Banner Blindness.** Notes are rare, short and
specific ("Iceland" is not on the server any more). A note that's always
there stops being read.

## Settings, menu and keyboard

- **Settings are opaque and full-screen**, in the list/detail shell. The
  sections are Photos, Slideshow, Videos, Display, Sleep, Connectivity
  and Server. The slideshow isn't drawn under them.
- **A sub page has its own back arrow**, before its title, and it goes up
  one level, to the page that opened it (Albums and Hidden to Photos,
  Wi-Fi networks to Connectivity, Join to Wi-Fi networks). The arrow is
  a list icon, on the content edge (400); the title and subtitle move
  to the text edge (440). The top bar's "← Settings" always leaves
  Settings, as its label says, from a sub page too. Android's two-pane
  Settings and M3's list-detail work this way (Jakob's Law): the way
  back is where the eye already is, not across the screen.
- **Connectivity** says first what carries the traffic (Ethernet or
  Wi-Fi) and its address; with a cable in, Wi-Fi reads as a backup.
  - The Wi-Fi networks are a sub-page, strongest first, with the joined
    one on top and its Forget beside it. A saved network joins on a
    tap; an open one asks first.
  - A password is typed on a page of its own, not in a dialog: the
    on-screen keyboard would cover a dialog.
  - Progress is words that change ("Joining…", "Joined: getting an
    address…", "Looking for networks…"), not an animation, which
    would keep egui drawing.
  - A join or a scan of the open list keeps the menu up; the idle
    timeout starts once it ends.
  - Where Wi-Fi can't be set up, the page says why and how to fix it.
- **Values that need more than a tap** use a `Value` row that opens a
  dialog, never an inline slider in a list: photo interval, sleep and
  wake times (`kit::number_picker`, `kit::time_picker`: the value large,
  tonal −/+, a slider under them). A slider in a list has no icon, so it
  broke the 400/440 columns.
- **Setting a value vs acting:**
  - A `Value` row opens a picker.
  - A `Trailing::Button` row is an action on one item, e.g. Unhide.
    Only its button is the target, so a scroll or a stray tap can't act.
  - A plain row with no trailing control acts when tapped, e.g. Rescan
    or Sync now.
- **An unfinished setup says what's missing** and leads to it. With no
  server, Albums reads "Add the server first" and opens Server; the menu
  says "add a server in Settings".
- **Unknown values are left out**, not shown as "—" mid-sentence. For
  example, "Nothing saved yet · up to 1 GB".
- **The menu is a floating toolbar,** bottom-centred, 24 px above the
  edge:
  - surface-container at 90% opacity;
  - every item the same width, sized for the longest label it can ever
    show, so Hide turning into "Undo hide" moves nothing;
  - the items 12 apart and 12 in from the bar's edges, room for a focus
    ring;
  - one status line in plain words;
  - Pause/Previous/Next disabled while there are no photos.

  It's placed at an explicit, measured position (`fixed_pos`). An
  anchored Area centres by last frame's size, and on the frame it stayed
  off centre after the items changed.
- **On-screen keyboard:**
  - visible keycaps with a 6 px gap between them;
  - letter keys in one tone, function keys (shift, backspace, Done) in a
    tinted one;
  - 56 px rows;
  - shift and backspace 1.5 keys wide;
  - a Done key that puts the keyboard away.

  Each key's target covers its share of the gaps, and sits 8 px higher
  than its keycap. On this panel fingers land 15 to 25 px above the glyph
  aimed at (taps on backspace hit the row above).

  It goes away once a physical key is pressed, and comes back with the
  next touch.

  What's being typed stays in view: the focused field, its supporting
  line, and the control it leads to (the next field, the button it
  submits to) sit above the keyboard, and the page scrolls to keep them
  there. Anything else may go under it, the nav list too, which never
  moves to make room.

## Keys

A physical keyboard does what a finger does, with the keys people know
from photo viewers and dialogs (Jakob's Law).

- **The slideshow** (the menu closed): ← and → go to the previous and
  next photo at once. Any other key opens the menu, as a tap does, with
  the first photo of a collage picked for Hide and Fill/Fit. Escape does
  nothing there, so pressing it to get away never brings the menu back.
- **The menu and settings:** the arrows and Tab (Shift-Tab back) move
  the focus; Enter or Space acts on the focused control, as a tap does.
  On a slider, ← and → change the value instead.
- **Escape steps back one level:** out of a text field, then out of a
  dialog (as Cancel), then out of a sub page, then from settings to the
  menu, and last the menu closes.
- **Where the focus goes:** opening the menu by key focuses its first
  item that can act; settings, the selected section; a sub page, its
  first control that isn't destructive; a picker, the chosen option; a
  number dialog, its slider; a confirmation, Cancel, never the action it
  confirms. Stepping back returns the focus to the control that opened
  the level. A key pressed while nothing has the focus (the menu was
  opened by a tap) only brings the focus in.
- **The focus ring** is M3's: 3 px of secondary, 2 px outside the
  control's shape, over the 10% focus state layer. A control that spans
  its pane (a list row, a picker's option) takes it just inside, where
  the pane can't clip it. A text field shows focus with its own
  indicator line instead. Only keys give focus: a tap never does, except
  on a text field.
- **The ring keeps at least 4 px of air** to anything else: another
  control, text, an icon, a container's edge. The layout makes that
  room: 12 between controls in a row and between nav items, 12 round the
  menu's items, the top bar's title 8 past its label column, a focused
  slider's gap 9 either side of its handle, and a sub page's back arrow
  with a 34 px state layer, so its ring is 4 clear of the title on 440.
  Where controls share an outline (segments) or one sits inside another
  (the eye in a field), the ring goes just inside. So does any ring the
  kit finds still within 4 px of something (a layout it doesn't space,
  like the gallery's rows of buttons 8 apart).
- A key counts as a touch for the menu's auto-dismiss and for the sleep
  schedule's idle time.

## Colour and text

- Text on any fill uses that fill's `on-` role. Minimum contrast: 4.5:1
  for body text, 3:1 for text 18 px and up and for icons and outlines.
- Disabled: content at 38%, container at 12% (`theme::state`).
- Arrows are icons, not characters: the bundled Roboto has none (they
  render as "?").

## Visual QA checklist

Run on the desktop host (`cargo run --release -- --exact --page <name>
--theme <dark|light> --screenshot f.png`, plus `--scroll PX`), every page,
both themes. Then on the frame for the items marked *frame*.

1. Every left edge is on 384, 400 or 440; every trailing edge on 1240.
2. Text centred on its row to ±0.5 px against the row's icon.
3. Row heights 56/72; section headers 24 above their caps and 16 below
   their baseline, by ink (allow for ascenders and descenders).
4. One style per role across pages (headers, notes, cards).
5. Joins at 3× zoom: no seams, notches, double lines or clipped glyphs.
6. No truncated labels; no text touching a container edge.
7. Contrast per role, in both themes; disabled states distinguishable.
8. Empty values show "—", never a blank or a fake zero.
9. With keys (`--key NAME`, repeatable): every control is reached by the
   arrows and Tab, shows its focus ring whole, and scrolls into view.
10. *frame*: colours and contrast on the real panel, in daylight.
11. *frame*: text weight at 11 to 14 px with the shader boost.
12. *frame*: targets hit by finger; press state appears on touch.
