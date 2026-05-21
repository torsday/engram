# Installing engram

## Requirements

- **macOS 14+** (Sonoma or later) for the full experience including the SwiftUI app
- **Linux** is supported for the CLI daemon only (no SwiftUI app)
- **Rust stable** (≥ 1.78) — pinned via `rust-toolchain.toml` in the repo
- An Anthropic, OpenAI, or local Ollama API key/instance for the LLM providers

## macOS — Cargo install (recommended during alpha)

```sh
# 1. Install Rust if you don't have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Install engram
cargo install engram-cli

# 3. Verify
engram status
```

Expected output:

```
engram v0.1.0
vault:    not configured (run `engram init`)
provider: not configured
index:    not running
```

## macOS — Homebrew tap (coming soon)

A Homebrew tap is planned for the beta release. Check back after the first tagged
release:

```sh
# Not yet available — use cargo install above
brew tap torsday/engram
brew install engram
```

## macOS — SwiftUI app (coming soon)

The universal SwiftUI app (iOS + macOS) is under development. When available it
will be distributed via TestFlight for alpha testers and the Mac App Store for
general availability.

To build from source today:

```sh
git clone https://github.com/torsday/engram
open ios/Engram.xcodeproj
# Select your signing team and run
```

## Linux — Cargo install

```sh
cargo install engram-cli
engram status
```

**Caveats on Linux:**

- The SwiftUI app is not available.
- Time Machine backup probing (`engram backup verify`) requires macOS; the layer
  reports "n/a" on Linux.
- Secret storage uses the system keychain on macOS. On Linux, secrets fall back to
  an age-encrypted file at `~/.config/engram/secrets.age`.

## Verify install

After any install method:

```sh
engram status
```

If the binary is on your `PATH` and the process exits cleanly, the install
succeeded. Run `engram init` to configure your first vault.
