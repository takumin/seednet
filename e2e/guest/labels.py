"""Prints the SELinux label of each file of the image, as `<label> <path>` lines, for run.sh to put
on the disks that it makes from the image: podman export keeps no labels. A path with white space,
a quote or a backslash is left out, since the tools that run.sh gives the labels to would split it.
"""

import os
import re
import sys

import selinux

UNSAFE = re.compile(r'[\s"\\]')


def main():
    left_out = 0
    for root, dirs, files in os.walk("/"):
        # What the build mounts, such as /proc, is not in the image, only the directory under it,
        # and what is in /run is not either, which is a tmpfs at boot.
        dirs[:] = [name for name in dirs if not os.path.ismount(os.path.join(root, name))]
        if root == "/run":
            dirs[:], files = [], []
        if root == "/":
            files = [name for name in files if name != ".e2e-labels"]
        paths = [root] if root == "/" else []
        paths += [os.path.join(root, name) for name in dirs + files]
        for path in paths:
            if UNSAFE.search(path):
                left_out += 1
                continue
            try:
                mode = os.lstat(path).st_mode
                _, label = selinux.matchpathcon(path, mode)
            except OSError:
                # No label for it in the policy, or it went away.
                continue
            print(label, path)
    if left_out:
        print(f"labels.py: left out {left_out} paths", file=sys.stderr)


main()
