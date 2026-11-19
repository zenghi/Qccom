"""Run with Python 3.9+ on Linux and frpc/frps 0.72.0 on PATH. No device required."""
import contextlib
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TEMPLATES = ROOT / "src/remote_com"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for(check, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError("Timed out waiting for " + check.__doc__)


def binding_servers(stack):
    # Deterministic STUN responses for frp's two binding probes.
    endpoints = [stack.enter_context(socket.socket(socket.AF_INET, socket.SOCK_DGRAM)) for _ in range(2)]
    for sock in endpoints:
        sock.bind(("127.0.0.1", 0))
    stop = threading.Event()

    def address(kind, ip, port):
        return struct.pack("!HHBBH4s", kind, 8, 0, 1, port, socket.inet_aton(ip))

    def serve():
        while not stop.is_set():
            for sock in select.select(endpoints, [], [], 0.1)[0]:
                packet, peer = sock.recvfrom(512)
                if len(packet) == 20:
                    other = endpoints[1 - endpoints.index(sock)]
                    attrs = address(1, *peer) + address(0x802C, *other.getsockname())
                    sock.sendto(struct.pack("!HH", 0x101, len(attrs)) + packet[4:] + attrs, peer)

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    stack.callback(thread.join)
    stack.callback(stop.set)
    return endpoints[0].getsockname()[1]


class TunnelTest(unittest.TestCase):
    def exercise(self, direct):
        with contextlib.ExitStack() as stack:
            tmp = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            stun = binding_servers(stack)
            if not direct:
                # Advertise a bound socket that never responds.
                blackhole = stack.enter_context(socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
                blackhole.bind(("127.0.0.1", 0))
                stun = blackhole.getsockname()[1]
            subprocess.run([sys.executable, ROOT / "deploy/remote-com/pair.py", "127.0.0.1",
                            "--stun", f"127.0.0.1:{stun}"], cwd=tmp, check=True, capture_output=True)
            server = tmp / "pair/server"
            control = free_port()
            settings = (server / "frps.toml").read_text().replace("/config/", "./").replace("7000", str(control), 1)
            (server / "local.toml").write_text('bindAddr = "127.0.0.1"\n' + settings)

            def start(exe, config, workdir, env):
                log = tmp / (config.stem + ".log")
                out = stack.enter_context(log.open("w"))
                child = subprocess.Popen([exe, "-c", config], cwd=workdir, env=env, stdout=out, stderr=subprocess.STDOUT)
                stack.callback(child.wait)
                stack.callback(child.terminate)
                return log

            coordinator = start("frps", server / "local.toml", server, os.environ)

            def listening():
                """the coordinator to listen"""
                return "frps started successfully" in coordinator.read_text()
            wait_for(listening)
            endpoint = stack.enter_context(socket.socket())
            endpoint.bind(("127.0.0.1", 0))
            endpoint.listen()
            endpoint.settimeout(12)
            visitor = free_port()
            for role in ("host", "client"):
                with zipfile.ZipFile(tmp / f"pair/{role}.zip") as archive:
                    invite = json.loads(archive.read(role + ".json"))
                    self.assertEqual(archive.read("server.crt"), (server / "server.crt").read_bytes())
                    self.assertEqual(set(archive.namelist()), {role + ".json", "server.crt"})
                cfg = tmp / (role + ".toml")
                cfg.write_text((TEMPLATES / cfg.name).read_text().replace("serverPort = 7000", f"serverPort = {control}"))
                env = {k: v for k, v in os.environ.items() if k.lower() not in ("http_proxy", "https_proxy")}
                env.update({"QCCOM_" + k.upper(): v for k, v in invite.items()})
                env.update(QCCOM_CA=str(server / "server.crt"),
                           QCCOM_LOOP_PORT=str(visitor if role == "client" else endpoint.getsockname()[1]))
                log = start("frpc", cfg, tmp, env)

                def registered():
                    """both proxy or visitor registrations"""
                    marker = "start visitor success" if role == "client" else "start proxy success"
                    return log.read_text().count(marker) >= 2
                wait_for(registered)

            if direct:
                def punched():
                    """the QUIC tunnel"""
                    return "establishing nat hole connection successful" in log.read_text()
                wait_for(punched)

            for size in (256 * 1024, 65549):
                with contextlib.ExitStack() as pair:
                    client = pair.enter_context(socket.create_connection(("127.0.0.1", visitor), 12))
                    # A QUIC stream becomes visible to the peer on the first write.
                    # hub4com likewise initiates Telnet negotiation on connect.
                    client.sendall(b"Q")
                    host = pair.enter_context(endpoint.accept()[0])
                    host.settimeout(12)
                    self.assertEqual(host.recv(1), b"Q")
                    for writer, reader in ((client, host), (host, client)):
                        reader.settimeout(12)
                        payload = os.urandom(size)
                        writer.sendall(payload)
                        with reader.makefile("rb") as stream:
                            self.assertEqual(stream.read(size), payload)
            self.assertEqual("try to transfer connection to visitor: serial-stcp" in log.read_text(), not direct)

    def test_direct(self):
        self.exercise(True)

    def test_udp_unavailable(self):
        self.exercise(False)


if __name__ == "__main__":
    unittest.main()
