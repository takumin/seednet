#!/bin/sh
# The editor of networkctl edit in the edit scenario, as SYSTEMD_EDITOR: adds the address in
# E2E_ADDRESS to each file it opens. In a drop-in, only what follows the first marker comment is
# kept, so the address goes right after it.
set -eu

: "${E2E_ADDRESS:?the address to add}"

for file; do
	if grep -q '^### Anything between here' "$file"; then
		sed -i "/^### Anything between here/a [Network]\nAddress=$E2E_ADDRESS" "$file"
	else
		printf '\n[Network]\nAddress=%s\n' "$E2E_ADDRESS" >> "$file"
	fi
done
