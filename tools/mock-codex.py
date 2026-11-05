#!/usr/bin/env python3
"""A tiny stand-in for `codex` used to test LoopMe without a Codex login.

Supports `--version`, `login status` and `app-server` (JSON lines on stdio).
Behaviour of `turn/start`, based on the user's text:
  * contains "盯"     -> replies and creates an item via the control block
  * contains "命令"   -> asks for a command approval first, then reports the answer
  * contains "问我"   -> asks the user a question (decision)
  * anything else     -> echoes the text
Point LoopMe at it with LOOPME_CODEX=/path/to/mock-codex.py.
"""
import json, sys, threading, time, uuid

if len(sys.argv) > 1 and sys.argv[1] == "--version":
    print("codex-cli 0.0.0-mock"); sys.exit(0)
if len(sys.argv) > 2 and sys.argv[1:3] == ["login", "status"]:
    print("Logged in (mock)"); sys.exit(0)
if len(sys.argv) < 2 or sys.argv[1] != "app-server":
    print("mock codex: unsupported", sys.argv[1:], file=sys.stderr); sys.exit(2)

lock = threading.Lock()
pending = {}          # our request id -> Event/result
next_id = [1000]

def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n"); sys.stdout.flush()

def ask(method, params):
    next_id[0] += 1
    rid = next_id[0]
    ev = threading.Event(); pending[rid] = [ev, None]
    send({"id": rid, "method": method, "params": params})
    ev.wait(3600)
    return pending.pop(rid)[1]

def stream(thread, turn, item, text):
    for i in range(0, len(text), 6):
        send({"method": "item/agentMessage/delta", "params": {"threadId": thread, "turnId": turn, "itemId": item, "delta": text[i:i+6]}})
        time.sleep(0.02)
    send({"method": "item/completed", "params": {"threadId": thread, "turnId": turn, "completedAtMs": int(time.time()*1000),
          "item": {"type": "agentMessage", "id": item, "text": text, "phase": None}}})

def run_turn(thread, turn, text):
    send({"method": "turn/started", "params": {"threadId": thread, "turn": {"id": turn, "status": "inProgress"}}})
    item = "msg_" + uuid.uuid4().hex[:8]
    user = text.split("用户：")[-1]
    if "主动检查" in text:
        reply = "CI 还在跑，没有变化。<loopme>{\"notify\":false,\"items\":[{\"op\":\"checked\",\"id\":\"%s\",\"changed\":false,\"nextCheckInMinutes\":90}]}</loopme>" % (
            text.split("事项 [")[1].split("]")[0] if "事项 [" in text else "")
    elif "命令" in user:
        res = ask("item/commandExecution/requestApproval", {"threadId": thread, "turnId": turn, "itemId": "cmd1",
                  "command": "git push origin HEAD", "cwd": "/tmp", "reason": "推送分支", "startedAtMs": 0})
        d = (res or {}).get("decision")
        reply = f"命令审批结果：{d}。<loopme>{{\"notify\":true}}</loopme>"
    elif "盯" in user:
        reply = ("好的，我会每小时看一次 PR 的 CI，变绿或者有评论时告诉你。"
                 "<loopme>{\"notify\":false,\"items\":[{\"op\":\"create\",\"title\":\"盯住 PR 的 CI\","
                 "\"detail\":\"仓库 loopme，PR #1\",\"doneWhen\":\"CI 全绿\",\"nextCheckInMinutes\":60}],"
                 "\"memory\":[{\"op\":\"add\",\"text\":\"用户在做 LoopMe 项目\"}]}</loopme>")
    elif "问我" in user:
        reply = "这个需要你拍板。<loopme>{\"notify\":true,\"decision\":{\"title\":\"现在合并 PR 吗？\",\"detail\":\"CI 已通过\"}}</loopme>"
    else:
        reply = "收到：" + user.strip()[:200] + "<loopme>{\"notify\":false}</loopme>"
    stream(thread, turn, item, reply)
    send({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": turn, "status": "completed", "error": None, "items": []}}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "method" not in msg and "id" in msg:   # response to our request
        p = pending.get(msg["id"])
        if p: p[1] = msg.get("result"); p[0].set()
        continue
    m, rid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if rid is None:
        continue
    if m == "initialize":
        send({"id": rid, "result": {"userAgent": "mock"}})
    elif m in ("thread/start", "thread/resume"):
        tid = params.get("threadId") or "thr_" + uuid.uuid4().hex[:8]
        send({"id": rid, "result": {"thread": {"id": tid}}})
    elif m == "thread/list":
        now = int(time.time())
        send({"id": rid, "result": {"nextCursor": None, "data": [
            {"id": "sess_a", "name": "修复登录测试", "preview": "pytest 失败", "cwd": "/Users/me/app", "updatedAt": now - 3600,
             "gitInfo": {"branch": "fix-login"}, "status": {"type": "notLoaded"}}]}})
    elif m == "turn/start":
        turn = "turn_" + uuid.uuid4().hex[:8]
        text = "".join(i.get("text", "") for i in params.get("input", []))
        send({"id": rid, "result": {"turn": {"id": turn, "status": "inProgress", "items": []}}})
        threading.Thread(target=run_turn, args=(params["threadId"], turn, text), daemon=True).start()
    elif m == "turn/interrupt":
        send({"id": rid, "result": {}})
    else:
        send({"id": rid, "error": {"code": -32601, "message": f"mock: {m} not supported"}})
