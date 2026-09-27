# Agent guidance — Raam

Project context for AI assistants (and a fine crib for humans). The
architecture is [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); the coding
discipline is [TIGERSTYLE.md](TIGERSTYLE.md). Read both before touching
`crates/`.

## Principles

- **The frame is the truth.** The product runs on a 493 MB Android 6
  frame with a GLES2-only GPU and nobody watching the log. Desktop and
  web exist to serve that device, not the other way round.
- **One renderer, one pixel truth.** The same GL code and shaders run
  on all three targets; the desktop goldens and the web diff will
  enforce it (the golden suite is next in
  [docs/plan/next.md](docs/plan/next.md)). A change that renders
  differently per host is wrong even if it looks fine.
- **The design is written down.** If a change contradicts
  ARCHITECTURE.md, the doc changes first (or the change is wrong).

## Never

- Never add a dependency without its row in ARCHITECTURE.md's
  dependency register. Never add an async runtime at all.
- Never put threads, I/O, `std::time::Instant`/`SystemTime`, or a
  platform type into `raam-core` or `raam-model`.
- Never hard-code a limit; name it in `raam-model::limits`.
- Never classify an error by string matching.
- Never commit secrets, LAN addresses, or device identifiers: this repo
  is public.
- Never cite what a reader can't see (private notes, a past working
  session, a numbered experiment) in a comment, doc or commit message;
  state the fact itself.
- Never weaken a failure-recovery path (GPU allocation, decoder
  backoff, the single-decoder rule) without re-running its injection
  test — these paths were bought with real debugging on real hardware.
- Never skip hooks or CI (`--no-verify`), and never bless changed
  pixel goldens as a side effect of an unrelated change.

## Build & test

```sh
cargo check --workspace      # green on any dev machine
cargo test  --workspace      # no device, no network
cargo clippy --workspace --all-targets   # -D warnings in CI
cargo fmt --all
```

[docs/BUILDING.md](docs/BUILDING.md) has each host's prerequisites.
Android builds use cargo-apk2 from `hosts/android` (workspace target
dir); `scripts/env-check.sh` verifies the NDK, signing env and device
setup first (personal values live in the untracked `.env`, never
here). The web build has its own script under `hosts/web` (wasm32 +
wasm-bindgen + wasm-opt). APK releases are manual for now.

## When modifying X, do Y

- **Rendering or shaders** → re-bless the desktop `--exact` goldens
  deliberately, in their own commit; run the web pixel diff.
- **The controller or pipeline states** → extend the simulation test
  alongside; failure injection included.
- **Settings** → one type in `raam-model`; add the DB migration and a
  round-trip test.
- **Anything with a limit, timeout, retry or capacity** → name it in
  `limits.rs` with its provenance.
- **Docs** → sentence-case headings; link from `docs/` to source with
  relative paths; a plan for future work goes in `docs/plan/`.

## Commit & PR conventions

Conventional Commits, CI-enforced on PR titles: `feat`, `fix`, `perf`,
`refactor`, `docs`, `test`, `build`, `ci`, `chore`. Commit messages and
PR bodies explain *why* — the constraint hit, the alternative rejected.
Keep PRs to one topic.
