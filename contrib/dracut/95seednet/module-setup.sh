#!/bin/bash
# Brings up the network of the initrd from the NoCloud network-config, with systemd-networkd or
# NetworkManager, whichever the initrd has.
# dracut sets moddir, initdir, systemdsystemunitdir and SYSTEMCTL before it sources this.
# shellcheck disable=SC2154

check()
{
	require_binaries seednet udevadm ip || return 1
	# Only when asked for, as with add_dracutmodules+=" seednet ".
	return 255
}

depends()
{
	# The network module takes systemd-networkd or NetworkManager, the one that the initrd is given or
	# else the first that the system has.
	echo network
	# A host name in seednet.url or root=live:... needs a resolver, and only systemd-resolved
	# takes the name servers of systemd-networkd.
	if [[ -x $dracutsysrootdir$systemdutildir/systemd-resolved ]]; then
		echo systemd-resolved
	fi
}

installkernel()
{
	# Bonds, bridges and VLANs, and the CIDATA disk.
	hostonly='' instmods bonding bridge 8021q vfat iso9660 nls_cp437 nls_iso8859-1 nls_utf8
}

install()
{
	if ! dracut_module_included network-manager && ! dracut_module_included systemd-networkd; then
		dfatal "seednet: the initrd needs systemd-networkd or NetworkManager"
		return 1
	fi
	inst_multiple seednet udevadm ip
	# The bonding module makes a bond0 of its own in balance-rr, which a bond0 of the config cannot be
	# made over, but for the max_bonds=0 of systemd in /usr/lib/modprobe.d. The modprobe of busybox,
	# which dracut 112 of Debian and Ubuntu takes when it can, reads /etc/modprobe.d alone.
	inst_simple "$moddir/seednet-modprobe.conf" /etc/modprobe.d/seednet.conf
	# NetworkManager writes /etc/resolv.conf itself.
	if ! dracut_module_included network-manager \
		&& [[ ! -x $dracutsysrootdir$systemdutildir/systemd-resolved ]]; then
		dwarn "seednet: without systemd-resolved, the initrd cannot resolve host names"
	fi

	# An https seednet.url is verified against the CA bundle of the system only.
	local bundle
	for bundle in \
		/etc/ssl/certs/ca-certificates.crt \
		/etc/pki/tls/certs/ca-bundle.crt \
		/etc/ssl/ca-bundle.pem \
		/etc/ssl/cert.pem; do
		if [[ -e $bundle ]]; then
			inst_simple "$(readlink -f "$bundle")" "$bundle"
		fi
	done

	# After 99-networkd-config.sh and 99-nm-config.sh, which add rd.neednet for a network rootfs.
	inst_hook cmdline 99 "$moddir/zz-seednet-cmdline.sh"
	inst_script "$moddir/seednet-initrd.sh" /usr/libexec/seednet-initrd
	inst_simple "$moddir/seednet-initrd.service" "$systemdsystemunitdir/seednet-initrd.service"
	inst_simple "$moddir/seednet-initrd-url.service" \
		"$systemdsystemunitdir/seednet-initrd-url.service"
	$SYSTEMCTL -q --root "$initdir" enable seednet-initrd.service seednet-initrd-url.service
}
