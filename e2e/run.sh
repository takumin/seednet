#!/bin/sh
# Boots a guest with seednet and checks its network, without root privileges.
#
#   e2e/run.sh <debian|ubuntu|fedora|el9|el10> <initrd> <rootfs> <scenario>
#
# <initrd> and <rootfs> are networkd or nm: the backend of the initrd and of the rootfs.
#
#   debian   Debian trixie, with either in the initrd whatever the rootfs has.
#   ubuntu   Ubuntu 26.04, with netplan, and the same in the initrd as in the rootfs.
#   fedora   Fedora 44, as ubuntu, without netplan, and with SELinux enforcing.
#   el9      Rocky Linux 9, as fedora, with NetworkManager only.
#   el10     AlmaLinux 10, as el9.
#
#   disk     The rootfs on a disk; seednet.service in the rootfs reads the CIDATA disk.
#   neednet  As disk, with rd.neednet=1: the initrd brings up the network, and the rootfs keeps it.
#            The config is of version 1, on a CIDATA disk of iso9660 labelled cidata, as
#            cloud-localds makes it.
#   cmdline  As neednet, without a CIDATA disk: the config is in seednet.network-config, gzipped and
#            in URL-safe base64.
#   url      No CIDATA disk: the initrd fetches the NoCloud seed of seednet.url over DHCP, by the
#            host name that DHCP gives, with seednet.checksum, and cloud-init takes the seed.
#   live     No rootfs disk: the initrd brings up the network from the CIDATA disk, without DHCP, to
#            fetch a live rootfs over HTTP, as root=live:http://... says.
#   edit     As disk, then the network is changed with the usual tool, and a second boot checks
#            that the change stayed: with nmcli, as the GUI changes it, or with networkctl edit, as a
#            drop-in and as a copy of a whole file.
#   https    As url and live at once, by a host name over HTTPS: the initrd fetches seednet.url
#            with the name server of DHCP, by the FQDN that DHCP gives, then the live rootfs with
#            the one of the config.
#   badsum   As url, with a seednet.checksum that the config does not have: the boot must stop in
#            the initrd, saying so.
#
# It runs on an x86_64 or aarch64 host with KVM, with guests of its architecture. It needs podman,
# passt with vhost-user, cloud-hypervisor, mkdosfs, mcopy, xorrisofs, mksquashfs 4.6 or later,
# mkfs.ext4 and debugfs of e2fsprogs 1.43 or later, ip, unshare, timeout, sha256sum, openssl and
# python3, and the release build of seednet for <arch>-unknown-linux-musl. The work goes in
# target/e2e.
#
# E2E_MIRROR_DEBIAN, E2E_MIRROR_UBUNTU, E2E_MIRROR_FEDORA, E2E_MIRROR_EL9 and E2E_MIRROR_EL10 name a
# mirror for the packages of the image of each distro, in place of the default one or the mirror
# list: the URL of the archive for Debian and Ubuntu, as http://ftp.jp.debian.org/debian, over http
# since their base images have no CA certificates, and of the directory of the releases for Fedora
# and Enterprise Linux, as https://ftp.riken.jp/Linux/fedora.
set -eu

distro=${1:?distro: debian, ubuntu, fedora, el9 or el10}
initrd=${2:?initrd: networkd or nm}
backend=${3:?rootfs: networkd or nm}
scenario=${4:?scenario: disk, neednet, cmdline, url, live, edit, https or badsum}
case $distro in debian | ubuntu | fedora | el9 | el10) ;; *) echo "unknown distro: $distro" >&2 && exit 2 ;; esac
arch=$(uname -m)
case $arch in
	x86_64) platform=linux/amd64 ;;
	aarch64) platform=linux/arm64 ;;
	*) echo "unsupported architecture: $arch" >&2 && exit 2 ;;
esac
for b in "$initrd" "$backend"; do
	case $b in networkd | nm) ;; *) echo "unknown backend: $b" >&2 && exit 2 ;; esac
