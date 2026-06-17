#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/raven-claude-runner-smoke.$$"

cleanup() {
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

assert_contains() {
  local needle="$1"
  local file="$2"

  if ! grep -Fq -- "$needle" "$file"; then
    echo "Expected $file to contain: $needle" >&2
    echo "--- $file" >&2
    cat "$file" >&2
    exit 1
  fi
}

mkdir -p "$TMP_ROOT/stub-bin" "$TMP_ROOT/target"

cat >"$TMP_ROOT/stub-bin/claude" <<'SH'
#!/bin/sh
set -eu
: "${CAPTURE_DIR:?CAPTURE_DIR is required}"
printf '%s\n' "$@" > "$CAPTURE_DIR/claude.args"
cat > "$CAPTURE_DIR/claude.stdin"
printf 'claude runner ok\n'
SH
chmod +x "$TMP_ROOT/stub-bin/claude"

PATH="$TMP_ROOT/stub-bin:$PATH" \
CAPTURE_DIR="$TMP_ROOT" \
RAVEN_TOOL_WORKDIR="$TMP_ROOT/target" \
RAVEN_CLAUDE_MODEL="sonnet" \
"$ROOT/bin/raven-claude-runner" <<'JSON' >"$TMP_ROOT/local.out"
{"route":"tool_now","text":"inspect git"}
JSON

assert_contains "claude runner ok" "$TMP_ROOT/local.out"
assert_contains "--print" "$TMP_ROOT/claude.args"
assert_contains "--output-format" "$TMP_ROOT/claude.args"
assert_contains "--permission-mode" "$TMP_ROOT/claude.args"
assert_contains "--model" "$TMP_ROOT/claude.args"
assert_contains "sonnet" "$TMP_ROOT/claude.args"
assert_contains "$TMP_ROOT/target" "$TMP_ROOT/claude.stdin"
assert_contains '"inspect git"' "$TMP_ROOT/claude.stdin"

cat >"$TMP_ROOT/stub-bin/curl" <<'SH'
#!/bin/sh
set -eu
: "${CAPTURE_DIR:?CAPTURE_DIR is required}"
printf '%s\n' "$@" > "$CAPTURE_DIR/curl.args"
cat > "$CAPTURE_DIR/curl.stdin"
printf 'endpoint runner ok\n'
SH
chmod +x "$TMP_ROOT/stub-bin/curl"

PATH="$TMP_ROOT/stub-bin:$PATH" \
CAPTURE_DIR="$TMP_ROOT" \
RAVEN_CLAUDE_ENDPOINT="http://claude-worker:8765/run" \
RAVEN_CLAUDE_ENDPOINT_TOKEN="secret" \
"$ROOT/bin/raven-claude-runner" <<'JSON' >"$TMP_ROOT/endpoint.out"
{"route":"background","text":"run ax2"}
JSON

assert_contains "endpoint runner ok" "$TMP_ROOT/endpoint.out"
assert_contains "-X" "$TMP_ROOT/curl.args"
assert_contains "POST" "$TMP_ROOT/curl.args"
assert_contains "Authorization: Bearer secret" "$TMP_ROOT/curl.args"
assert_contains "http://claude-worker:8765/run" "$TMP_ROOT/curl.args"
assert_contains '"run ax2"' "$TMP_ROOT/curl.stdin"
