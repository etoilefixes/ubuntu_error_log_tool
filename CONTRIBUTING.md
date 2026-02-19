# Contributing

Thanks for your interest in contributing to `logtool`.

## Development Setup

1. Install Rust stable toolchain.
2. Clone the repository.
3. Build and test:

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features
```

## Code Style

- Keep code changes focused and small.
- Prefer clear error messages for CLI and daemon responses.
- Run `cargo fmt` before committing.

## Commit Guidelines

- Use concise commit messages.
- Suggested format:
  - `feat: ...`
  - `fix: ...`
  - `docs: ...`
  - `chore: ...`

## Pull Request Checklist

- [ ] Code compiles with `cargo build`.
- [ ] Tests pass with `cargo test`.
- [ ] Lints pass with `cargo clippy --all-targets --all-features`.
- [ ] Docs updated if behavior changed.

