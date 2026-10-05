---
name: raam-tigerstyle
description: How TIGERSTYLE.md and NASA/JPL's Power of 10 are enforced in raam — which lint, CI step, Miri job or review covers each rule, the known gaps, raam's canonical examples to copy (named limits, typed error enums, cast helpers, must_use reasons, SAFETY notes, entry asserts), the bugs each rule caught here, and the review order that found them. Use it whenever you write or review Rust in raam (crates/, hosts/, src/), run a TigerStyle or Power of 10 review, add a limit, an error type, an `unsafe` block, a cast or a lint, or touch CI's clippy or Miri steps — alongside the general tigerstyle-rust skill if it is installed.
---

# TigerStyle in raam

[TIGERSTYLE.md](../../../TIGERSTYLE.md) is the discipline;
[AGENTS.md](../../../AGENTS.md) holds the "never" list. Both win over
this skill. This skill says **how each rule is checked here**, what
isn't checked yet, and which code to copy. If the general
`tigerstyle-rust` skill is installed, this is its raam supplement, and
this one wins where they differ. The vendored `rust-skills` comes
after both (see its precedence note).

## Writing

- **A new limit** goes in `crates/raam-model/src/limits.rs`, under its
  `// ---- section ----` banner. Its doc comment ends with its source
  (*Measured*, *Chosen*, or the hardware). Copy `ALARM_RETRY`:

  ```rust
  /// A wake alarm that failed to set is tried again after this long, the
  /// screen staying on meanwhile. Chosen: soon enough that a passing fault
  /// costs a minute of screen, slow enough that a lasting one doesn't spin
  /// the loop or flood the log.
  pub const ALARM_RETRY: Duration = Duration::from_secs(60);
  ```

- **A new fallible path** gets a variant in its subsystem's error
  enum. Copy `LibraryError` (`crates/raam-engine/src/library.rs`):
  - written by hand, no `thiserror`;
  - a doc comment on every variant;
  - context fields such as `Db { doing, source }` and
    `Io { doing, path, source }`;
  - behaviour hangs off a predicate (`is_offline()`), never off the
    message text.

  `Result<_, String>` remains in the hosts and raam-core's seams; that
  is raam#94. Don't add more.
- **A loop following outside data** is capped and fails at the cap,
  as `immich.rs`'s album paging does (`for _ in 0..limits::IMMICH_MAX_PAGES`,
  then an error, never a partial list). A raw body reader
  (`body_mut().as_reader()`) gets `Read::take(max_bytes + 1)`, because
  ureq's raw reader has no limit. `read_json`/`read_to_vec` stop at
  ureq's own 10 MB, which is enough for small payloads; anything
  bigger, or anything written to storage, gets a named limit.
- **A queue** says what happens when it's full (`App::queue_key`:
  drop the key and log a warning at `MAX_QUEUED_KEYS`).
- **A precondition** is an entry assert with the values in the
  message, plus a `# Panics` section if the fn is public. Examples:
  `source.rs:shrink_to_cover` (empty tile) and
  `slideshow.rs:upload_size` (RGBA length). A layout fact is a
  `const _: () = assert!(..)`, as at the top of `painter.rs`.
- **A value that leaks or stalls if dropped** gets
  `#[must_use = "<what breaks>"]`. See `video.rs`: "stop() it: a
  dropped probe leaks its decoder, and no clip opens again". A
  deliberate discard is written `drop(..)`.
- **A cast:**
  - use `From` where the value can't be lost;
  - the `raam_core::num::sat_*` / `to_f32` helpers where saturating
    is the point;
  - `gl::gl_sizei` / `gl::from_gl_size` / `gl::gl_enum_param` and
    `db::sql_int` (`#[track_caller]`, panicking with the value)
    where it must fit;
  - `cast_signed()` / `cast_unsigned()` for bit patterns;
  - libc aliases (`time_t`, `statvfs`) keep `as`, under an
    `#[allow(.., reason)]` that names the target.
- **An `unsafe` block** gets a `// SAFETY:` comment directly above it,
  naming the invariant (see the GL context note in `gl.rs`). Fields
  that hold raw handles document their drop order (see `src/main.rs`'s
  `Gl`).
- **A suppression** is `#[expect(lint, reason = "...")]`. It is
  `#[allow]` only when the lint fires on some of CI's targets and not
  others.

Before pushing, run the four clippy runs CI does (below),
`cargo test --workspace`, and the general skill's `scripts/audit.sh`
on the files you touched, if you have it.

## Enforcement map

