#!/usr/bin/env bash
# scripts/file-issue.sh — atomic GitHub issue filer for engram
#
# Creates an issue, adds it to the "engram v1" project, sets all single-select
# field values, then verifies by re-reading the project item. This is the only
# sanctioned way to file engram issues — never use `gh issue create` directly
# unless you also chain the project + field-edit steps and verify.
#
# Per the user's standing rule (~/.claude/CLAUDE.md): "When filing issues into
# a GitHub Project board, the wiring is multi-step: create issue, add to
# project, set status, set fields. Never chain those as separate commands you
# might forget to finish — use a single atomic filer."
#
# Usage:
#   scripts/file-issue.sh \
#     --title "Add JWT refresh token rotation" \
#     --body-file path/to/body.md \
#     --milestone "v1.0-foundation" \
#     --labels "type: feature,domain: core,P1 · high,size: M,model: sonnet-low,status: ready" \
#     --status "Up Next" \
#     --priority "P1 · high" \
#     --size "M" \
#     --risk "🟢 Low" \
#     --model "sonnet-low"
#
# All --status, --priority, --size, --risk, --model values are the single-select
# OPTION NAMES (not IDs). The script looks up IDs internally via case statements
# (compatible with macOS bash 3.2 which lacks associative arrays).

set -euo pipefail

REPO="torsday/engram"
PROJECT_OWNER="@me"
PROJECT_NUMBER=8
PROJECT_ID="PVT_kwHOAARNgc4BX6_c"

# Field IDs (hard-coded; immutable for this project)
FIELD_STATUS="PVTSSF_lAHOAARNgc4BX6_czhTEdxc"
FIELD_PRIORITY="PVTSSF_lAHOAARNgc4BX6_czhTEd3w"
FIELD_SIZE="PVTSSF_lAHOAARNgc4BX6_czhTEd30"
FIELD_RISK="PVTSSF_lAHOAARNgc4BX6_czhTEd34"
FIELD_MODEL_QUEUE="PVTSSF_lAHOAARNgc4BX6_czhTEd38"

# Option-ID lookup helpers (case statements for bash 3.2 compatibility).
# Sync with `gh project field-list 8 --owner "@me" --format json` if fields change.

status_opt_id() {
    case "$1" in
        "Backlog")     echo "d6571969" ;;
        "Up Next")     echo "c373fa25" ;;
        "In Progress") echo "1626046a" ;;
        "In Review")   echo "bbd086a7" ;;
        "On Hold")     echo "ca424e32" ;;
        "Done")        echo "fb8a175d" ;;
        *) return 1 ;;
    esac
}

priority_opt_id() {
    case "$1" in
        "P0 · critical") echo "8fb84140" ;;
        "P1 · high")     echo "71679cf0" ;;
        "P2 · medium")   echo "c4200c71" ;;
        *) return 1 ;;
    esac
}

size_opt_id() {
    case "$1" in
        "XS") echo "22f31820" ;;
        "S")  echo "6e360c3c" ;;
        "M")  echo "f0415cd7" ;;
        "L")  echo "85372fd1" ;;
        "XL") echo "31299ff0" ;;
        *) return 1 ;;
    esac
}

risk_opt_id() {
    case "$1" in
        "🔴 High")   echo "4dc685c0" ;;
        "🟡 Medium") echo "21f6bae3" ;;
        "🟢 Low")    echo "351301d2" ;;
        *) return 1 ;;
    esac
}

model_q_opt_id() {
    case "$1" in
        "sonnet-low")   echo "7a09b90a" ;;
        "opus-med")     echo "273f3e4a" ;;
        "opus-high")    echo "6e179971" ;;
        "opus-1m-max")  echo "84ad08f5" ;;
        "In Progress")  echo "8b689b4b" ;;
        "In Review")    echo "385eb87f" ;;
        "On Hold")      echo "b3cbb3a2" ;;
        "Done")         echo "6cc1b28a" ;;
        *) return 1 ;;
    esac
}

