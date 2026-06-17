#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/freeq-raven-railway-smoke.$$"

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

assert_not_contains() {
	local needle="$1"
	local file="$2"

	if grep -Fq -- "$needle" "$file"; then
		echo "Expected $file not to contain: $needle" >&2
		echo "--- $file" >&2
		cat "$file" >&2
		exit 1
	fi
}

mkdir -p "$TMP_ROOT/source" "$TMP_ROOT/stub-bin" "$TMP_ROOT/host/bin" "$TMP_ROOT/axbin"

cp "$ROOT/bin/freeq-raven-railway" "$TMP_ROOT/host/bin/freeq-raven-railway"

cat >"$TMP_ROOT/host/bin/freeq-raven" <<'SH'
#!/bin/sh
sleep 0.2
SH
chmod +x "$TMP_ROOT/host/bin/freeq-raven"

cat >"$TMP_ROOT/stub-bin/curl" <<SH
#!/bin/sh
printf '%s\n' "\$@" > "$TMP_ROOT/curl.args"
printf 'exit 0\n'
SH
chmod +x "$TMP_ROOT/stub-bin/curl"

cat >"$TMP_ROOT/stub-bin/bash" <<SH
#!/bin/sh
printf '%s\n' "\$@" > "$TMP_ROOT/install.args"
exit 0
SH
chmod +x "$TMP_ROOT/stub-bin/bash"

cat >"$TMP_ROOT/axbin/ax" <<SH
#!/bin/sh
printf '%s\n' "\$*" >> "$TMP_ROOT/ax.calls"

case "\${1:-}" in
  init)
    if [ ! -f .alexandria-next/alexandria-config.json ]; then
      mkdir -p .alexandria-next
      printf '{"schemaVersion":1,"workspace":"%s"}\n' "\${4:-}" > .alexandria-next/alexandria-config.json
    fi
    ;;
  inspect)
    printf '{}\n'
    ;;
  start)
    sleep 0.1
    ;;
  internal)
    sleep 0.1
    ;;
esac
SH
chmod +x "$TMP_ROOT/axbin/ax"

git init -q -b main "$TMP_ROOT/source"
mkdir -p "$TMP_ROOT/source/.alexandria-next"
printf '{"schemaVersion":1,"workspace":"docs/alexandria"}\n' >"$TMP_ROOT/source/.alexandria-next/alexandria-config.json"
git -C "$TMP_ROOT/source" config user.email "raven-smoke@example.invalid"
git -C "$TMP_ROOT/source" config user.name "Raven Smoke"
git -C "$TMP_ROOT/source" add .alexandria-next/alexandria-config.json
git -C "$TMP_ROOT/source" commit -q -m "seed repo"

PATH="$TMP_ROOT/stub-bin:$PATH" \
	ALEXANDRIA_INSTANCE_ID="alexandria-wedo" \
	ALEXANDRIA_PROJECT_REPO="$TMP_ROOT/source" \
	ALEXANDRIA_PROJECT_BRANCH="main" \
	ALEXANDRIA_DATA_DIR="$TMP_ROOT/data" \
	ALEXANDRIA_AX2_INSTALL_DIR="$TMP_ROOT/axbin" \
	ALEXANDRIA_NEXT_ACP_PROVIDER="claude" \
	ALEXANDRIA_NEXT_WORKSPACE="$TMP_ROOT/data/workspaces/alexandria-wedo" \
	/bin/bash "$TMP_ROOT/host/bin/freeq-raven-railway"

assert_contains "--yes" "$TMP_ROOT/install.args"
assert_contains "--acp-provider" "$TMP_ROOT/install.args"
assert_contains "claude" "$TMP_ROOT/install.args"
assert_not_contains "--init" "$TMP_ROOT/install.args"
assert_contains "https://getalexandria.ai/install.sh" "$TMP_ROOT/curl.args"
assert_not_contains "install-next.sh" "$TMP_ROOT/curl.args"
assert_contains "init all --workspace .alexandria-next/railway-workspace --acp-provider claude" "$TMP_ROOT/ax.calls"
assert_contains "internal host freeq-raven heartbeat --connection host:freeq-raven:alexandria-wedo --follow --poll-interval-ms 1000" "$TMP_ROOT/ax.calls"
assert_contains '"workspace":".alexandria-next/railway-workspace"' "$TMP_ROOT/data/projects/alexandria-wedo/.alexandria-next/alexandria-config.json"

workspace_link="$TMP_ROOT/data/projects/alexandria-wedo/.alexandria-next/railway-workspace"
if [ ! -L "$workspace_link" ]; then
	echo "Expected $workspace_link to be a symlink" >&2
	exit 1
fi

if [ "$(readlink "$workspace_link")" != "$TMP_ROOT/data/workspaces/alexandria-wedo" ]; then
	echo "Unexpected workspace symlink target: $(readlink "$workspace_link")" >&2
	exit 1
fi
