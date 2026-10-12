"""The servers of the end-to-end tests: the config and the live rootfs over HTTP or HTTPS, and a
DNS server that knows one name.

    server.py WWW PORTFILE [--port N] [--tls CERT KEY] [--dns NAME ADDRESS]

It serves WWW on 127.0.0.1, on --port or else a free port, which it writes to PORTFILE when it is
ready. With --dns, it answers A queries for NAME with ADDRESS on 127.0.0.1:53, and has no other
names.
"""

import argparse
import functools
import http.server
import ipaddress
import socket
import ssl
import struct
import threading


def dns_answer(query: bytes, name: str, address: bytes) -> bytes | None:
    """The answer to one query, or None for what is not a query."""
    if len(query) < 12:
        return None
    ident, flags, qdcount = struct.unpack("!HHH", query[:6])
    if flags & 0x8000 or qdcount != 1:
        return None
    labels, i = [], 12
    while i < len(query) and query[i]:
        labels.append(query[i + 1 : i + 1 + query[i]].decode("ascii", "replace"))
        i += 1 + query[i]
    question = query[12 : i + 5]
    if len(question) != i + 5 - 12:
        return None
    qtype, qclass = struct.unpack("!HH", question[-4:])
    known = ".".join(labels).lower() == name

    answers = b""
    if known and qtype == 1 and qclass == 1:
        # A pointer to the name in the question, A, IN, a TTL of 60 and the address.
        answers = struct.pack("!HHHIH", 0xC00C, 1, 1, 60, 4) + address
    # A response, authoritative, with recursion as asked and available, and NXDOMAIN for others.
    rcode = 0 if known else 3
    flags = 0x8400 | (flags & 0x0100) | 0x0080 | rcode
    header = struct.pack("!HHHHHH", ident, flags, 1, 1 if answers else 0, 0, 0)
    return header + question + answers


def serve_dns(name: str, address: str) -> None:
    packed = ipaddress.IPv4Address(address).packed
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 53))
    while True:
        query, peer = sock.recvfrom(4096)
        answer = dns_answer(query, name.lower().rstrip("."), packed)
        if answer:
            sock.sendto(answer, peer)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("www")
    parser.add_argument("portfile")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--tls", nargs=2, metavar=("CERT", "KEY"))
    parser.add_argument("--dns", nargs=2, metavar=("NAME", "ADDRESS"))
    args = parser.parse_args()

    if args.dns:
        threading.Thread(target=serve_dns, args=args.dns, daemon=True).start()

    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=args.www)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    if args.tls:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(*args.tls)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    with open(args.portfile, "w") as portfile:
        portfile.write(str(server.server_port))
    server.serve_forever()


if __name__ == "__main__":
    main()
