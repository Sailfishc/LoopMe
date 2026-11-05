#!/usr/bin/env python3
"""End-to-end test: daemon + mock Codex + a simulated phone.

  python3 tools/e2e_test.py [path/to/loopme-daemon]
"""
import asyncio, json, os, subprocess, sys, tempfile, time, uuid
sys.path.insert(0, os.path.dirname(__file__))
import loopme_client as lc

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


async def main():
    daemon = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "daemon/target/debug/loopme-daemon")
    data = tempfile.mkdtemp(prefix="loopme-e2e-")
    port = 17788
    env = dict(os.environ, LOOPME_CODEX=os.path.join(ROOT, "tools/mock-codex.py"), LOOPME_HOSTS="127.0.0.1", RUST_LOG="info")
    proc = subprocess.Popen([daemon, "--data-dir", data, "--port", str(port), "--bind", "127.0.0.1"], env=env)
    try:
        for _ in range(50):
            if os.path.exists(os.path.join(data, "local-client.json")):
                break
            time.sleep(0.1)
        await asyncio.sleep(0.3)
        mac = await lc.connect_local(data)
        snap = await mac.call("sync")
        assert snap["status"]["listenPort"] == port

        # Pairing: QR -> phone -> SAS -> confirm on Mac.
        st = await mac.call("pairing.start")
        async def confirm(code):
            ev = await mac.wait_event(lambda e: e["event"] == "pairing" and e["data"]["state"] == "confirm")
            assert ev["data"]["sas"] == code
            await mac.call("pairing.confirm", {"accept": True})
        phone_state = await lc.pair(st["url"], confirm)
        phone = await lc.connect_state(phone_state)
        print("paired + encrypted session OK")

        # Non-local devices cannot use admin methods.
        try:
            await phone.call("pairing.start"); raise AssertionError("phone must not start pairing")
        except RuntimeError:
            pass

        # Chat: streaming reply, item creation, memory.
        cid = uuid.uuid4().hex
        r1 = await phone.call("message.send", {"clientId": cid, "text": "帮我盯着 PR 的 CI"})
        r2 = await phone.call("message.send", {"clientId": cid, "text": "帮我盯着 PR 的 CI"})
        assert r1 == r2, "idempotent send"
        item_ev = await phone.wait_event(lambda e: e["event"] == "item", 20)
        assert item_ev["data"]["title"] == "盯住 PR 的 CI"
        snap = await phone.call("sync")
        users = [m for m in snap["messages"] if m["role"] == "user"]
        assert len(users) == 1, "deduplicated"
        agent = [m for m in snap["messages"] if m["role"] == "agent"]
        assert agent and "<loopme>" not in agent[-1]["text"] and agent[-1]["itemId"], agent
        assert any(m["text"] == "用户在做 LoopMe 项目" for m in snap["memory"])
        print("chat + item + memory OK")

        # Delete memory -> tombstone blocks re-adding.
        mem = next(m for m in snap["memory"] if m["text"] == "用户在做 LoopMe 项目")
        await phone.call("memory.delete", {"id": mem["id"]})
        await phone.call("message.send", {"clientId": uuid.uuid4().hex, "text": "再帮我盯着另一个 PR"})
        await phone.wait_event(lambda e: e["event"] == "message" and e["data"]["role"] == "agent" and not e["data"]["streaming"], 20)
        await asyncio.sleep(0.5)
        snap = await phone.call("sync")
        assert [m["status"] for m in snap["memory"] if m["text"] == "用户在做 LoopMe 项目"] == ["deleted"]
        print("memory tombstone OK")

        # Approval -> decision -> answer from phone.
        await phone.call("message.send", {"clientId": uuid.uuid4().hex, "text": "跑一个命令"})
        dec = await phone.wait_event(lambda e: e["event"] == "decision" and e["data"]["status"] == "pending", 20)
        await phone.call("decision.answer", {"decisionId": dec["data"]["id"], "choice": "approve", "clientId": uuid.uuid4().hex})
        reply = await phone.wait_event(lambda e: e["event"] == "message" and e["data"]["role"] == "agent" and not e["data"]["streaming"] and "审批" in e["data"]["text"], 20)
        assert "accept" in reply["data"]["text"], reply
        print("approval OK")

        # Item check now -> scheduled again with a check counted.
        item_id = item_ev["data"]["id"]
        await phone.call("item.action", {"itemId": item_id, "action": "checkNow"})
        def seen(e):
            return e["event"] == "item" and e["data"]["id"] == item_id and e["data"]["checks"] == 1
        ev = await phone.wait_event(seen, 20)
        assert ev["data"]["status"] == "scheduled" and ev["data"]["unchangedChecks"] == 1
        print("item check OK")

        # Session analysis -> suggestion card from Codex sessions (the mock suggests nothing, but the run must succeed).
        await mac.call("brief.now")
        await mac.wait_event(lambda e: e["event"] == "status" and e["data"]["agent"] == "idle", 20)

        # Revoke -> phone is disconnected and cannot reconnect.
        await mac.call("devices.revoke", {"deviceId": phone_state["deviceId"]})
        await asyncio.sleep(0.3)
        try:
            await lc.connect_state(phone_state); raise AssertionError("revoked device connected")
        except RuntimeError as e:
            assert "unknown_device" in str(e)
        print("revoke OK")
        print("ALL OK")
    finally:
        proc.terminate(); proc.wait(5)


asyncio.run(main())