| Rule | How raam checks it today |
|---|---|
| P1: simple control flow, no recursion | Review only |
| P2: every loop bounded | Review, plus named limits |
| P3: no allocation after init on hot paths | Review only. The budget constants (`APP_TOTAL_STEADY_MB`, `MALI_PEAK_MB`, `TILE_TEXTURES_PER_PLAN_MB`, `TRANSITION_SCRATCH_TARGETS`) are declared but **asserted nowhere** |
| P4: short functions | Review only (`too_many_lines` off: 30 sites) |
| P5: assertions | `missing_panics_doc` (raam-engine opts out crate-wide); density is review only |
| P6: smallest scope | Review only |
| P7: return values checked | rustc `unused_must_use` and `#[must_use]` with reasons. `let_underscore_must_use` and `must_use_candidate` are off |
| P8: few macros and `cfg` | One `macro_rules!` (`ids.rs`). Every `cfg` target has a clippy run in CI |
| P9: restricted `unsafe` | `undocumented_unsafe_blocks`. A Miri job on pinned nightly covers `raam-model` and `raam-core`'s `gl::` raw-pointer helpers |
| P10: all warnings, zero tolerated | `-D warnings` on four clippy runs: native `--all-targets`, `--features gles`, wasm32 `raam-web`, armv7 `raam-android`. Pedantic: only the four cast lints and `missing_panics_doc`. **No scheduled run** (raam#111) |
| Typed errors | Review only |
| Explicit integer sizes | The four cast lints, plus review for `usize` in the model |
| Named limits | Review only (`unreadable_literal` off) |
| Pure core | Review only. The wasm32 build catches some, but `Instant` panics at runtime on wasm instead of failing the build |
| Crash, don't limp | Android panic hook (`exit(70)`, the system relaunches). **No restart-loop guard yet** |
| Simulation first, goldens | `cargo test --workspace`; `goldens-own-commit.sh` in CI; the pixel check runs locally |
| Dependencies | `cargo deny check` in CI; the register row is review |
| Formatting | `cargo fmt --all --check` in CI; the opt-in pre-commit hook |

## Known gaps

When a change touches one of these, say so in the PR. Closing one is a
PR of its own.

1. **P10 rule 10.**
   - Full pedantic is off: about 650 native sites over 40 lints, led
     by `must_use_candidate`, `cast_precision_loss`,
     `unreadable_literal`, `doc_markdown` and `missing_errors_doc`.
   - No scheduled CI run: no daily pedantic or restriction report, no
     fresh advisory check, no wider Miri.
   - The plan is raam#111.
2. **The restart-loop guard** that TIGERSTYLE.md describes doesn't
   exist. A deterministic crash at startup restarts about once a
   second.
3. **The memory budgets aren't asserted** where they're spent.
4. **`Result<_, String>`** in the hosts and raam-core's seams (raam#94).
5. **The pure core isn't machine-checked.** It needs `disallowed-types`
   for `Instant`/`SystemTime` and `disallowed-methods` for
   `thread::spawn`, scoped to `raam-core` and `raam-model`, or a CI
   grep.
6. **`let _ =` on `Result`s** outside tests, without a reason, e.g. in
   `library.rs` and `db.rs`. Also: should a failed DB read crash or
   fall back to a default? That is raam#45, waiting on a decision.
7. **Bare `#[allow]`s with no reason,** in `gl.rs`, `theme.rs`,
   `transitions.rs`, `egl.rs` and `webgl.rs`.
8. **Unnamed threads.** The fetch, library, writer and weather workers
   and the Android player threads are started with `thread::spawn`.
   The panic hook therefore logs "panic on thread ?". Only `wifi.rs`
   uses `thread::Builder` with a name.

## Reviewing: the order that has paid off here

This is the general skill's review order, with raam's evidence. The
items it lists that aren't here (errors classified by string, budgets
declared but not asserted) still apply. Read each file whole, then
check in this order. Each item lists the PRs where the check found
real bugs.

1. **Swallowed errors**, and what the default then causes:
   - #38: a failed DB read made the sweep delete cache files and
     write a short export;
   - #28: a failed DELETE still unlinked the files;
   - #95: every sync error showed as "offline".
2. **Unbounded anything:**
   - #39: clip downloads had no byte limit, and album paging had no
     page cap;
   - #63: a wake alarm retried every 50 ms;
   - #53: the key queue;
   - #50: the WebGL name table.
3. **Ordering bugs a new bound creates.** #39: a download cut short
   at the cap failed the probe.
4. **`unsafe` and `as`:**
   - #82: writing the SAFETY notes found a drop-order bug and the
     audio buffer race fixed in #83;
   - #72: UB fixes;
   - #49, #54, #91: casts of outside data that wrapped or panicked.
5. **Catch-alls on owned enums.** #48: an unknown transition name
   became a fade.
6. **Missing entry asserts.** #64: a 0×0 tile hung the fetch thread;
   #57: RGBA length.
7. **Bare numbers** that belong in `limits.rs` (#43, #54).
8. **Duplicated loops; long functions.**

How to run it:

- Report findings before opening PRs.
- Make PRs one topic each, and give each fix a test that fails on the
  old code. A std `TcpListener` serves fake HTTP; a SQLite trigger
  makes a write fail; the simulation tests drive virtual time, as
  `video.rs`'s `a_stopped_decoder_never_released_keeps_clips_backed_off`
  does.
- Before merging PRs branched in parallel, trial-merge each one onto
  the current main and run all four clippy runs and the tests. Two
  PRs can each pass CI and still break main together.
- A change that alters pixels re-blesses goldens in a commit of its
  own (AGENTS.md).
