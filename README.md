# engram

Your thoughts, encoded. A living knowledge base that rewrites itself.

---

## Status

Early implementation. The Rust workspace scaffold is in place; all crates compile but contain module stubs only. See [`docs/design/README.md`](docs/design/README.md) for the corpus index and reading order, and [`SPEC.md`](SPEC.md) for v1 acceptance criteria.

## Building

Requires a recent stable Rust (pinned via `rust-toolchain.toml`).

```sh
cargo build --workspace   # build all crates
cargo test --workspace    # run all tests
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

The binary is `engram-cli`; after building it lives at `target/debug/engram`.

## Copyright

Copyright (c) 2026 Torsday. All rights reserved.

This repository is source-visible but **not open-source**. There is no `LICENSE` file. No use, modification, or redistribution is granted. A license may be added in the future.
