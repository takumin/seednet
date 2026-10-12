#!/bin/sh
# Decides whether seednet brings up the network of the initrd: when the network is needed, as
# rd.neednet says or a network rootfs such as root=live:https://... implies, or for seednet.url.

# shellcheck source=/dev/null
command -v getarg > /dev/null || . /lib/dracut-lib.sh

# getarg reads /etc/cmdline.d too, as `dracut --kernel-cmdline` fills it, which the conditions of
# the units do not, so they go by what this leaves in /run/seednet.
mkdir -p /run/seednet
# An empty seednet.url= undoes one before it, as seednet takes it.
if [ -n "$(getarg seednet.url)" ]; then
	: > /run/seednet/url
	echo rd.neednet=1 > /etc/cmdline.d/90-seednet.conf
	# As 99-networkd-config.sh and 99-nm-config.sh do for rd.neednet, which ran before this.
	if [ -e /lib/nm-lib.sh ]; then
		# shellcheck source=/dev/null
		command -v nm_generate_connections > /dev/null || . /lib/nm-lib.sh
		nm_generate_connections
	else
		mkdir -p /run/networkd/initrd
		: > /run/networkd/initrd/neednet
	fi
fi

if getargbool 0 rd.neednet; then
	: > /run/seednet/neednet
fi
