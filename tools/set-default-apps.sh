#!/usr/bin/env bash
# Make an installed app the default for the file types it declares.
#   tools/set-default-apps.sh dev.asw.MacCalendar.desktop "text/calendar;application/ics;"
# Sets both the standard list (xdg-mime) and COSMIC's own list, which COSMIC
# Settings reads and which takes priority.
set -euo pipefail
desktop="$1"
mimes="$2"
[ -n "$mimes" ] || exit 0

for mime in ${mimes//;/ }; do
  xdg-mime default "$desktop" "$mime"
done

python3 -I - "$desktop" "$mimes" <<'PY'
import os, sys
desktop, mimes = sys.argv[1], [m for m in sys.argv[2].split(';') if m]
path = os.path.expanduser('~/.config/cosmic-mimeapps.list')
lines = open(path).read().split('\n') if os.path.exists(path) else ['[Default Applications]']
if '[Default Applications]' not in lines:
    lines = ['[Default Applications]'] + lines
out, section, seen = [], None, set()
for line in lines:
    if line.startswith('['):
        if section == '[Default Applications]':
            out.extend(f'{m}={desktop}' for m in mimes if m not in seen)
        section = line.strip()
    elif section == '[Default Applications]' and '=' in line:
        key = line.split('=', 1)[0]
        if key in mimes:
            line = f'{key}={desktop}'
            seen.add(key)
    out.append(line)
if section == '[Default Applications]':
    out.extend(f'{m}={desktop}' for m in mimes if m not in seen)
open(path, 'w').write('\n'.join(out).rstrip('\n') + '\n')
PY
