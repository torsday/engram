#!/usr/bin/env bash
# gen-mcp-docs.sh — regenerate docs/api/mcp.md from MCP tool source comments.
#
# Usage:
#   scripts/gen-mcp-docs.sh            # write to docs/api/mcp.md
#   scripts/gen-mcp-docs.sh --check    # exit 1 if committed content would change
#
# The script extracts the doc-comment block at the top of each tool's source
# file (the lines between the first `//!` and the first non-`//!` line) and
# reassembles them into the reference doc.
#
# Limitations:
#   - Reads from crates/engram-mcp/src/*.rs (excluding lib.rs).
#   - Tool order follows the order of modules declared in lib.rs.
#   - Does not auto-generate REST or CLI docs (those are stubs pending implementation).
#
# Called by the `docs-drift` CI job in .github/workflows/ci.yml.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MCP_SRC="${REPO_ROOT}/crates/engram-mcp/src"
OUT="${REPO_ROOT}/docs/api/mcp.md"
CHECK_MODE=false

if [[ "${1:-}" == "--check" ]]; then
  CHECK_MODE=true
fi

# Extract tool order from lib.rs (modules declared with real files, not empty stubs)
TOOL_ORDER=()
while IFS= read -r line; do
  if [[ "$line" =~ ^pub\ mod\ ([a-z_]+)\; ]]; then
    mod="${BASH_REMATCH[1]}"
    if [[ "$mod" != "server" ]] && [[ -f "${MCP_SRC}/${mod}.rs" ]]; then
      TOOL_ORDER+=("$mod")
    fi
  fi
done < "${MCP_SRC}/lib.rs"

if [[ ${#TOOL_ORDER[@]} -eq 0 ]]; then
  echo "gen-mcp-docs: no tool modules found in lib.rs" >&2
  exit 1
fi

# Build the header
HEADER='# MCP tool reference

Engram exposes a set of MCP (Model Context Protocol) tools that Claude Desktop
and Claude Code can call directly. These tools let Claude read your vault, search
notes, inspect links, and check system health — all without leaving your machine.

The MCP server starts automatically with `engram serve` and is available on the
stdio transport (configured in `~/.config/claude/claude_desktop_config.json`).

> **Auto-generated** — this file is regenerated from source by
> `scripts/gen-mcp-docs.sh`. Do not edit by hand; run the script and commit the
> result. CI fails if the committed content diverges from the generated output.

---'

# Extract doc comments from each tool file
generate_content() {
  echo "$HEADER"
  echo ""

  for mod in "${TOOL_ORDER[@]}"; do
    src="${MCP_SRC}/${mod}.rs"
    # Extract lines starting with //! until the first blank line or non-//! line
    awk '
      /^\/\/!/ { print substr($0, 5); next }
      /^$/ && found { exit }
      /^\/\/!/ { found=1 }
      !found && !/^\/\/!/ { exit }
    ' "$src"
    echo ""
    echo "---"
    echo ""
  done
}

GENERATED="$(generate_content)"

if $CHECK_MODE; then
  CURRENT="$(cat "$OUT" 2>/dev/null || echo "")"
  if [[ "$GENERATED" == "$CURRENT" ]]; then
    echo "gen-mcp-docs: docs/api/mcp.md is up to date"
    exit 0
  else
    echo "gen-mcp-docs: docs/api/mcp.md is stale — run scripts/gen-mcp-docs.sh and commit" >&2
    diff <(echo "$CURRENT") <(echo "$GENERATED") || true
    exit 1
  fi
fi

mkdir -p "$(dirname "$OUT")"
echo "$GENERATED" > "$OUT"
echo "gen-mcp-docs: wrote ${OUT}"
