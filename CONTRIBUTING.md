# How to contribute

Raam is young — one maintainer, a design, and a port in progress from a
private lab of 28 experiments. Contributions are welcome, and the bar
is the same for everyone (AI-assisted or not): the code has to be
correct, the scope has to match what the PR claims, and the pixel
match across hosts has to hold.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[TIGERSTYLE.md](TIGERSTYLE.md) first; point your assistant at
[AGENTS.md](AGENTS.md).

## Doing good work here

- **Own what you ship.** If an assistant wrote it, you are still the
  author: read the diff, be able to defend every line.
- **Actually test your change.** Build it, run the host it touches,
  walk the path you changed and one you didn't. `cargo test
  --workspace` passing is necessary, not sufficient — the real device
  has failure modes no CI runner reproduces.
- **Ship a test, or ship a repro hook.** Pure logic gets a unit or
  simulation test; behaviour that can't be automated gets a debug
  switch that re-triggers it.
- **Smaller is better.** One topic per PR; resist "while I'm here"
  cleanups. Anything sweeping deserves an issue first.
- **Explain why, not what.** In commits, PRs and comments alike — the
  diff already shows the what.
- **CI is on your side.** When it fails, fix the cause; don't skip a
  hook or disable a check to unblock yourself.

## Conventions

PR titles follow Conventional Commits (`feat`, `fix`, `perf`,
`refactor`, `docs`, `test`, `build`, `ci`, `chore`) — CI checks this.
`cargo fmt` and clippy (`-D warnings`) gate merges.

## Bugs and ideas

Open an issue. For frame bugs, say which device, and attach the logcat
lines around the failure if you can get them.
