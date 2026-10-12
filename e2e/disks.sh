#!/bin/sh
# Makes the disks of a guest from a container of its image, in the current directory: vmlinuz,
# initrd.img, rootfs.img and rootfs.squashfs. run.sh runs it in podman unshare, as root in a user
# namespace, so that the files keep their owners.
#
#   e2e/disks.sh <container>
#
# An image with SELinux has the labels of its files in /.e2e-labels, as `<label> <path>` lines,
# since podman export keeps none. They go on the disks here: with debugfs on the ext4 one, and as
# pseudo files on the squashfs one.
set -eu

cid=${1:?container}

rm -rf rootfs rootfs.img rootfs.squashfs labels debugfs.cmd pseudo
mkdir rootfs
podman export "$cid" | tar -C rootfs -xf -
cp rootfs/boot/vmlinuz.seednet vmlinuz
cp rootfs/boot/initrd.seednet.img initrd.img
chmod 644 initrd.img

if [ -e rootfs/.e2e-labels ]; then
	mv rootfs/.e2e-labels labels
	awk '{ printf "ea_set \"%s\" security.selinux %s\n", $2, $1 }' labels > debugfs.cmd
	awk '{ p = $2; sub("^/", "", p); if (p == "") p = "/"; printf "%s x security.selinux=%s\n", p, $1 }' \
		labels > pseudo
fi

mkfs.ext4 -q -d rootfs rootfs.img 3G
if [ -e debugfs.cmd ]; then
	debugfs -w -f debugfs.cmd rootfs.img > debugfs.log 2>&1
fi
if [ -e pseudo ]; then
	mksquashfs rootfs rootfs.squashfs -noappend -quiet -pf pseudo
else
	mksquashfs rootfs rootfs.squashfs -noappend -quiet
fi
rm -rf rootfs
