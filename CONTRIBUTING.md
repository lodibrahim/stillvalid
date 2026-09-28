# Contributing to stillvalid

Thanks for helping. Start with [AGENTS.md](AGENTS.md) for context and rules, and [docs/ROADMAP.md](docs/ROADMAP.md) for open work.

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

## Releasing

Maintainers only. [dist](https://github.com/axodotdev/cargo-dist) builds and publishes a release when a `vX.Y.Z` tag is pushed (`.github/workflows/release.yml`).

1. Bump `version` in `Cargo.toml`, run `cargo build` to update `Cargo.lock`, and add a `## X.Y.Z - YYYY-MM-DD` entry to [CHANGELOG.md](CHANGELOG.md) (it becomes the release notes). Merge that to `main`.
2. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Move the Action's major tag so `uses: lodibrahim/stillvalid@v1` gets the new `action.yml`: `git tag -f v1 && git push -f origin v1`.

The release workflow only runs on tags with three version numbers, so pushing `v1` does not start a release.

## Rules that are not up for debate

See "Non-negotiables" in [AGENTS.md](AGENTS.md): every verdict carries evidence, and stillvalid is read-only by default.

## Security

Report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

By contributing, you agree your work is dual licensed under MIT OR Apache-2.0, as described in the [README](README.md#license).

This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