# Parse args
TITLE=""
BODY_FILE=""
MILESTONE=""
LABELS=""
STATUS="Up Next"
PRIORITY=""
SIZE=""
RISK="🟢 Low"
MODEL=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --title)      TITLE="$2"; shift 2 ;;
        --body-file)  BODY_FILE="$2"; shift 2 ;;
        --milestone)  MILESTONE="$2"; shift 2 ;;
        --labels)     LABELS="$2"; shift 2 ;;
        --status)     STATUS="$2"; shift 2 ;;
        --priority)   PRIORITY="$2"; shift 2 ;;
        --size)       SIZE="$2"; shift 2 ;;
        --risk)       RISK="$2"; shift 2 ;;
        --model)      MODEL="$2"; shift 2 ;;
        *)            echo "Unknown arg: $1" >&2; exit 1 ;;
    esac
done

# Validate required
for var in TITLE BODY_FILE MILESTONE LABELS PRIORITY SIZE MODEL; do
    eval "val=\${$var}"
    if [[ -z "$val" ]]; then
        echo "ERROR: --${var}=missing" >&2
        exit 1
    fi
done

if [[ ! -f "$BODY_FILE" ]]; then
    echo "ERROR: body file does not exist: $BODY_FILE" >&2
    exit 1
fi

# Resolve option IDs (fail fast if any are unknown)
status_id=$(status_opt_id "$STATUS")     || { echo "ERROR: unknown Status: $STATUS" >&2; exit 1; }
priority_id=$(priority_opt_id "$PRIORITY") || { echo "ERROR: unknown Priority: $PRIORITY" >&2; exit 1; }
size_id=$(size_opt_id "$SIZE")           || { echo "ERROR: unknown Size: $SIZE" >&2; exit 1; }
risk_id=$(risk_opt_id "$RISK")           || { echo "ERROR: unknown Risk: $RISK" >&2; exit 1; }
model_q_id=$(model_q_opt_id "$MODEL")    || { echo "ERROR: unknown Model: $MODEL" >&2; exit 1; }

echo "==> Creating issue: $TITLE"
issue_url=$(gh issue create \
    --repo "$REPO" \
    --title "$TITLE" \
    --body-file "$BODY_FILE" \
    --label "$LABELS" \
    --milestone "$MILESTONE")
issue_number=$(echo "$issue_url" | grep -oE '[0-9]+$')
echo "    #$issue_number  $issue_url"

echo "==> Adding to project $PROJECT_NUMBER"
item_id=$(gh project item-add "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" \
    --url "$issue_url" --format json | jq -r '.id')
echo "    item: $item_id"

echo "==> Setting field values"
set_field() {
    local field_id="$1"
    local option_id="$2"
    local field_name="$3"
    gh project item-edit \
        --id "$item_id" \
        --project-id "$PROJECT_ID" \
        --field-id "$field_id" \
        --single-select-option-id "$option_id" >/dev/null
    echo "    $field_name set"
}

set_field "$FIELD_STATUS"       "$status_id"   "Status=$STATUS"
set_field "$FIELD_PRIORITY"     "$priority_id" "Priority=$PRIORITY"
set_field "$FIELD_SIZE"         "$size_id"     "Size=$SIZE"
set_field "$FIELD_RISK"         "$risk_id"     "Risk=$RISK"
set_field "$FIELD_MODEL_QUEUE"  "$model_q_id"  "Model Queue=$MODEL"

echo "==> Verifying (re-reading project item)"
gh api graphql -f query='
query($itemId: ID!) {
  node(id: $itemId) {
    ... on ProjectV2Item {
      content {
        ... on Issue { number title }
      }
      fieldValues(first: 20) {
        nodes {
          ... on ProjectV2ItemFieldSingleSelectValue {
            field { ... on ProjectV2SingleSelectField { name } }
            name
          }
        }
      }
    }
  }
}' -F itemId="$item_id" | jq -r '
.data.node |
"    issue:  #\(.content.number)  \(.content.title)\n" +
(.fieldValues.nodes | map(select(.field.name != null)) | map("    \(.field.name): \(.name)") | join("\n"))
'

echo "==> Done: $issue_url"
