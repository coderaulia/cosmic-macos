#!/usr/bin/env bash
# Shell test verifying tools/panel-layout.sh keeps dev.asw.CosmicExtAppletClockCenter
# in plugins_center and performs slot-based updates without clobbering sibling slots.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cp -a "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1" "$TMP/v1"

assert_contains() {
  local file="$1" needle="$2"
  if ! grep -Fq "$needle" "$file"; then
    echo "FAIL: expected '$needle' in $file" >&2
    cat "$file" >&2
    exit 1
  fi
}

assert_not_contains() {
  local file="$1" needle="$2"
  if grep -Fq "$needle" "$file"; then
    echo "FAIL: did not expect '$needle' in $file" >&2
    cat "$file" >&2
    exit 1
  fi
}

# 1. Preset starts with ClockCenter in plugins_center and toolbar wings
assert_contains "$TMP/v1/plugins_center" '"dev.asw.CosmicExtAppletClockCenter"'
assert_not_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletClockCenter"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletLordMenu"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletAppMenu"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletClipboard"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletSpotlight"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletControlCenter"'

# 2. Switching clipboard to clippy-land preserves all other slots and center clock
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --set clipboard=io.github.k33wee.clippy-land
assert_contains "$TMP/v1/plugins_center" '"dev.asw.CosmicExtAppletClockCenter"'
assert_contains "$TMP/v1/plugins_wings" '"io.github.k33wee.clippy-land"'
assert_not_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletClipboard"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletControlCenter"'

# 3. Uninstalling clock-center puts CosmicAppletTime in center and Notifications in right wing,
#    without overwriting the clipboard or toolbar slots
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --set clock=com.system76.CosmicAppletTime --set notifications=yes
assert_contains "$TMP/v1/plugins_center" '"com.system76.CosmicAppletTime"'
assert_contains "$TMP/v1/plugins_wings" '"com.system76.CosmicAppletNotifications"'
assert_contains "$TMP/v1/plugins_wings" '"io.github.k33wee.clippy-land"'
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletLordMenu"'

# 4. Reinstalling clock-center puts ClockCenter back in plugins_center and removes Notifications
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --set clock=dev.asw.CosmicExtAppletClockCenter --set notifications=none
assert_contains "$TMP/v1/plugins_center" '"dev.asw.CosmicExtAppletClockCenter"'
assert_not_contains "$TMP/v1/plugins_wings" '"com.system76.CosmicAppletNotifications"'
assert_contains "$TMP/v1/plugins_wings" '"io.github.k33wee.clippy-land"'

# 5. Reinstalling clipboard switches back to CosmicExtAppletClipboard
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --set clipboard=dev.asw.CosmicExtAppletClipboard
assert_contains "$TMP/v1/plugins_wings" '"dev.asw.CosmicExtAppletClipboard"'
assert_not_contains "$TMP/v1/plugins_wings" '"io.github.k33wee.clippy-land"'

# 6. --reset matches the checked-in preset files byte-for-byte
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --reset --geometry
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/plugins_center" "$TMP/v1/plugins_center"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/plugins_wings" "$TMP/v1/plugins_wings"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/size" "$TMP/v1/size"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/background" "$TMP/v1/background"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/opacity" "$TMP/v1/opacity"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/spacing" "$TMP/v1/spacing"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/border_width" "$TMP/v1/border_width"
diff -u "$ROOT/presets/macos-glass/com.system76.CosmicPanel.Panel/v1/autohover_delay_ms" "$TMP/v1/autohover_delay_ms"

# 7. --size customizes panel height and scales font/pill height in style dir
"$ROOT/tools/panel-layout.sh" --preset "$TMP/v1" --style-dir "$TMP/style" --size 36
assert_contains "$TMP/v1/size" 'Custom(36)'
assert_contains "$TMP/style/font_size" '16.3'
assert_contains "$TMP/style/pill_height" '30'

echo "test-panel-layout.sh: ALL PASSED"
