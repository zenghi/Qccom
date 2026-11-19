"""Run against a real Mosquitto broker and pyftpdlib in one namespace.

python3 tests/remote_mappings.py /path/to/qccom-server /path/to/tunnel-worker
"""
from contextlib import ExitStack
from ftplib import FTP
from io import BytesIO
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer
from remote_com_rust import launch, wait, SERVER, WORKER


def mapping(kind, local_port):
    return dict(service=kind, number=7001 if kind=="Com" else local_port,
                host="127.0.0.2", port=local_port, passive=[51000, 51003], serial="", label="")


def transfer(root, relay):
    host = [mapping("Mqtt", 21883), mapping("Ftp", 21021)]
    client = [dict(m, port=m["port"] + 1000) for m in host]
    with ExitStack() as stack:
        for role, maps in [("host", host), ("client", client)]:
            path = root / (role + "-maps.json")
            path.write_text(json.dumps(maps))
            cmd = [WORKER, "mappings", root/"qccom-pair"/(role+".json"), path]
            if relay:
                cmd.append("--relay-only")
            worker=stack.enter_context(launch(cmd, root, role))
            for service in ("MQTT","FTP"):
                wait(worker, service + (" Waiting" if role=="host" else " Online { direct: " + str(not relay).lower()), 30)
        sub=stack.enter_context(subprocess.Popen(["mosquitto_sub","-h","127.0.0.1","-p","22883","-q","1","-t","qccom/test","-C","1","-W","10"],stdout=subprocess.PIPE))
        payload=os.urandom(256*1024)
        with FTP() as ftp:
            ftp.connect("127.0.0.1",22021,timeout=8)
            ftp.trust_server_pasv_ipv4_address=True
            ftp.login("test","test")
            ftp.storbinary("STOR board.bin",BytesIO(payload))
            copy=BytesIO()
            ftp.retrbinary("RETR board.bin",copy.write)
            assert payload==copy.getvalue()
            assert ftp.sendcmd("EPSV").startswith("229 ")
            assert "(127,0,0,1," in ftp.sendcmd("PASV")
            ftp.quit()
        subprocess.run(["mosquitto_pub","-h","127.0.0.1","-p","22883","-t","qccom/test","-m","pair-test","-q","1","-r"],check=True,timeout=8)
        assert sub.communicate(timeout=12)[0].strip()==b"pair-test" and sub.returncode==0
    print(("forced relay" if relay else "direct"),"simultaneous MQTT QoS1 and FTP binary upload/download OK",flush=True)


def main():
    with tempfile.TemporaryDirectory() as folder, ExitStack() as stack:
        root=Path(folder)
        subprocess.run([SERVER,"init","127.0.0.1"],cwd=root,check=True)
        server=stack.enter_context(launch([SERVER,"serve"],root,"server"))
        wait(server,"rendezvous ready")
        cfg=root/"mosquitto.conf"
        cfg.write_text("listener 21883 127.0.0.2\nallow_anonymous true\n")
        stack.enter_context(launch(["mosquitto","-c",cfg],root,"mqtt"))
        auth=DummyAuthorizer()
        auth.add_user("test","test",folder,perm="elradfmw")
        class Handler(FTPHandler):
            authorizer=auth
            passive_ports=range(51000,51004)
            masquerade_address="192.0.2.123"
        ftp=FTPServer(("127.0.0.2",21021),Handler)
        threading.Thread(target=ftp.serve_forever,daemon=True).start()
        stack.callback(ftp.close_all)
        transfer(root,False)
        transfer(root,True)

if __name__=="__main__":
    main()