done
case $scenario in
	disk | neednet | cmdline | url | live | edit | https | badsum) ;;
	*) echo "unknown scenario: $scenario" >&2 && exit 2 ;;
esac
case $distro,$initrd,$backend in
	el*,networkd,* | el*,*,networkd) echo "$distro has no systemd-networkd" >&2 && exit 2 ;;
	debian,*) ;;
	*,networkd,nm | *,nm,networkd) echo "$distro has the same in the initrd as in the rootfs" >&2 && exit 2 ;;
esac

repo=$(cd "$(dirname "$0")/.." && pwd)
guest=$distro-$initrd-$backend
work=$repo/target/e2e/$guest
tls=$repo/target/e2e/tls
mkdir -p "$work"
cd "$work"

# The host name of the servers, which only the name server of the tests knows.
name=seednet.e2e.test
# The FQDN that DHCP gives the guest, by which it fetches its seed.
guest_fqdn=seednet-e2e.e2e.test

pids=
boot_pids=
netns=
# Waits up to 30 seconds for a file to be there, as test says it, such as -s for one not empty.
wait_for()
{
	n=0
	while ! test "$1" "$2"; do
		n=$((n + 1))
		if [ "$n" -gt 300 ]; then
			echo "$2 is not there after 30 seconds" >&2
			return 1
		fi
		sleep 0.1
	done
}

cleanup()
{
	# The servers and passt in the network namespace stop themselves when netns.run goes.
	rm -f netns.run
	[ -z "$netns" ] || wait "$netns" || true
	# shellcheck disable=SC2086
	[ -z "$pids$boot_pids" ] || kill $pids $boot_pids 2> /dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# A CA that the image trusts, and the certificate of the HTTPS server, made once.
"$repo/e2e/tls.sh" "$tls" "$name"

# The image, and the disks made from it, which are made again only when the image changes.
# The initrd of Containerfile.rpm takes the backend of the rootfs.
case $distro in
	debian | ubuntu) build="-f $repo/e2e/Containerfile.deb --build-arg INITRD=$initrd" ;;
	*) build="-f $repo/e2e/Containerfile.rpm" ;;
esac
case $distro in
	debian) mirror=${E2E_MIRROR_DEBIAN:-} ;;
	ubuntu) mirror=${E2E_MIRROR_UBUNTU:-} ;;
	fedora) mirror=${E2E_MIRROR_FEDORA:-} ;;
	el9) mirror=${E2E_MIRROR_EL9:-} ;;
	el10) mirror=${E2E_MIRROR_EL10:-} ;;
esac
# The platform of the host, for a base image of another architecture that the storage of podman may
# have under the same name, which the build would otherwise take and run emulated.
# shellcheck disable=SC2086
podman build -q --platform "$platform" $build --build-arg "DISTRO=$distro" --build-arg "BACKEND=$backend" \
	--build-arg "MIRROR=$mirror" \
	--build-arg "SEEDNET=target/$arch-unknown-linux-musl/release/seednet" \
	--build-context "tls=$tls" -t "localhost/seednet-e2e-$guest" "$repo" > image.id.new
if ! cmp -s image.id.new image.id || [ ! -e rootfs.img ] || [ ! -e rootfs.squashfs ]; then
	cid=$(podman create "localhost/seednet-e2e-$guest")
	podman unshare "$repo/e2e/disks.sh" "$cid"
	python3 -I "$repo/e2e/kernel.py" vmlinuz
	podman rm -f "$cid" > /dev/null
	mv image.id.new image.id
fi

rm -rf cidata.img cidata
if [ "$scenario" = neednet ]; then
	# As cloud-localds makes it, with a config of version 1.
	mkdir cidata
	cp "$repo/e2e/network-config-v1" cidata/network-config
	cp "$repo/e2e/meta-data" "$repo/e2e/user-data" cidata/
	xorrisofs -quiet -o cidata.img -V cidata -J -r cidata
