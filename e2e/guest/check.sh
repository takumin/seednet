#!/bin/sh
# Checks that the network is the way e2e/network-config says, that seednet brought it up from the
# expected source, and that nothing else configured it. Prints E2E-RESULT: PASS or FAIL, then
# powers off.

failures=0
fail()
{
	echo "E2E-FAIL: $*"
	failures=$((failures + 1))
}

param()
{
	tr ' ' '\n' < /proc/cmdline | sed -n "s/^$1=//p" | tail -n 1
}

# The report goes to the serial console at the end: the getty on it hangs up whatever opened it
# before, so it is opened only then.
exec > /run/e2e-report 2>&1

# Wait for the boot, cloud-init included.
systemctl is-system-running --wait > /dev/null 2>&1 || true
cloud-init status --wait > /dev/null 2>&1 || true

# The state of the network after the boot, which is checked again until it passes, for a while,
# since the network may still be settling. Only the last check is reported.
check_state()
{
	echo '=== E2E-REPORT'
	ip -c=never -br link
	ip -c=never -br addr
	ip -c=never route
	cat /run/seednet/source /run/seednet/backend 2> /dev/null
	journalctl -b -o cat --no-pager | grep '^seednet:'
	systemctl --failed --no-legend --no-pager

	# The interfaces, renamed.
	for dev in nic0 nic1 nic2 bond0 br0 vlan10; do
		[ -e "/sys/class/net/$dev" ] || fail "no $dev"
	done

	# bond0 with its ports and primary, br0 with vlan10, and vlan10 on nic2.
	[ "$(cat /sys/class/net/bond0/bonding/slaves 2> /dev/null | tr ' ' '\n' | sort | xargs)" = "nic0 nic1" ] \
		|| fail "the ports of bond0 are not nic0 and nic1"
	[ "$(cat /sys/class/net/bond0/bonding/primary 2> /dev/null)" = nic0 ] || fail "the primary of bond0 is not nic0"
	[ "$(cat /sys/class/net/bond0/bonding/mode 2> /dev/null)" = "active-backup 1" ] || fail "bond0 is not active-backup"
	[ -e /sys/class/net/br0/brif/vlan10 ] || fail "vlan10 is not a port of br0"
	if ! grep -q 'VID: 10' /proc/net/vlan/vlan10 2> /dev/null \
		|| ! grep -q 'Device: nic2' /proc/net/vlan/vlan10; then
		fail "vlan10 is not VLAN 10 on nic2"
	fi

	# The addresses and routes.
	ip -4 -o addr show dev bond0 | grep -q ' 10.0.2.15/24 ' || fail "bond0 has no 10.0.2.15/24"
	ip -4 -o addr show dev br0 | grep -q ' 192.168.10.1/24 ' || fail "br0 has no 192.168.10.1/24"
	ip route show default | grep -q 'via 10.0.2.2 dev bond0' || fail "no default route via bond0"
	ip route show 198.51.100.0/24 | grep -q 'via 192.168.10.254 dev br0' || fail "no route to 198.51.100.0/24"
	# Nothing of the network of the initrd stays, such as the IPv6 link-local address of a MAC address
	# that another backend gave the bridge.
	[ "$(ip -6 -o addr show dev br0 scope link | wc -l)" -le 1 ] || fail "br0 has more than one IPv6 link-local address"

	# seednet, from the expected source.
	systemctl -q is-active seednet.service || fail "seednet.service is not active"
	case $(param e2e.source) in
		url) expected=$(param seednet.url) ;;
		cmdline) expected="the kernel command line" ;;
		*) expected="the CIDATA disk" ;;
	esac
	[ "$(cat /run/seednet/source 2> /dev/null)" = "$expected" ] || fail "the source is not $expected"

	# The network of the initrd, where it brought it up, by the backend that it was to have. The journal
	# keeps what the initrd logged.
	case $(param e2e.initrd) in
		nm)
			journalctl -q -b -o cat --no-pager -u nm-initrd.service -u NetworkManager-initrd.service | grep -q . \
				|| fail "NetworkManager did not run in the initrd"
			;;
		networkd)
			journalctl -q -b -o cat --no-pager -u nm-initrd.service -u NetworkManager-initrd.service | grep -q . \
				&& fail "NetworkManager ran in the initrd"
			;;
	esac

	# Nothing else rendered the network.
	# But for the profiles that the NetworkManager of Ubuntu keeps there.
	for file in /etc/netplan/*; do
		case $file in /etc/netplan/90-NM-*.yaml | '/etc/netplan/*') ;; *) fail "cloud-init wrote $file" ;; esac
	done
	ls /etc/systemd/network/*cloud-init* /etc/NetworkManager/system-connections/*cloud-init* > /dev/null 2>&1 \
		&& fail "cloud-init wrote its own network files"
	[ -e /run/systemd/network/zzzz-dracut-default.network ] && fail "the catch-all DHCP of dracut is left"
	ls /run/systemd/network/*netplan* /run/NetworkManager/system-connections/*netplan* > /dev/null 2>&1 \
		&& fail "netplan wrote network files"
	for file in /etc/sysconfig/network-scripts/ifcfg-*; do
		[ -e "$file" ] && [ "$file" != /etc/sysconfig/network-scripts/ifcfg-lo ] \
			&& fail "cloud-init wrote $file"
	done

	# SELinux, where the guest has it: enforcing, and nothing denied, such as NetworkManager reading the
	# files of seednet.
	if [ "$(param e2e.selinux)" = 1 ]; then
		[ "$(cat /sys/fs/selinux/enforce 2> /dev/null)" = 1 ] || fail "SELinux is not enforcing"
		# But for what systemd sometimes does with the uevent socket of the initrd as it switches to the
		# rootfs, which has nothing to do with the network.
		journalctl -b -o cat --no-pager | grep 'avc:  denied' \
			| grep -v 'comm="systemd" .* scontext=system_u:system_r:init_t:s0 tcontext=system_u:system_r:kernel_t:s0 tclass=netlink_kobject_uevent_socket ' \
			&& fail "SELinux denied something"
	fi

	case $(param e2e.backend) in
		networkd) [ "$(cat /run/seednet/backend 2> /dev/null)" = networkd ] || fail "the backend is not networkd" ;;
		nm) [ "$(cat /run/seednet/backend 2> /dev/null)" = network-manager ] || fail "the backend is not network-manager" ;;
	esac
	case $(cat /run/seednet/backend 2> /dev/null) in
		networkd)
			systemctl -q is-failed systemd-networkd-wait-online.service && fail "systemd-networkd-wait-online failed"
			;;
		network-manager)
			systemctl -q is-failed NetworkManager-wait-online.service && fail "NetworkManager-wait-online failed"
			nmcli -t -f NAME,STATE connection show > /tmp/connections
			nmcli -f NAME,TYPE,DEVICE,STATE,FILENAME connection show
			for id in nic0 nic1 nic2 bond0 br0 vlan10; do
				grep -qx "seednet-$id:activated" /tmp/connections || fail "seednet-$id is not activated"
			done
			grep -vE '^(seednet-[a-z0-9]+|lo):' /tmp/connections && fail "NetworkManager has connections of its own"
			;;
		*)
			fail "no backend"
			;;
	esac
}

deadline=$(($(date +%s) + 30))
while :; do
	failures=0
	check_state > /run/e2e-state 2>&1
	[ "$failures" -eq 0 ] || [ "$(date +%s)" -ge "$deadline" ] && break
	sleep 1
done
cat /run/e2e-state

# Whether $1 gets the address $2, which a reload of the network may take a moment for.
has_address()
{
	for _ in 1 2 3 4 5 6 7 8 9 10; do
		ip -4 -o addr show dev "$1" | grep -q " $2 " && return 0
		sleep 1
	done
	return 1
}

# networkctl edit with the editor that adds the address $1, in a terminal that script gives it,
# since it edits nothing without one.
networkctl_edit()
{
	address=$1
	shift
	SYSTEMD_EDITOR=/usr/local/bin/e2e-editor E2E_ADDRESS=$address \
		script -qec "networkctl edit $*" /dev/null
}

# The edit scenario: an admin changes the network with the usual tool, and a second boot checks
# that the change stayed.
#
# nmcli saves a change over D-Bus, as the GUI does, and NetworkManager then keeps the connection in
# /etc with the UUID that seednet gave it: in /etc/NetworkManager, or on Ubuntu in /etc/netplan,
# which netplan renders to /run. After the reboot, seednet leaves that connection to
# NetworkManager.
#
# networkctl edit writes a drop-in, or a copy of the whole file, in /etc, by the name of the file of
# seednet. After the reboot, the drop-in still applies to the file that seednet writes again, and
# the copy hides it.
case $(cat /run/seednet/backend 2> /dev/null),$(param e2e.phase) in
	network-manager,edit)
		nmcli connection modify seednet-br0 +ipv4.addresses 192.168.20.1/24 \
			|| fail "nmcli could not change seednet-br0"
		nmcli -f NAME,UUID,FILENAME connection show
		ls -l /etc/NetworkManager/system-connections /run/NetworkManager/system-connections
		sync
		;;
	network-manager,edited)
		has_address br0 192.168.20.1/24 || fail "br0 has no 192.168.20.1/24, the address added with nmcli"
		# FILENAME is a column of the list only, not a field of a connection.
		nmcli -t -f NAME,FILENAME connection show \
			| grep -qxE 'seednet-br0:(/etc/NetworkManager/system-connections/|/run/NetworkManager/system-connections/netplan-).*' \
			|| fail "seednet-br0 is not the connection in /etc"
		journalctl -b -o cat --no-pager \
			| grep -q '^seednet: warning: .* has the UUID of 10-seednet-br0\.nmconnection' \
			|| fail "seednet did not leave seednet-br0 to NetworkManager"
		;;
	networkd,edit)
		networkctl_edit 192.168.20.1/24 --drop-in=50-e2e @br0 \
			|| fail "networkctl could not add a drop-in to br0"
		networkctl_edit 192.168.30.1/24 @bond0 || fail "networkctl could not change the file of bond0"
		networkctl cat @br0 @bond0
		ls -lR /etc/systemd/network /run/systemd/network
		has_address br0 192.168.20.1/24 || fail "the drop-in did not give br0 192.168.20.1/24"
		has_address bond0 192.168.30.1/24 || fail "the copy did not give bond0 192.168.30.1/24"
		sync
		;;
	networkd,edited)
		networkctl status br0 bond0 --no-pager
		etc=/etc/systemd/network
		has_address br0 192.168.20.1/24 || fail "br0 has no 192.168.20.1/24, the address of the drop-in"
		has_address br0 192.168.10.1/24 || fail "br0 has no 192.168.10.1/24, the address of seednet"
		has_address bond0 192.168.30.1/24 || fail "bond0 has no 192.168.30.1/24, the address of the copy"
		[ -f $etc/10-seednet-br0.network.d/50-e2e.conf ] || fail "no drop-in of br0 in /etc"
		[ -e $etc/10-seednet-br0.network ] && fail "networkctl edit --drop-in copied the file of br0"
		networkctl status br0 --no-pager | grep -q 'Network File: /run/systemd/network/10-seednet-br0\.network' \
			|| fail "the file of br0 is not the one of seednet"
		networkctl status bond0 --no-pager | grep -q "Network File: $etc/10-seednet-bond0\\.network" \
			|| fail "the file of bond0 is not the copy in /etc"
		journalctl -b -o cat --no-pager \
			| grep -q "^seednet: warning: $etc/10-seednet-bond0\\.network overrides the file of seednet" \
			|| fail "seednet did not tell of the copy of the file of bond0"
		;;
esac

if [ "$failures" -eq 0 ]; then
	echo 'E2E-RESULT: PASS'
else
	echo "E2E-RESULT: FAIL ($failures)"
fi
# The getty on the console hangs it up as it starts, which may be now, so it goes first. Then all of
# the report, before the power goes off: the shutdown drops what the console has not sent yet.
tty=$(param console)
systemctl stop "serial-getty@$tty.service" > /dev/null 2>&1 || true
python3 -I -c '
import os, sys, termios
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_NOCTTY)
data = open(sys.argv[1], "rb").read()
while data:
	data = data[os.write(fd, data):]
termios.tcdrain(fd)
' /run/e2e-report "/dev/$tty"
systemctl poweroff
