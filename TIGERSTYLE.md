# TigerStyle, for Raam

Adapted from [TigerBeetle's TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)
for a photo frame with 493 MB of RAM, a GLES2-only GPU, and no one
standing next to it when it fails. The rules bind `crates/` hardest;
hosts follow them wherever the platform allows.

## Safety

- **Crash, don't limp.** A state the code doesn't understand is a
  panic, not a fallback. On the frame, panic → `exit(70)` → the system
  relaunches the home app within a second. A **restart-loop guard**
  (persisted crash timestamps; back off when crashes cluster) keeps a
  deterministic crash from becoming a boot loop.
- **Recover only where recovery is designed.** The survivable failures
  are enumerated — GPU allocation failure (drop the plan, retry, cut
  the transition), decoder failure (backoff, skip clips), network loss
  (resync cadence) — each with a tested path. Everything else crashes.
- **Assert liberally.** Function entry preconditions, state-machine
  transitions, buffer arithmetic. Assertions stay on in release; the
  frame is the one place we can't attach a debugger, so the log line
  from a failed assert is the debugger.
- **`lock().unwrap()` is the policy**, not a smell: a poisoned lock is
  a crashed invariant.
- **Typed errors.** Error enums per fallible subsystem. Classifying an
  error by matching a string prefix is a review failure.
- **Explicit integer sizes** in the model and at every seam: `u32`,
  `i64`, never `usize` for anything that isn't an index into memory.

## Named limits

- **Every limit has a name** in `raam-model::limits`, with a comment
  saying where its value comes from (measured, chosen, or hardware).
  No bare numbers in logic: not a timeout, a retry cap, a queue depth,
  a texture size, a poll interval.
- **Everything is bounded.** Every queue has a capacity, every retry a
  cap, every cache an eviction policy, every wait a deadline. An
  unbounded anything is a bug that hasn't fired yet.

## Memory

- **Allocate up front, within a budget.** The GPU budget (tile
  textures, transition scratch) is reserved at startup, sized by the
  named limits, because the one failure mode observed in the field was
  a per-plan GPU allocation racing a transition under pressure. Heap
  allocation after startup is bounded and deliberate (photo decode
  buffers, per-clip players), never per-frame.
- **The budgets are in ARCHITECTURE.md** and asserted where they're
  spent.

## The core stays pure

- No threads, no I/O, no `std::time::Instant`/`SystemTime`, no
  `getrandom` in `raam-core` or `raam-model`. Time comes from the
  injected `Clock`; randomness is seeded from it. This is what makes
  the simulation tests deterministic and the wasm build possible — the
  same property, enforced once.
- Platform types (`AndroidApp`, JNI, winit, web-sys) never appear in
  `crates/`. If a host type wants in, it becomes a trait in the core.

## Testing

- **Simulation first.** New controller or pipeline behaviour lands with
  a deterministic simulation test driving it through virtual time,
  including its failure injection. If it can't be simulated, its seams
  are wrong.
- **Pixel changes are golden-tested.** Anything that alters rendering
  re-blesses the desktop `--exact` goldens knowingly, in its own
  commit.
- **The whole workspace tests on the dev machine.** `cargo test
  --workspace` never needs a device, an emulator, or the network.

## Dependencies

- Every dependency earns a row in ARCHITECTURE.md's register before it
  enters a `Cargo.toml`. Prefer 80 lines of our own code to a crate
  whose surface we use 2% of. No async runtimes, ever.

## Style

- `cargo fmt` settles formatting arguments; clippy runs with
  `-D warnings`.
- Comments say *why*, not *what*. A constant's comment names its
  source; a workaround's comment names what it works around and when
  it can go.
- Naming: plain words, no abbreviations that save two letters
  (`deadline`, not `dl`). Units in names where a type doesn't carry
  them (`timeout_ms`, `cap_mb`).
