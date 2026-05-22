# Installing engram

## Requirements

- **macOS 14+** (Sonoma or later) for the full experience
- **Linux x86_64** is supported for the CLI/daemon only
- An Anthropic, OpenAI, or local Ollama API key/instance for the LLM providers

## Quick install (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/torsday/engram/main/scripts/install.sh | sh
```

Supported platforms:

- macOS arm64 (Apple Silicon)
- macOS x86_64 (Intel)
- Linux x86_64

## Manual download from GitHub Releases

1. Go to [GitHub Releases](https://github.com/torsday/engram/releases/latest).
2. Download the archive for your platform:
   - `engram-<version>-aarch64-apple-darwin.tar.gz` — macOS Apple Silicon
   - `engram-<version>-x86_64-apple-darwin.tar.gz` — macOS Intel
   - `engram-<version>-x86_64-unknown-linux-gnu.tar.gz` — Linux x86_64
3. Extract and install:

```sh
tar -xzf engram-<version>-<target>.tar.gz
chmod +x engram
sudo mv engram /usr/local/bin/engram
```

## Build from source

Requires Rust stable (≥ 1.78, pinned via `rust-toolchain.toml`). Install via
[rustup](https://rustup.rs/) if needed.

```sh
git clone https://github.com/torsday/engram.git
cd engram
cargo install --path crates/engram-cli
```

The binary is installed to `~/.cargo/bin/engram`. Ensure `~/.cargo/bin` is in
your `PATH`.

## macOS — Homebrew tap (coming soon)

A Homebrew tap is planned for the beta release:

```sh
# Not yet available — use the install script above
brew tap torsday/engram
brew install engram
```

## Vault setup

After installing, create and initialise a vault:

```sh
mkdir ~/my-vault
engram migrate --vault ~/my-vault
```

This creates `.engram/` inside the vault directory with the SQLite index, vector
store, and config file.

## Configuration

The config file lives at:

```
~/my-vault/.engram/config.toml
```

Edit it to set your preferred LLM provider, embedding model, and other options.
See `engram config --help` for available keys.

**Caveats on Linux:**

- Secret storage uses the system keychain on macOS. On Linux, secrets fall back to
  an age-encrypted file at `~/.config/engram/secrets.age`.
- Time Machine backup probing (`engram backup verify`) requires macOS; the layer
  reports "n/a" on Linux.

## Verify install

```sh
engram --version
engram status
```

If the binary is on your `PATH` and the process exits cleanly, the install
succeeded.
