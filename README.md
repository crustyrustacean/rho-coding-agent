# rho-coding-agent

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./License.txt)
[![Rust 2024 Edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

A coding agent written in Rust.

## Features

- 🦀 **Built in Rust** — fast, safe, and reliable
- 📦 **Workspace layout** — core library (`rho-core`) and dev task runner (`xtask`)
- 🔧 **`cargo xtask`** — unified dev workflow for linting, building, testing, and releasing
- 📝 **Conventional commits** — automated changelogs via [git-cliff](https://git-cliff.org)
- 🚀 **Streamlined releases** — version bumps and publishing via [cargo-release](https://github.com/crate-ci/cargo-release)

## Installation

```sh
cargo install --path rho-core
```

## Usage

```sh
rho-core
```

## Development

This project uses the [cargo-xtask](https://github.com/matklad/cargo-xtask) pattern. All development workflows are driven through `cargo xtask`:

```sh
cargo xtask fmt               # Check formatting
cargo xtask lint              # Run Clippy lints
cargo xtask build             # Build all workspace crates
cargo xtask test              # Run all tests
cargo xtask ci                # Full CI pipeline (fmt → lint → build → test)
cargo xtask changelog         # Generate CHANGELOG.md (unreleased)
cargo xtask changelog 0.2.0   # Generate CHANGELOG.md for a specific version
```

### Project layout

```
rho-core/     # Core library
xtask/        # Dev task runner (not published)
```

### Releasing

```sh
cargo xtask changelog <version>          # Update CHANGELOG.md
git add -A && git commit -m "chore(release): prepare <version>"
cargo release <version>                  # Bump version, tag, push & publish when ready
```

## Contributing

Contributions are welcome! Please follow [conventional commits](https://www.conventionalcommits.org/) when submitting pull requests:

- `feat:` — new features
- `fix:` — bug fixes
- `docs:` — documentation changes
- `refactor:` — code changes that neither fix bugs nor add features
- `test:` — adding or updating tests
- `chore:` — maintenance tasks (tooling, CI, dependencies)

Run `cargo xtask ci` before opening a PR to ensure all checks pass.

## License

This project is licensed under the [MIT License](./License.txt).
