#!/usr/bin/env bash
# Repository hygiene and integration verification for cosmic-macos.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PASS=0
FAIL=0
SKIP=0

pass() {
  printf '  PASS  %s\n' "$1"
  PASS=$((PASS + 1))
}

fail() {
  printf '  FAIL  %s\n' "$1" >&2
  FAIL=$((FAIL + 1))
}

skip() {
  printf '  SKIP  %s\n' "$1"
  SKIP=$((SKIP + 1))
}

echo "==> 1. Shell syntax checks (bash -n)"
shopt -s nullglob
syntax_targets=(
  "$ROOT"/install*.sh
  "$ROOT/setup.sh"
  "$ROOT"/tools/*.sh
  "$ROOT/bin/cosmic-preset"
  "$ROOT/PKGBUILD"
)
for f in "${syntax_targets[@]}"; do
  [ -f "$f" ] || continue
  rel="${f#"$ROOT"/}"
  if bash -n "$f"; then
    pass "bash -n $rel"
  else
    fail "bash -n $rel"
  fi
done

echo
echo "==> 2. Panel layout tests"
if "$ROOT/tools/test-panel-layout.sh"; then
  pass "tools/test-panel-layout.sh"
else
  fail "tools/test-panel-layout.sh"
fi

echo
echo "==> 3. Overlay vs upstream-applets sync"
if [ ! -d "$ROOT/upstream-applets" ]; then
  skip "upstream-applets/ not set up (fresh checkout; run ./setup.sh applets to compare)"
else
  for overlay_dir in "$ROOT"/overlays/cosmic-applets/*/; do
    [ -d "$overlay_dir" ] || continue
    name="$(basename "$overlay_dir")"
    upstream_dir="$ROOT/upstream-applets/$name"
    if [ ! -d "$upstream_dir" ]; then
      fail "missing upstream-applets/$name"
      continue
    fi
    if diff_out="$(diff -ru -x target "$overlay_dir" "$upstream_dir" 2>&1)"; then
      pass "sync overlays/cosmic-applets/$name == upstream-applets/$name"
    else
      fail "differs: overlays/cosmic-applets/$name vs upstream-applets/$name"
      printf '%s\n' "$diff_out" | head -n 40 >&2
    fi
  done
fi

echo
echo "==> 4. Presets & cosmic-preset"
if [ -d "$ROOT/presets/macos-glass" ]; then
  pass "presets/macos-glass exists"
else
  fail "presets/macos-glass missing"
fi

if [ -d "$ROOT/presets/macos-light" ]; then
  if [ -f "$ROOT/presets/macos-light/com.system76.CosmicTheme.Mode/v1/is_dark" ]; then
    pass "presets/macos-light exists (with CosmicTheme.Mode/v1/is_dark)"
  else
    fail "presets/macos-light missing com.system76.CosmicTheme.Mode/v1/is_dark"
  fi
else
  skip "presets/macos-light not present yet"
fi

PRESET_BIN=""
if command -v cosmic-preset >/dev/null 2>&1; then
  PRESET_BIN="$(command -v cosmic-preset)"
elif [ -x "$HOME/.local/bin/cosmic-preset" ]; then
  PRESET_BIN="$HOME/.local/bin/cosmic-preset"
fi

if [ -n "$PRESET_BIN" ]; then
  if "$PRESET_BIN" list >/dev/null; then
    pass "$PRESET_BIN list"
  else
    fail "$PRESET_BIN list"
  fi
else
  skip "cosmic-preset not installed"
fi

echo
echo "Summary: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
