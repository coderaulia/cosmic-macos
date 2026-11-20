#!/usr/bin/env bash
# Add or remove one entry in COSMIC's custom keyboard shortcuts, keeping a
# backup of whatever the key did before so removal puts it back.
#
#   tools/shortcut.sh set   "Super,Shift" s 'Spawn("mac-screenshot")'
#   tools/shortcut.sh unset "Super,Shift" s
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
FILE="$CONFIG_HOME/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"
BACKUP_DIR="$CONFIG_HOME/cosmic-presets/.shortcut-backup"

mode="${1:?set|unset}" mods="${2:?modifiers}" key="${3:?key}" action="${4:-}"
[ "$mode" = set ] && [ -z "$action" ] && { echo "shortcut.sh: set needs an action" >&2; exit 1; }
mkdir -p "$(dirname "$FILE")" "$BACKUP_DIR"
[ -f "$FILE" ] || printf '{\n}' >"$FILE"

python3 - "$FILE" "$BACKUP_DIR" "$mode" "$mods" "$key" "$action" <<'EOF'
import os, re, sys
path, backup_dir, mode, mods, key, action = sys.argv[1:]
mods = [m.strip() for m in mods.split(",") if m.strip()]
text = open(path).read()
entry_re = re.compile(r'\(\s*modifiers:\s*\[([^\]]*)\],\s*key:\s*"([^"]*)",\s*\):\s*([^\n]*?),\n', re.S)
backup = os.path.join(backup_dir, "-".join(mods + [key]))

def same(m):
    have = [x.strip() for x in m.group(1).split(",") if x.strip()]
    return sorted(have) == sorted(mods) and m.group(2) == key

old = None
def drop(m):
    global old
    if same(m):
        old = m.group(3)
        return ""
    return m.group(0)
text = entry_re.sub(drop, text)

# Our own bindings, by bare name or full path (~/.local/bin/mac-...), are never
# saved as "what the key did before".
ours = re.compile(r'Spawn\("(?:[^"]*/)?mac-')
if os.path.exists(backup) and ours.match(open(backup).read()):
    os.remove(backup)

if mode == "set":
    if old is not None and not ours.match(old) and not os.path.exists(backup):
        open(backup, "w").write(old)
    mod_lines = "".join(f"            {m},\n" for m in mods)
    entry = f'    (\n        modifiers: [\n{mod_lines}        ],\n        key: "{key}",\n    ): {action},\n'
    text = text.rstrip()
    assert text.endswith("}")
    text = text[:-1] + entry + "}"
else:
    if os.path.exists(backup):
        prev = open(backup).read()
        mod_lines = "".join(f"            {m},\n" for m in mods)
        text = text.rstrip()[:-1] + f'    (\n        modifiers: [\n{mod_lines}        ],\n        key: "{key}",\n    ): {prev},\n' + "}"
        os.remove(backup)

tmp = path + ".tmp"
open(tmp, "w").write(text)
os.replace(tmp, path)
EOF
