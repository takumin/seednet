"""Makes the kernel of a guest one that cloud-hypervisor boots directly.

    python3 -I e2e/kernel.py <vmlinuz>

On x86_64, cloud-hypervisor boots a bzImage, so the file stays as it is. On aarch64, it boots only
an uncompressed Image, which the distributions ship compressed: with gzip, as Debian does, or as an
EFI zboot image, as Fedora does, with the payload compressed in it. The file is replaced with the
Image that it holds.
"""

import gzip
import lzma
import os
import platform
import shutil
import struct
import subprocess
import sys

# The magic of an arm64 Image, at 0x38 of its header.
ARM64_MAGIC = b"ARM\x64"


def decompress(data, kind):
    if kind == "gzip":
        return gzip.decompress(data)
    if kind in ("xz", "lzma"):
        return lzma.decompress(data)
    if kind == "zstd":
        return subprocess.run(
            ["zstd", "-dc"], input=data, stdout=subprocess.PIPE, check=True
        ).stdout
    raise SystemExit(f"unsupported compression of the kernel: {kind}")


def image(data):
    if data[0x38:0x3C] == ARM64_MAGIC:
        return data
    if data[:2] == b"\x1f\x8b":
        return image(gzip.decompress(data))
    # EFI zboot: "MZ", then "zimg", the offset and size of the payload, and its compression.
    if data[:2] == b"MZ" and data[4:8] == b"zimg":
        offset, size = struct.unpack_from("<II", data, 8)
        kind = data[24:32].split(b"\0")[0].decode()
        return image(decompress(data[offset : offset + size], kind))
    raise SystemExit("the kernel is not an arm64 Image, gzipped or in EFI zboot")


def main():
    path = sys.argv[1]
    if platform.machine() != "aarch64":
        return
    with open(path, "rb") as f:
        data = image(f.read())
    tmp = path + ".tmp"
    with open(tmp, "wb") as f:
        f.write(data)
    shutil.copymode(path, tmp)
    os.replace(tmp, path)


main()
