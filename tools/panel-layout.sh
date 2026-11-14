#!/usr/bin/env bash
# Slot-based reader/writer for COSMIC panel layout files (plugins_center &
# plugins_wings) in a CosmicPanel.Panel/v1 preset directory.
#
# Keeps dev.asw.CosmicExtAppletClockCenter in plugins_center and updates only
# the requested slots so install-clock-center.sh, install-clipboard.sh, and
# install-panel.sh never clobber each other's layout changes.
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
DEFAULT_PRESET="$CONFIG_HOME/cosmic-presets/macos-glass/com.system76.CosmicPanel.Panel/v1"
PRESET="$DEFAULT_PRESET"
PRESET_OVERRIDDEN=0
STYLE_DIR="$CONFIG_HOME/cosmic/dev.asw.MacPanelStyle/v1"
LIVE_PANEL_DIR="$CONFIG_HOME/cosmic/com.system76.CosmicPanel.Panel/v1"
MODE=""
GEOM_MODE=""
SIZE_PX=""
FONT_SIZE=""
SETS=()

usage() {
  cat <<'EOF'
Usage: tools/panel-layout.sh [--preset <dir>] [--set <slot>=<value>]...
                             [--toolbar | --legacy-wings | --reset]
                             [--geometry | --legacy-geometry]
                             [--size <px>] [--font-size <px>]

Slots for --set:
  clock=<id>           Center slot (plugins_center)
  clipboard=<id|none>  Clipboard slot in right wing
  lord-menu=<id|none>  System menu slot in left wing
  app-menu=<id|none>   App menu slot in left wing
  spotlight=<id|none>  Spotlight slot in right wing
  control-center=<id|none>  Control Center slot in right wing
  notifications=<id|yes|no|none>  Stock notifications slot in right wing

Customization:
  --size <px>          Set top panel height (20..=64, default 32) and scale font/pills
  --font-size <px>     Override panel applet text size (e.g. 14.5, 16)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --preset)
      PRESET="${2:?--preset requires a directory}"
      PRESET_OVERRIDDEN=1
      shift 2
      ;;
    --style-dir)
      STYLE_DIR="${2:?--style-dir requires a directory}"
      shift 2
      ;;
    --set)
      SETS+=("${2:?--set requires slot=value}")
      shift 2
      ;;
    --toolbar)
      MODE="toolbar"
      shift
      ;;
    --legacy-wings)
      MODE="legacy"
      shift
      ;;
    --reset)
      MODE="reset"
      shift
      ;;
    --geometry)
      GEOM_MODE="toolbar"
      shift
      ;;
    --legacy-geometry)
      GEOM_MODE="legacy"
      shift
      ;;
    --size)
      SIZE_PX="${2:?--size requires a pixel integer}"
      shift 2
      ;;
    --font-size)
      FONT_SIZE="${2:?--font-size requires a number}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "panel-layout.sh: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

[ -d "$PRESET" ] || { echo "panel-layout.sh: preset directory not found: $PRESET" >&2; exit 1; }

CENTER_ITEMS=()
LEFT_ITEMS=()
RIGHT_ITEMS=()

parse_plugins_center() {
  local file="$PRESET/plugins_center"
  CENTER_ITEMS=()
  [ -f "$file" ] || return 0
  while IFS= read -r id; do
    [ -n "$id" ] && CENTER_ITEMS+=("$id")
  done < <(grep -oE '"[^"]+"' "$file" | tr -d '"')
}

parse_plugins_wings() {
  local file="$PRESET/plugins_wings"
  LEFT_ITEMS=()
  RIGHT_ITEMS=()
  [ -f "$file" ] || return 0

  # Split on `], [` (or `],[`) between the left and right tuple lists.
  local section="left"
  local line id
  while IFS= read -r line; do
    if [[ "$line" == *"],"*"["* ]] || [[ "$line" == *"], ["* ]]; then
      while IFS= read -r id; do
        [ -n "$id" ] && LEFT_ITEMS+=("$id")
      done < <(printf '%s\n' "${line%%]*}" | grep -oE '"[^"]+"' | tr -d '"' || true)
      section="right"
      while IFS= read -r id; do
        [ -n "$id" ] && RIGHT_ITEMS+=("$id")
      done < <(printf '%s\n' "${line#*]}" | grep -oE '"[^"]+"' | tr -d '"' || true)
      continue
    fi
    while IFS= read -r id; do
      [ -n "$id" ] || continue
      if [ "$section" = "left" ]; then
        LEFT_ITEMS+=("$id")
      else
        RIGHT_ITEMS+=("$id")
      fi
    done < <(printf '%s\n' "$line" | grep -oE '"[^"]+"' | tr -d '"' || true)
    if [[ "$line" == *"],"* ]] && [ "$section" = "left" ]; then
      section="right"
    fi
  done < "$file"
}