else
	mkdosfs -n CIDATA -C cidata.img 2048 > /dev/null
	mcopy -i cidata.img "$repo/e2e/network-config" "$repo/e2e/meta-data" "$repo/e2e/user-data" ::
fi

# A server for the config and the live rootfs, on the host loopback that passt maps the gateway to.
# With https, it is in a network namespace of its own with passt, where it can take the ports of
# HTTPS and DNS, and the name server of the tests is at 10.0.2.3, which the config names.
rm -rf www port
mkdir www
# The seed of the guest, by its host name and by its FQDN.
for seed in "www/seed/${guest_fqdn%%.*}" "www/seed/$guest_fqdn"; do
	mkdir -p "$seed"
	cp "$repo/e2e/network-config" "$repo/e2e/meta-data" "$repo/e2e/user-data" "$seed/"
done
ln rootfs.squashfs www/rootfs.squashfs 2> /dev/null || cp rootfs.squashfs www/
if [ "$scenario" = https ]; then
	server=https://$name
else
	python3 -I "$repo/e2e/server.py" www port > http.log 2>&1 &
	pids="$pids $!"
	wait_for -s port || {
		cat http.log >&2
		exit 1
	}
	server=http://10.0.2.2:$(cat port)
fi

# The serial console that --serial of cloud-hypervisor is: a 16550 on x86_64, a PL011 on aarch64.
case $arch in
	aarch64) tty=ttyAMA0 ;;
	*) tty=ttyS0 ;;
esac
cmdline="console=$tty panic=-1"
# The guests of Containerfile.rpm have SELinux, which must be enforcing and deny nothing.
case $distro in debian | ubuntu) ;; *) cmdline="$cmdline e2e.selinux=1" ;; esac
disks="path=cidata.img,image_type=raw,readonly=on"
dhcp=--no-dhcp
memory=1G
case $scenario in
	disk | edit)
		cmdline="$cmdline root=/dev/vda rw"
		;;
	neednet)
		cmdline="$cmdline root=/dev/vda rw rd.neednet=1"
		;;
	cmdline)
		config=$(gzip -9n < "$repo/e2e/network-config" | basenc --base64url -w 0 | tr -d =)
		cmdline="$cmdline root=/dev/vda rw rd.neednet=1 seednet.network-config=$config"
		disks=
		;;
	url | https | badsum)
		sum=$(sha256sum "$repo/e2e/network-config" | cut -d' ' -f1)
		# The checksum of something else.
		[ "$scenario" != badsum ] || sum=$(echo | sha256sum | cut -d' ' -f1)
		seed='{hostname}'
		[ "$scenario" != https ] || seed='{fqdn}'
		# ds-identify looks for the seed before seednet hands it to cloud-init.
		cmdline="$cmdline seednet.url=$server/seed/$seed/ seednet.checksum=sha256:$sum ds=nocloud"
		disks=
		dhcp=
		;;
esac
case $scenario in
	url | badsum) cmdline="$cmdline root=/dev/vda rw" ;;
	live | https)
		cmdline="$cmdline root=live:$server/rootfs.squashfs rd.live.overlay.overlayfs=1"
		memory=3G
		;;
esac
case $scenario in
	url | https) cmdline="$cmdline e2e.source=url" ;;
	cmdline) cmdline="$cmdline e2e.source=cmdline" ;;
	*) cmdline="$cmdline e2e.source=cidata" ;;
esac
# The backend of the rootfs, and the one that brings up the network of the initrd, where it does.
cmdline="$cmdline e2e.backend=$backend"
case $scenario in
	disk | edit) ;;
	*) cmdline="$cmdline e2e.initrd=$initrd" ;;
esac
if [ "$scenario" != live ] && [ "$scenario" != https ]; then
	cp --sparse=always rootfs.img run.img
	disks="path=run.img,image_type=raw $disks"
fi

# What seednet says of a config that does not have the checksum.
badsum="is not the expected one"

