#!/usr/bin/env python3
"""End-to-end test: daemon + mock Codex + a simulated phone.

  python3 tools/e2e_test.py [path/to/loopme-daemon]
"""
import asyncio, json, os, subprocess, sys, tempfile, time, uuid
sys.path.insert(0, os.path.dirname(__file__))
import loopme_client as lc

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def fire_hook(daemon, data, payload, wait=True):
    """Runs the daemon binary the way Codex runs a hook."""
    p = subprocess.Popen([daemon, "hook", "--loopme-data-dir", data], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    p.stdin.write(json.dumps(payload)); p.stdin.close()
    p.stdin = None  # communicate() must not flush the pipe we already closed.
    if wait:
        p.wait(10)
    return p


async def codex_hooks_flow(daemon, data, codex_home, mac, phone):
    # Phones cannot change hooks; the Mac's one-time confirm installs and trusts them.
    try:
        await phone.call("hooks.enable"); raise AssertionError("phone must not enable hooks")
    except RuntimeError:
        pass
    r = await mac.call("hooks.enable")
    assert r["hooks"] == "on", r
    cfg = json.load(open(os.path.join(codex_home, "hooks.json")))
    assert set(cfg["hooks"]) == {"SessionStart", "UserPromptSubmit", "Stop", "PermissionRequest", "SessionEnd", "Interrupt"}
    print("codex hooks installed + trusted OK")

    # A git repo standing in for the user's project, with uncommitted work.
    repo = tempfile.mkdtemp(prefix="loopme-repo-")
    for args in (["init", "-q", "-b", "fix-login"], ["config", "user.name", "t"], ["config", "user.email", "t@t"]):
        subprocess.run(["git", "-C", repo] + args, check=True)
    open(os.path.join(repo, "a.txt"), "w").write("one\n")
    subprocess.run(["git", "-C", repo, "add", "."], check=True)
    subprocess.run(["git", "-C", repo, "commit", "-q", "-m", "init"], check=True)
    open(os.path.join(repo, "a.txt"), "w").write("one\ntwo\n")

    base = {"session_id": "s_user", "cwd": repo, "transcript_path": None, "model": "m", "permission_mode": "default"}
    fire_hook(daemon, data, dict(base, hook_event_name="SessionStart", source="startup"))
    fire_hook(daemon, data, dict(base, hook_event_name="UserPromptSubmit", turn_id="t1", prompt="修一下登录测试"))
    ev = await phone.wait_event(lambda e: e["event"] == "session" and e["data"]["state"] == "working", 10)
    assert ev["data"]["title"] == "修一下登录测试", ev

    # Permission request while away -> decision on the phone -> the hook answers Codex.
    hp = fire_hook(daemon, data, dict(base, hook_event_name="PermissionRequest", turn_id="t1", tool_name="Bash",
                                       tool_input={"command": "npm install --save-dev vitest@3"}), wait=False)
    dec = await phone.wait_event(lambda e: e["event"] == "decision" and e["data"]["kind"] == "sessionApproval", 10)
    assert "vitest" in dec["data"]["title"]
    await phone.call("decision.answer", {"decisionId": dec["data"]["id"], "choice": "approve", "clientId": uuid.uuid4().hex})
    out = hp.communicate(timeout=10)[0]
    assert json.loads(out)["hookSpecificOutput"]["decision"]["behavior"] == "allow", out
    print("remote approval of the user's own Codex OK")

    # The session stops while the user is away -> merged signal -> takeover offer.
    fire_hook(daemon, data, dict(base, hook_event_name="Stop", turn_id="t1", stop_hook_active=False,
                                  last_assistant_message="2 tests failed in tests/test_login.py"))
    offer = await phone.wait_event(lambda e: e["event"] == "message" and (e["data"].get("offer") or {}).get("kind") == "takeover", 30)
    o = offer["data"]["offer"]
    assert [a["id"] for a in o["actions"]] == ["take", "mine", "ignore"] and o["sessionId"] == "s_user", o
    print("signal -> takeover offer OK")

    # 让我来 -> work on a separate branch in a worktree -> result card; the user's copy is untouched.
    await phone.call("offer.act", {"messageId": offer["data"]["id"], "action": "take", "clientId": uuid.uuid4().hex})
    res = await phone.wait_event(lambda e: e["event"] == "message" and (e["data"].get("offer") or {}).get("kind") == "result", 30)
    ro = res["data"]["offer"]
    assert ro["branch"].startswith("loopme/") and [a["id"] for a in ro["actions"]] == ["diff", "push", "undo"], ro
    diff = (await phone.call("offer.diff", {"messageId": res["data"]["id"]}))["diff"]
    assert "+fix" in diff and "two" not in diff, diff
    head = subprocess.run(["git", "-C", repo, "rev-parse", "--abbrev-ref", "HEAD"], capture_output=True, text=True).stdout.strip()
    assert head == "fix-login" and not os.path.exists(os.path.join(repo, "loopme-fix.txt"))
    snap = await phone.call("sync")
    first = next(m for m in snap["messages"] if m["id"] == offer["data"]["id"])
    assert first["offer"]["state"] == "done"
    undone = await phone.call("offer.act", {"messageId": res["data"]["id"], "action": "undo"})
    assert undone["offer"]["state"] == "closed"
    branches = subprocess.run(["git", "-C", repo, "branch", "--list", "loopme/*"], capture_output=True, text=True).stdout
    assert branches.strip() == "", branches
    print("takeover on a separate branch + diff + undo OK")

    # The session shows up for the "现在" bar.
    assert any(s["id"] == "s_user" for s in snap["sessions"])

    r = await mac.call("hooks.disable")
    assert r["hooks"] == "off"
    cfg = json.load(open(os.path.join(codex_home, "hooks.json")))
    assert cfg["hooks"] == {}, cfg
    print("codex hooks removed OK")


def healthz(port, query=""):
    import hashlib, http.client, ssl
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    c = http.client.HTTPSConnection("127.0.0.1", port, context=ctx, timeout=5)
    c.request("GET", "/healthz" + query)
    fp = hashlib.sha256(c.sock.getpeercert(binary_form=True)).hexdigest()
    body = json.loads(c.getresponse().read())
    c.close()
    return body, fp


async def relay_checks(data, relay_port, mac):
    snap = await mac.call("sync")
    st = snap["status"]
    assert st["relay"] == "on" and st["relayPort"] == relay_port, st
    body, fp = await asyncio.to_thread(healthz, relay_port)
    assert body["ok"] and "macOnline" not in body, body
    assert fp == st["relayFingerprint"], (fp, st["relayFingerprint"])
    # The daemon itself no longer listens on the network.
    assert snap["status"]["listenPort"] == 17788
    print("relay up, certificate pinned by fingerprint OK")


async def relay_auth_checks(phone_state):
    r = phone_state["relay"]
    body, _ = await asyncio.to_thread(healthz, phone_state["port"], f"?mac={r['macId']}&t={r['token']}")
    assert body["macOnline"] is True, body
    body, _ = await asyncio.to_thread(healthz, phone_state["port"], f"?mac={r['macId']}&t=wrong")
    assert body["macOnline"] is False, body
    bad = dict(r, token="x" * 24)
    ws = await lc.open_ws(lc.endpoint("127.0.0.1", phone_state["port"], bad, "session"), bad)
    err = json.loads(await ws.recv())
    assert err["code"] == "bad_token", err
    await ws.close()
    pinned_wrong = dict(r, fingerprint="0" * 64)
    try:
        await lc.open_ws(lc.endpoint("127.0.0.1", phone_state["port"], pinned_wrong, "session"), pinned_wrong)
        raise AssertionError("wrong fingerprint must be refused")
    except RuntimeError:
        pass
    print("relay rejects wrong token and wrong certificate OK")


async def main():
    daemon = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "daemon/target/debug/loopme-daemon")
    data = tempfile.mkdtemp(prefix="loopme-e2e-")
    port = 17788
    codex_home = tempfile.mkdtemp(prefix="loopme-codex-home-")
    env = dict(os.environ, LOOPME_CODEX=os.path.join(ROOT, "tools/mock-codex.py"), LOOPME_HOSTS="127.0.0.1", RUST_LOG="info",
               CODEX_HOME=codex_home, LOOPME_MERGE_MS="500")
    # The user is away from the Mac, so signals reach the phone.
    os.makedirs(data, exist_ok=True)
    open(os.path.join(data, "presence"), "w").write("away")
    relay_port = 18443
    proc = subprocess.Popen([daemon, "--data-dir", data, "--port", str(port), "--relay-port", str(relay_port)], env=env)
    try:
        for _ in range(50):
            if os.path.exists(os.path.join(data, "local-client.json")):
                break
            time.sleep(0.1)
        await asyncio.sleep(0.3)
        mac = await lc.connect_local(data)
        snap = await mac.call("sync")
        assert snap["status"]["listenPort"] == port
        for _ in range(50):
            if (await mac.call("sync"))["status"]["relay"] == "on":
                break
            await asyncio.sleep(0.1)
        await relay_checks(data, relay_port, mac)

        # Pairing: QR -> phone -> SAS -> confirm on Mac.
        st = await mac.call("pairing.start")
        async def confirm(code):
            ev = await mac.wait_event(lambda e: e["event"] == "pairing" and e["data"]["state"] == "confirm")
            assert ev["data"]["sas"] == code
            await mac.call("pairing.confirm", {"accept": True})
        assert "&r=" in st["url"] and "&f=" in st["url"], st["url"]
        phone_state = await lc.pair(st["url"], confirm)
        assert phone_state["relay"]["fingerprint"]
        phone = await lc.connect_state(phone_state)
        print("paired + encrypted session through the relay OK")
        await relay_auth_checks(phone_state)

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

        # Reply with context and 👎 feedback.
        snap = await phone.call("sync")
        last_agent = [m for m in snap["messages"] if m["role"] == "agent"][-1]
        await phone.call("message.send", {"clientId": uuid.uuid4().hex, "text": "这条再说说", "replyTo": last_agent["id"]})
        rep = await phone.wait_event(lambda e: e["event"] == "message" and e["data"]["role"] == "agent" and not e["data"]["streaming"] and "引用" in e["data"]["text"], 20)
        snap = await phone.call("sync")
        assert any(m["role"] == "user" and m.get("replyTo") == last_agent["id"] for m in snap["messages"])
        fb = await phone.call("message.feedback", {"messageId": rep["data"]["id"], "value": "down"})
        assert fb["feedback"] == "down"
        print("reply context + feedback OK")

        await codex_hooks_flow(daemon, data, codex_home, mac, phone)

        # Session analysis -> suggestion card from Codex sessions (the mock suggests nothing, but the run must succeed).
        await mac.call("brief.now")
        await mac.wait_event(lambda e: e["event"] == "status" and e["data"]["agent"] == "idle", 20)

        # Revoke -> phone is disconnected and cannot reconnect.
        await mac.call("devices.revoke", {"deviceId": phone_state["deviceId"]})
        await phone.wait_event(lambda e: e["event"] == "unpaired", 5)
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
