"""Generate a dedicated QCCOM rendezvous server and two trusted frpc profiles."""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tempfile
import zipfile


def hostname(value):
    try:
        return str(ipaddress.IPv4Address(value))
    except ValueError:
        if len(value) <= 253 and all(re.fullmatch(r"[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?", label)
                                     for label in value.split(".")):
            return value
    raise argparse.ArgumentTypeError("Expected an IPv4 address or DNS name")


def generate(root, server, stun):
    token = secrets.token_hex(32)
    key = secrets.token_hex(32)
    name = "qccom-" + secrets.token_hex(8)
    conf = root / "server"
    conf.mkdir()
    # A dedicated, pinned server certificate. No private key leaves the server.
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:3072", "-sha256", "-nodes",
                    "-days", "730", "-subj", "/CN=qccom-rendezvous",
                    "-addext", "subjectAltName=DNS:qccom-rendezvous",
                    "-addext", "extendedKeyUsage=serverAuth",
                    "-keyout", "server.key", "-out", "server.crt"], cwd=conf, check=True)
    (conf / "frps.toml").write_text(f'''bindPort = 7000
proxyBindAddr = "127.0.0.1"
auth.token = "{token}"
transport.tls.force = true
transport.tls.certFile = "/config/server.crt"
transport.tls.keyFile = "/config/server.key"
''')
    # The parent stays owner-only. Inside the read-only bind mount the
    # unprivileged container can traverse server/ and read its three files.
    conf.chmod(0o755)
    for path in conf.iterdir():
        path.chmod(0o444)
    for role in ("host", "client"):
        invitation = dict(server=server, stun=stun, token=token, secret=key, channel=name)
        with zipfile.ZipFile(root / f"{role}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(f"{role}.json", json.dumps(invitation))
            archive.write(conf / "server.crt", "server.crt")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("server", type=hostname)
    p.add_argument("--stun", default="stun.easyvoip.com:3478", help="RFC 5780 STUN endpoint (host:port)")
    args = p.parse_args()
    host, sep, port = args.stun.rpartition(":")
    if not sep or not port.isdigit() or not 1 <= int(port) <= 65535:
        p.error("Expected --stun host:port")
    hostname(host)
    os.umask(0o077)
    target = Path("pair")
    if target.exists():
        p.error("pair already exists; keep it for redeployment or move it aside to rotate credentials")
    with tempfile.TemporaryDirectory(dir=".") as tmp:
        output = Path(tmp) / "pair"
        output.mkdir()
        generate(output, args.server, args.stun)
        output.rename(target)
    print("pair/host.zip -> device PC; pair/client.zip -> flashing PC")


if __name__ == "__main__":
    main()
