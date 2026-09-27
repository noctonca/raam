# The owner's checks on the frame

Put the frame on the page first (with the app's `page` and `theme`
props). Ask everything in one `AskUserQuestion`, with a sentence of
instructions in each question. These are the things the desktop can't
settle; everything else was measured already.

1. **Press and timing.** Tap each new control. Does the press shade show
   under the finger, and does the result feel instant (within 400 ms,
   Doherty)?
2. **Long hold.** Hold each new control for 2 s or more, then lift. Does it
   act on lift, with the press shade on the whole time? (egui's 0.8 s
   long-touch bug showed up only here.)
3. **Scroll against tap.** Start a drag on a row and scroll. Does nothing
   toggle or open?
4. **Legibility, both themes, at arm's length.** Check the smallest text
   (supporting lines, labelSmall), the coloured headers and the notes. Is
   anything thin, washed out or hard to read?
5. **Target size.** If targets are new or denser: on the Touch targets page,
   tap the lit square about 10 times fast per row. Which is the smallest
   size hit reliably? This confirms or raises the 48 px rule.
6. **Disabled states.** Can you tell disabled from enabled at a glance,
   and is disabled text still readable?
7. **Anything the new UI adds:** animation smoothness, a dialog's
   dismissal, text entry with egui_keyboard, performance with the
   slideshow underneath.

Read the answers closely. "Works, but only after I lift" is expected for
taps; "a hold then lift does nothing" is a bug.