# Boots the guest once, with e2e.phase=$1 when it is not empty, and checks its report.
boot()
{
	phase=$1

	# Three NICs, each on its own passt: nic0 and nic1 for bond0, and nic2 for vlan10.
	passt="passt --vhost-user -f -q -a 10.0.2.15 -n 24 -g 10.0.2.2 $dhcp -H $guest_fqdn --no-ndp --no-dhcpv6 --no-ra"
	nets=
	for i in 0 1 2; do
		rm -f "p$i.sock"
		nets="$nets vhost_user=true,socket=p$i.sock,mac=52:54:00:00:00:0$((i + 1))"
	done
	if [ "$scenario" = https ]; then
		# The servers and passt in a network namespace, until netns.run goes: killing podman
		# unshare would leave them running.
		touch netns.run
		podman unshare unshare -n sh -euc '
			ip link set lo up
			python3 -I "$1" www port --port 443 --tls "$2/server.crt" "$2/server.key" \
				--dns "$3" 10.0.2.2 > http.log 2>&1 &
			pids=$!
			for i in 0 1 2; do
				$4 -s "p$i.sock" -D 10.0.2.3 --dns-forward 10.0.2.3 --dns-host 127.0.0.1 \
					> "passt$i.log" 2>&1 &
				pids="$pids $!"
			done
			while [ -e netns.run ]; do sleep 1; done
			kill $pids 2> /dev/null || true
			wait
		' sh "$repo/e2e/server.py" "$tls" "$name" "$passt" &
		netns=$!
	else
		for i in 0 1 2; do
			$passt -s "p$i.sock" > "passt$i${phase:+-$phase}.log" 2>&1 &
			boot_pids="$boot_pids $!"
		done
	fi
	for i in 0 1 2; do
		wait_for -S "p$i.sock"
	done

	console=console-$scenario${phase:+-$phase}.log
	# shellcheck disable=SC2086
	timeout 300 cloud-hypervisor \
		--kernel vmlinuz --initramfs initrd.img --cmdline "$cmdline${phase:+ e2e.phase=$phase}" \
		--cpus boot=2 --memory "size=$memory,shared=on" \
		${disks:+--disk $disks} --net $nets \
		--serial tty --console off > "$console" 2>&1 &
	vm=$!
	boot_pids="$boot_pids $vm"
	if [ "$scenario" = badsum ]; then
		# The guest stops in the emergency shell, which it does not leave by itself.
		n=0
		while kill -0 "$vm" 2> /dev/null && ! grep -q "$badsum" "$console" && [ "$n" -lt 120 ]; do
			sleep 1
			n=$((n + 1))
		done
		kill "$vm" 2> /dev/null || true
	fi
	wait "$vm" || true
	if [ -n "$netns" ]; then
		rm -f netns.run
		wait "$netns" || true
		netns=
	fi
	# shellcheck disable=SC2086
	kill $boot_pids 2> /dev/null || true
	boot_pids=

	sed -n '/=== E2E-REPORT/,/E2E-RESULT/p' "$console" | tr -d '\r'
	if [ "$scenario" = badsum ]; then
		if grep -q "$badsum" "$console" && ! grep -q 'E2E-RESULT' "$console"; then
			echo "PASS: $distro $initrd $backend $scenario"
		else
			echo "FAIL: $distro $initrd $backend $scenario: the boot did not stop on the checksum (see $work/$console)"
			exit 1
		fi
	elif grep -q 'E2E-RESULT: PASS' "$console"; then
		echo "PASS: $distro $initrd $backend $scenario${phase:+ ($phase)}"
	else
		echo "FAIL: $distro $initrd $backend $scenario${phase:+ ($phase)} (see $work/$console)"
		exit 1
	fi
}

case $scenario in
	edit)
		# The same disk both times, so that the second boot finds what the first one changed.
		boot edit
		boot edited
		;;
	*)
		boot ''
		;;
esac
