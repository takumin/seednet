#!/bin/sh
# Makes the CA that the guests trust, and the certificate of the HTTPS server of the tests for the
# host name <name>, in <dir>, once. Runs at once make it once: each makes its own, and the first to
# rename it to <dir> wins.
#
#   e2e/tls.sh <dir> <name>
set -eu

dir=${1:?directory}
name=${2:?host name}

[ -e "$dir/server.crt" ] && exit 0
mkdir -p "$(dirname "$dir")"
tmp=$(mktemp -d "$dir.XXXXXX")
(
	cd "$tmp"
	openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc -days 3650 \
		-subj '/CN=seednet e2e CA' -keyout ca.key -out ca.crt \
		-addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign 2> /dev/null
	openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc \
		-subj "/CN=$name" -keyout server.key -out server.csr 2> /dev/null
	printf 'subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' \
		"$name" > server.ext
	openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -days 3650 \
		-extfile server.ext -out server.crt 2> /dev/null
)
# rename(2) replaces an empty directory but not one that another run filled.
mv -T "$tmp" "$dir" 2> /dev/null || rm -rf "$tmp"
