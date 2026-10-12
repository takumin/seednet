#!/bin/sh
# Brings up the network of the initrd from the NoCloud network-config: before systemd-networkd or
# NetworkManager starts, or with `url`, once the DHCP network of dracut is up to fetch the config
# over.
set -eu

seednet --backend auto apply --stage initrd --takeover

# Without a config, dracut brings up the network its own way.
[ -e /run/seednet/source ] || exit 0
backend=$(cat /run/seednet/backend)
# The unit of dracut, or the one of NetworkManager 1.54 and later.
nm_unit=nm-initrd.service
if [ -e /usr/lib/systemd/system/NetworkManager-initrd.service ]; then
	nm_unit=NetworkManager-initrd.service
fi

# The catch-all DHCP of dracut is for when nothing says how the network is.
rm -f /etc/systemd/network/zzzz-dracut-default.network

if [ "${1:-}" = url ]; then
	# NetworkManager would bring the interfaces up again as they go down, and take the networks of
	# seednet only as it starts.
	if [ "$backend" = network-manager ]; then
		systemctl stop "$nm_unit"
	fi
	# A running interface keeps its name, so take the physical ones down for the renames.
	for dev in /sys/class/net/*; do
		if [ -e "$dev/device" ]; then
			ip link set "${dev##*/}" down
		fi
	done
fi

# udevd reads new .link files only every few seconds, so tell it to, then rename.
udevadm control --reload
udevadm trigger --action=add --subsystem-match=net
udevadm settle

if [ "${1:-}" = url ]; then
	case $backend in
		networkd)
			# networkctl needs D-Bus, which the initrd lacks, and a restart reads the new files too.
			systemctl restart systemd-networkd.service
			;;
		network-manager)
			# The initqueue reaches for the rootfs once this is done, so wait for the network.
			systemctl start "$nm_unit"
			nm-online -s -q -t 60
			;;
	esac
fi
