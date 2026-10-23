# Contributing

- `cargo xtask check` and `cargo xtask test` must pass before pushing.
- Coverage: `cargo xtask coverage` — cargo-llvm-cov HTML report in
  `target/llvm-cov/html`; pass `--fail-under-lines 100` to gate.
- Keep `cicada-core` pure: no I/O imports, ports only.
- Architecture changes land as ADRs under `project/adrs/` first.
