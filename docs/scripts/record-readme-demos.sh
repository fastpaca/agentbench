#!/bin/bash
# Record all demo GIFs for the README
#
# Usage:
#   cd examples/membench_qa_test
#   ../../docs/scripts/record-readme-demos.sh
#
# Prerequisites:
#   brew install asciinema agg
#   cargo build --release (or the script will do it)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TUI_DEMO="$SCRIPT_DIR/tui-demo"
OUTPUT_DIR="$REPO_ROOT/docs/images"
DEMO_COLS=140
DEMO_ROWS=40
DEMO_THEME="dracula"
DEMO_FONT_SIZE=10
DEMO_ARGS=(--cols "$DEMO_COLS" --rows "$DEMO_ROWS" --theme "$DEMO_THEME" --font-size "$DEMO_FONT_SIZE")

# Ensure we're in an example directory
if [[ ! -f "pacabench.yaml" ]]; then
    echo "Error: Run this from an example directory (e.g., examples/membench_qa_test)"
    exit 1
fi

# Build pacabench
echo "Building pacabench..."
cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --release --bin pacabench -q
PACABENCH="$REPO_ROOT/target/release/pacabench"

# Add to PATH so tui-demo can find it
export PATH="$(dirname "$PACABENCH"):$PATH"

echo ""
echo "Recording README demos..."
echo "Output: $OUTPUT_DIR"
echo ""

# Ensure colors are enabled in recorded output.
unset NO_COLOR

# Demo 1: Running a benchmark with --limit
echo "==> Recording: simple-run.gif (pacabench run --limit 15)"
"$TUI_DEMO" \
    -o "$OUTPUT_DIR/simple-run.gif" \
    "${DEMO_ARGS[@]}" \
    --pause 3 \
    -- "pacabench run --limit 15"

echo ""

# Demo 2: List all runs
echo "==> Recording: simple-show.gif (pacabench show)"
"$TUI_DEMO" \
    -o "$OUTPUT_DIR/simple-show.gif" \
    "${DEMO_ARGS[@]}" \
    --pause 2.5 \
    -- "pacabench show"

echo ""

# Demo 3: Show specific run details
echo "==> Recording: simple-show-detail.gif (pacabench show <id>)"

# Get the most recent completed run ID
LATEST_RUN=$($PACABENCH show 2>/dev/null | grep completed | head -1 | awk '{print $1}')
if [[ -z "$LATEST_RUN" ]]; then
    echo "Error: No completed runs found. Run 'pacabench run --limit 15' first."
    exit 1
fi

# Use the last 8 chars of the run id (nanoid can include hyphens)
SHORT_ID="${LATEST_RUN: -8}"

"$TUI_DEMO" \
    -o "$OUTPUT_DIR/simple-show-detail.gif" \
    "${DEMO_ARGS[@]}" \
    --pause 2.5 \
    -- "pacabench show $SHORT_ID"

echo ""
echo "Done! Generated:"
echo "  $OUTPUT_DIR/simple-run.gif"
echo "  $OUTPUT_DIR/simple-show.gif"
echo "  $OUTPUT_DIR/simple-show-detail.gif"