write_plugins_center() {
  local file="$PRESET/plugins_center"
  if [ ${#CENTER_ITEMS[@]} -eq 0 ]; then
    printf 'None' > "$file"
    return
  fi
  {
    printf 'Some([\n'
    for item in "${CENTER_ITEMS[@]}"; do
      printf '    "%s",\n' "$item"
    done
    printf '])'
  } > "$file"
}

write_plugins_wings() {
  local file="$PRESET/plugins_wings"
  {
    printf 'Some(([\n'
    for item in "${LEFT_ITEMS[@]}"; do
      printf '    "%s",\n' "$item"
    done
    printf '], [\n'
    for item in "${RIGHT_ITEMS[@]}"; do
      printf '    "%s",\n' "$item"
    done
    printf ']))'
  } > "$file"
}

array_contains() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    [ "$item" = "$needle" ] && return 0
  done
  return 1
}

replace_or_remove_in() {
  # $1 = array name (LEFT_ITEMS or RIGHT_ITEMS), $2 = new value (or "none"), $@ = candidate old ids
  local arr_name="$1"
  local new_val="$2"
  shift 2
  local candidates=("$@")
  local -n arr_ref="$arr_name"
  local out=()
  local replaced=0
  local item
  for item in "${arr_ref[@]}"; do
    if array_contains "$item" "${candidates[@]}"; then
      if [ "$new_val" != "none" ] && [ "$new_val" != "no" ] && [ "$replaced" = 0 ]; then
        out+=("$new_val")
        replaced=1
      fi
    else
      out+=("$item")
    fi
  done
  arr_ref=("${out[@]}")
  return $((replaced == 0 ? 1 : 0))
}

insert_after_in() {
  # $1 = array name, $2 = anchor id, $3 = new value
  local arr_name="$1"
  local anchor="$2"
  local new_val="$3"
  local -n arr_ref="$arr_name"
  local out=()
  local inserted=0
  local item
  for item in "${arr_ref[@]}"; do
    out+=("$item")
    if [ "$item" = "$anchor" ] && [ "$inserted" = 0 ]; then
      out+=("$new_val")
      inserted=1
    fi
  done
  if [ "$inserted" = 0 ]; then
    out+=("$new_val")
  fi
  arr_ref=("${out[@]}")
}

insert_before_in() {
  # $1 = array name, $2 = anchor id, $3 = new value
  local arr_name="$1"
  local anchor="$2"
  local new_val="$3"
  local -n arr_ref="$arr_name"
  local out=()
  local inserted=0
  local item
  for item in "${arr_ref[@]}"; do
    if [ "$item" = "$anchor" ] && [ "$inserted" = 0 ]; then
      out+=("$new_val")
      inserted=1
    fi
    out+=("$item")
  done
  if [ "$inserted" = 0 ]; then
    out+=("$new_val")
  fi
  arr_ref=("${out[@]}")
}

detect_clipboard_slot() {
  if array_contains "dev.asw.CosmicExtAppletClipboard" "${RIGHT_ITEMS[@]}"; then
    echo "dev.asw.CosmicExtAppletClipboard"
  elif array_contains "io.github.k33wee.clippy-land" "${RIGHT_ITEMS[@]}"; then
    echo "io.github.k33wee.clippy-land"
  else
    echo "dev.asw.CosmicExtAppletClipboard"
  fi
}

parse_plugins_center
parse_plugins_wings

case "$MODE" in
  reset)
    CENTER_ITEMS=("dev.asw.CosmicExtAppletClockCenter")
    LEFT_ITEMS=(
      "dev.asw.CosmicExtAppletLordMenu"
      "dev.asw.CosmicExtAppletAppMenu"
    )
    RIGHT_ITEMS=(
      "com.system76.CosmicAppletStatusArea"
      "dev.asw.CosmicExtAppletClipboard"
      "com.system76.CosmicAppletAudio"
      "com.system76.CosmicAppletBattery"
      "com.system76.CosmicAppletNetwork"
      "dev.asw.CosmicExtAppletSpotlight"
      "dev.asw.CosmicExtAppletControlCenter"
    )
    ;;
  toolbar)
    if [ ${#CENTER_ITEMS[@]} -eq 0 ]; then
      CENTER_ITEMS=("dev.asw.CosmicExtAppletClockCenter")
    fi
    clipboard_id="$(detect_clipboard_slot)"
    has_notifications=0
    if array_contains "com.system76.CosmicAppletNotifications" "${RIGHT_ITEMS[@]}" || \
       array_contains "com.system76.CosmicAppletTime" "${CENTER_ITEMS[@]}"; then
      has_notifications=1
    fi
    LEFT_ITEMS=(
      "dev.asw.CosmicExtAppletLordMenu"
      "dev.asw.CosmicExtAppletAppMenu"
    )
    RIGHT_ITEMS=(
      "com.system76.CosmicAppletStatusArea"
      "$clipboard_id"
      "com.system76.CosmicAppletAudio"
      "com.system76.CosmicAppletBattery"
      "com.system76.CosmicAppletNetwork"
      "dev.asw.CosmicExtAppletSpotlight"
      "dev.asw.CosmicExtAppletControlCenter"
    )
    if [ "$has_notifications" = 1 ] && ! array_contains "dev.asw.CosmicExtAppletClockCenter" "${CENTER_ITEMS[@]}"; then
      insert_before_in RIGHT_ITEMS "dev.asw.CosmicExtAppletSpotlight" "com.system76.CosmicAppletNotifications"
    fi
    ;;
  legacy)
    if [ ${#CENTER_ITEMS[@]} -eq 0 ]; then
      CENTER_ITEMS=("dev.asw.CosmicExtAppletClockCenter")
    fi
    clipboard_id="$(detect_clipboard_slot)"
    LEFT_ITEMS=(
      "dev.cappsy.CosmicExtAppletLogoMenu"
      "com.system76.CosmicPanelLauncherButton"
      "com.system76.CosmicPanelWorkspacesButton"
      "com.system76.CosmicPanelAppButton"
    )
    RIGHT_ITEMS=(
      "io.github.cosmic_utils.minimon-applet"
      "$clipboard_id"
      "com.system76.CosmicAppletTiling"
      "com.system76.CosmicAppletStatusArea"
      "com.system76.CosmicAppletAudio"
      "com.system76.CosmicAppletNetwork"
      "com.system76.CosmicAppletBluetooth"
      "com.system76.CosmicAppletBattery"
    )
    if array_contains "com.system76.CosmicAppletTime" "${CENTER_ITEMS[@]}"; then
      RIGHT_ITEMS+=("com.system76.CosmicAppletNotifications")
    fi
    RIGHT_ITEMS+=("com.system76.CosmicAppletPower")
    ;;
esac

for spec in "${SETS[@]}"; do
  slot="${spec%%=*}"
  val="${spec#*=}"
  [ "$slot" != "$spec" ] || { echo "panel-layout.sh: invalid --set '$spec' (expected slot=value)" >&2; exit 1; }
  case "$slot" in
    clock)
      if [ "$val" = "none" ] || [ "$val" = "no" ]; then
        CENTER_ITEMS=()
      else
        CENTER_ITEMS=("$val")
      fi
      ;;
    clipboard)
      if ! replace_or_remove_in RIGHT_ITEMS "$val" "dev.asw.CosmicExtAppletClipboard" "io.github.k33wee.clippy-land"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          insert_after_in RIGHT_ITEMS "com.system76.CosmicAppletStatusArea" "$val"
        fi
      fi
      ;;
    lord-menu|system-menu)
      if ! replace_or_remove_in LEFT_ITEMS "$val" "dev.asw.CosmicExtAppletLordMenu" "dev.cappsy.CosmicExtAppletLogoMenu"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          LEFT_ITEMS=("$val" "${LEFT_ITEMS[@]}")
        fi
      fi
      ;;
    app-menu)
      if ! replace_or_remove_in LEFT_ITEMS "$val" "dev.asw.CosmicExtAppletAppMenu"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          insert_after_in LEFT_ITEMS "dev.asw.CosmicExtAppletLordMenu" "$val"
        fi
      fi
      ;;
    spotlight)
      if ! replace_or_remove_in RIGHT_ITEMS "$val" "dev.asw.CosmicExtAppletSpotlight"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          insert_before_in RIGHT_ITEMS "dev.asw.CosmicExtAppletControlCenter" "$val"
        fi
      fi
      ;;
    control-center)
      if ! replace_or_remove_in RIGHT_ITEMS "$val" "dev.asw.CosmicExtAppletControlCenter"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          RIGHT_ITEMS+=("$val")
        fi
      fi
      ;;
    notifications)
      if [ "$val" = "yes" ]; then
        val="com.system76.CosmicAppletNotifications"
      fi
      if ! replace_or_remove_in RIGHT_ITEMS "$val" "com.system76.CosmicAppletNotifications"; then
        if [ "$val" != "none" ] && [ "$val" != "no" ]; then
          if array_contains "com.system76.CosmicAppletPower" "${RIGHT_ITEMS[@]}"; then
            insert_before_in RIGHT_ITEMS "com.system76.CosmicAppletPower" "$val"
          else
            insert_before_in RIGHT_ITEMS "dev.asw.CosmicExtAppletSpotlight" "$val"
          fi
        fi
      fi
      ;;
    *)
      echo "panel-layout.sh: unknown slot '$slot'" >&2
      exit 1
      ;;
  esac
