# Contributing to stillvalid

Thanks for helping. Start with [CLAUDE.md](CLAUDE.md) for context and rules, and [docs/ROADMAP.md](docs/ROADMAP.md) for open work.

## Build and test

```sh
cargo build
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo deny check   # optional locally; CI runs it
```

## Pull requests

1. Open an issue first for anything larger than a small fix.
2. Keep PRs focused on one change, with tests.
3. CI must pass on Linux, macOS and Windows.
4. Record design decisions in [docs/DECISIONS.md](docs/DECISIONS.md) and tick finished items in [docs/ROADMAP.md](docs/ROADMAP.md).
5. Commit messages: imperative mood, lowercase start (e.g. `add retry to fetcher`).

## Rules that are not up for debate

See "Non-negotiables" in [CLAUDE.md](CLAUDE.md): every verdict carries evidence, and stillvalid is read-only by default.

## Security

Report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

By contributing, you agree your work is dual licensed under MIT OR Apache-2.0, as described in the [README](README.md#license).

This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
