#!/bin/sh
# Boots the guests of the end-to-end tests: the guests at once, and the scenarios of each in turn,
# since they share the disks of their guest. Prints a line for each scenario as it ends, and fails
# if any did.
#
#   e2e/all.sh [<distro>-<initrd>-<rootfs> ...]
#
# Without guests, it boots all of them. E2E_JOBS guests run at once: by default, as many as the
# host has two CPUs and 4 GiB of available memory for, which is what a guest with a live rootfs
# takes. The output of each scenario goes to target/e2e/<guest>/<scenario>.log.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
scenarios="disk neednet cmdline url live https edit noseed"

# One guest, as xargs runs it.
if [ "${1:-}" = --guest ]; then
	guest=$2
	# shellcheck disable=SC2046
	set -- $(echo "$guest" | tr - ' ')
	mkdir -p "$repo/target/e2e/$guest"
	failed=0
	for scenario in $scenarios; do
		log=$repo/target/e2e/$guest/$scenario.log
		if "$repo/e2e/run.sh" "$1" "$2" "$3" "$scenario" > "$log" 2>&1 < /dev/null; then
			tail -n 1 "$log"
		else
			failed=1
			# A run that ends before its check, as when the image does not build, says so last.
			last=$(tail -n 1 "$log")
			case $last in
				FAIL:*) echo "$last" ;;
				*) echo "FAIL: $1 $2 $3 $scenario (see $log)" ;;
			esac
		fi
	done
	exit "$failed"
fi

guests=${*:-debian-networkd-networkd debian-networkd-nm debian-nm-networkd debian-nm-nm
ubuntu-networkd-networkd ubuntu-nm-nm fedora-networkd-networkd fedora-nm-nm el9-nm-nm el10-nm-nm}

if [ -z "${E2E_JOBS:-}" ]; then
	cpus=$(($(nproc) / 2))
	memory=$(($(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo) / 4194304))
	E2E_JOBS=$((cpus < memory ? cpus : memory))
	[ "$E2E_JOBS" -ge 1 ] || E2E_JOBS=1
fi

mkdir -p "$repo/target/e2e"
results=$repo/target/e2e/results
echo "$guests" | tr -s ' \n' '\n' | grep . \
	| xargs -P "$E2E_JOBS" -I{} "$0" --guest {} | tee "$results" || true

passed=$(grep -c '^PASS:' "$results" || true)
failed=$(grep -c '^FAIL:' "$results" || true)
echo "=== $passed passed, $failed failed"
[ "$failed" -eq 0 ]
