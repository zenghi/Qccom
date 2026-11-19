"""Exercise the production Rust transport. Usage: python3 tests/remote_com_rust.py SERVER WORKER"""
from contextlib import ExitStack, contextmanager
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

SERVER, WORKER = (Path(arg).resolve() for arg in sys.argv[1:])


@contextmanager
def launch(args, root, name):
    path = root / (name + ".log")
    with path.open("wb") as log:
        proc = subprocess.Popen(args, cwd=root, stdout=log, stderr=subprocess.STDOUT)
        try:
            yield proc, path
        finally:
            proc.terminate()
            proc.wait(timeout=10)


def wait(proc_log, marker, seconds=22):
    proc, log = proc_log
    end = time.monotonic() + seconds
    while marker not in log.read_text():
        if proc.poll() is not None or time.monotonic() > end:
            raise AssertionError(f"Missing {marker}: {log.read_text()}")
        time.sleep(0.05)


def receive(sock, size):
    result = bytearray()
    while len(result) < size:
        chunk = sock.recv(size - len(result))
        if not chunk:
            raise EOFError("serial stream closed early")
        result.extend(chunk)
    return bytes(result)


def worker_args(profile, port):
    args = [WORKER]
    if WORKER.name.lower() == "qccom-portable.exe":
        args.append("--qccom-tunnel")
    return [*args, profile, str(port)]


def transfer(root, relay_only):
    with ExitStack() as stack:
        device = stack.enter_context(socket.socket())
        device.bind(("127.0.0.1", 0))
        device.listen(1)
        device.settimeout(18)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        for role, local in [("host", device.getsockname()[1]), ("client", port)]:
            args = worker_args(root / "qccom-pair" / (role + ".json"), local)
            if relay_only:
                args.append("--relay-only")
            worker = stack.enter_context(launch(args, root, role))
            wait(worker, "READY")
        for seq in range(2):
            with socket.create_connection(("127.0.0.1", port), 8) as flasher, device.accept()[0] as board:
                flasher.settimeout(10)
                board.settimeout(10)
                for sender, receiver in [(flasher, board), (board, flasher)]:
                    payload = os.urandom(65537) + bytes(range(256))
                    sender.sendall(payload)
                    assert receive(receiver, len(payload)) == payload
                if seq == 0:
                    wait(worker, "PATH relay" if relay_only else "PATH direct")
                # Verify EOF on the other side before re-opening the virtual COM.
                flasher.shutdown(socket.SHUT_WR)
                assert board.recv(1) == b""
                board.shutdown(socket.SHUT_WR)
                assert flasher.recv(1) == b""
        print("relay" if relay_only else "direct", "binary both directions, close and reopen OK", flush=True)


def main():
    with tempfile.TemporaryDirectory() as folder:
        root = Path(folder)
        subprocess.run([SERVER, "init", "127.0.0.1"], cwd=root, check=True)
        first = (root / "qccom-pair/host.json").read_bytes()
        assert subprocess.run([SERVER, "init", "127.0.0.1"], cwd=root, capture_output=True).returncode
        assert first == (root / "qccom-pair/host.json").read_bytes()
        with launch([SERVER, "serve"], root, "server") as service:
            wait(service, "rendezvous ready")
            transfer(root, False)
            if WORKER.name.lower() != "qccom-portable.exe":
                transfer(root, True)
            other = root / "other"
            other.mkdir()
            subprocess.run([SERVER, "init", "127.0.0.1"], cwd=other, check=True)
            unauthorized = json.loads((other / "qccom-pair/host.json").read_text())
            # Keep the real server certificate. Only the endpoint identity is wrong.
            unauthorized["certificate"] = json.loads(first)["certificate"]
            profile = other / "wrong.json"
            profile.write_text(json.dumps(unauthorized))
            attempt = subprocess.run(worker_args(profile, 10000), capture_output=True, timeout=22)
            assert attempt.returncode and b"READY" not in attempt.stdout
            print("unpaired identity rejected")


if __name__ == "__main__":
    main()
