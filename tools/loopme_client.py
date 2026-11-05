#!/usr/bin/env python3
"""Reference client for the LoopMe daemon protocol (the iOS/Mac apps mirror this).

Usage:
  loopme_client.py local  <data-dir>             # connect as the Mac app, print a snapshot summary
  loopme_client.py pair   <loopme://pair?...> <state.json>
  loopme_client.py send   <state.json> <text>    # send a chat message and print the reply events

Needs: pip install websockets cryptography
"""
import asyncio, base64, json, os, struct, sys, uuid
from urllib.parse import urlparse, parse_qs

import websockets
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes, serialization


def b64d(s):
    s = s.strip()
    pad = "=" * (-len(s) % 4)
    try:
        return base64.b64decode(s + pad, validate=True)
    except Exception:
        return base64.urlsafe_b64decode(s + pad)


def b64e(b):
    return base64.b64encode(b).decode()


def raw_pub(priv):
    return priv.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def sas(mac_pub, dev_pub):
    import hashlib
    d = hashlib.sha256(b"loopme v1 sas" + mac_pub + dev_pub).digest()
    return "%04d" % (struct.unpack(">I", d[:4])[0] % 10000)


def parse_pair_url(url):
    q = {k: v[0] for k, v in parse_qs(urlparse(url).query).items()}
    return {"macPub": b64d(q["k"]), "code": q["c"], "hosts": q["h"].split(","), "port": int(q["p"]),
            "name": b64d(q.get("n", "")).decode() if q.get("n") else "Mac"}


class Session:
    def __init__(self, ws, keys):
        self.ws = ws
        self.tx = ChaCha20Poly1305(keys[0]); self.rx = ChaCha20Poly1305(keys[1])
        self.tx_ctr = 0; self.rx_next = 0
        self.pending = {}; self.events = asyncio.Queue()

    @classmethod
    async def connect(cls, host, port, device_id, priv, mac_pub):
        ws = await websockets.connect(f"ws://{host}:{port}/v1", max_size=None)
        cn = os.urandom(16)
        await ws.send(json.dumps({"t": "hello", "deviceId": device_id, "nonce": b64e(cn)}))
        hello = json.loads(await ws.recv())
        if hello.get("t") != "hello":
            raise RuntimeError(hello)
        sn = b64d(hello["nonce"])
        shared = priv.exchange(X25519PublicKey.from_public_bytes(mac_pub))
        def hk(info):
            return HKDF(algorithm=hashes.SHA256(), length=32, salt=cn + sn, info=info).derive(shared)
        s = cls(ws, (hk(b"loopme v1 c2s"), hk(b"loopme v1 s2c")))
        s.reader = asyncio.create_task(s._read())
        return s

    async def _read(self):
        async for frame in self.ws:
            nonce, ct = frame[:12], frame[12:]
            ctr = struct.unpack("<Q", nonce[4:])[0]
            assert nonce[:4] == b"\0\0\0\0" and ctr >= self.rx_next, "replay"
            msg = json.loads(self.rx.decrypt(nonce, ct, None)); self.rx_next = ctr + 1
            if "event" in msg:
                await self.events.put(msg)
            elif msg.get("id") in self.pending:
                self.pending.pop(msg["id"]).set_result(msg)

    async def call(self, method, params=None):
        rid = uuid.uuid4().hex
        fut = asyncio.get_running_loop().create_future(); self.pending[rid] = fut
        nonce = b"\0\0\0\0" + struct.pack("<Q", self.tx_ctr); self.tx_ctr += 1
        await self.ws.send(nonce + self.tx.encrypt(nonce, json.dumps({"id": rid, "method": method, "params": params or {}}).encode(), None))
        resp = await asyncio.wait_for(fut, 30)
        if "error" in resp:
            raise RuntimeError(resp["error"]["message"])
        return resp["result"]

    async def wait_event(self, pred, timeout=15):
        async def go():
            while True:
                e = await self.events.get()
                if pred(e):
                    return e
        return await asyncio.wait_for(go(), timeout)


async def connect_local(data_dir):
    lc = json.load(open(os.path.join(data_dir, "local-client.json")))
    priv = X25519PrivateKey.from_private_bytes(b64d(lc["privateKey"]))
    return await Session.connect("127.0.0.1", lc["port"], lc["deviceId"], priv, b64d(lc["macPublicKey"]))


async def pair(url, on_sas=None):
    info = parse_pair_url(url)
    priv = X25519PrivateKey.generate()
    pub = raw_pub(priv)
    last = None
    for host in info["hosts"]:
        try:
            ws = await asyncio.wait_for(websockets.connect(f"ws://{host}:{info['port']}/v1/pair"), 3)
            break
        except Exception as e:
            last = e
    else:
        raise RuntimeError(f"no host reachable: {last}")
    await ws.send(json.dumps({"t": "pair", "code": info["code"], "devicePub": b64e(pub), "deviceName": "Test Phone"}))
    pending = json.loads(await ws.recv())
    if pending.get("t") != "pair_pending":
        raise RuntimeError(pending)
    assert pending["sas"] == sas(info["macPub"], pub), "SAS mismatch"
    if on_sas:
        await on_sas(pending["sas"])
    done = json.loads(await ws.recv())
    if done.get("t") != "paired":
        raise RuntimeError(done)
    return {"deviceId": done["deviceId"], "privateKey": b64e(priv.private_bytes(serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption())),
            "macPublicKey": b64e(info["macPub"]), "host": host, "port": info["port"], "sas": pending["sas"]}


async def connect_state(st):
    priv = X25519PrivateKey.from_private_bytes(b64d(st["privateKey"]))
    return await Session.connect(st["host"], st["port"], st["deviceId"], priv, b64d(st["macPublicKey"]))


async def main(argv):
    if argv[0] == "local":
        s = await connect_local(argv[1])
        snap = await s.call("sync")
        print(json.dumps(snap["status"], ensure_ascii=False, indent=2))
        print(f"{len(snap['messages'])} messages, {len(snap['items'])} items, {len(snap['devices'])} devices")
    elif argv[0] == "pair":
        async def show(code):
            print("校验码:", code, "— 请在 Mac 上确认")
        st = await pair(argv[1], show)
        json.dump(st, open(argv[2], "w")); print("paired as", st["deviceId"])
    elif argv[0] == "send":
        s = await connect_state(json.load(open(argv[1])))
        await s.call("message.send", {"clientId": uuid.uuid4().hex, "text": argv[2]})
        e = await s.wait_event(lambda e: e["event"] == "message" and e["data"]["role"] == "agent" and not e["data"]["streaming"], 600)
        print(e["data"]["text"])
    else:
        print(__doc__)


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1:] or ["help"]))