done

if [ -n "$MODE" ] || [ ${#SETS[@]} -gt 0 ]; then
  write_plugins_center
  write_plugins_wings
fi

case "$GEOM_MODE" in
  toolbar)
    printf 'Custom(32)' > "$PRESET/size"
    printf 'ThemeDefault' > "$PRESET/background"
    printf '0.55' > "$PRESET/opacity"
    printf '2' > "$PRESET/spacing"
    printf '1.0' > "$PRESET/border_width"
    printf 'Some(150)' > "$PRESET/autohover_delay_ms"
    ;;
  legacy)
    printf 'XS' > "$PRESET/size"
    printf 'Dark' > "$PRESET/background"
    printf '0.65' > "$PRESET/opacity"
    printf '0' > "$PRESET/spacing"
    printf '0.0' > "$PRESET/border_width"
    printf 'Some(500)' > "$PRESET/autohover_delay_ms"
    ;;
esac

if [ -n "$SIZE_PX" ]; then
  if ! [[ "$SIZE_PX" =~ ^[0-9]+$ ]] || [ "$SIZE_PX" -lt 20 ] || [ "$SIZE_PX" -gt 64 ]; then
    echo "panel-layout.sh: --size must be an integer between 20 and 64 (got '$SIZE_PX')" >&2
    exit 1
  fi
  printf 'Custom(%s)' "$SIZE_PX" > "$PRESET/size"
  if [ "$PRESET_OVERRIDDEN" = 0 ] && [ -d "$LIVE_PANEL_DIR" ]; then
    printf 'Custom(%s)' "$SIZE_PX" > "$LIVE_PANEL_DIR/size"
  fi
  if [ -z "$FONT_SIZE" ]; then
    FONT_SIZE="$(awk -v s="$SIZE_PX" 'BEGIN { printf "%.1f", 14.5 * s / 32.0 }')"
  fi
  if [ "$PRESET_OVERRIDDEN" = 0 ] || [ "$STYLE_DIR" != "$CONFIG_HOME/cosmic/dev.asw.MacPanelStyle/v1" ]; then
    mkdir -p "$STYLE_DIR"
    printf '%s' "$((SIZE_PX - 6))" > "$STYLE_DIR/pill_height"
  fi
fi

if [ -n "$FONT_SIZE" ]; then
  if ! [[ "$FONT_SIZE" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    echo "panel-layout.sh: --font-size must be a positive number (got '$FONT_SIZE')" >&2
    exit 1
  fi
  if [ "$PRESET_OVERRIDDEN" = 0 ] || [ "$STYLE_DIR" != "$CONFIG_HOME/cosmic/dev.asw.MacPanelStyle/v1" ]; then
    mkdir -p "$STYLE_DIR"
    printf '%s' "$FONT_SIZE" > "$STYLE_DIR/font_size"
  fi
fi
