//! The agent engine: runs Codex turns one at a time, turns approvals into
//! decisions, and applies the control block (items, memory, questions).

use crate::app::{App, PendingApproval, RunKind, RunRequest};
use crate::codex::{CodexClient, CodexEvent};
use crate::control::{self, Control, MemoryOp};
use crate::model::*;
use crate::notify::{self, truncate};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const DEFAULT_RECHECK_MIN: i64 = 180;

pub async fn run_worker(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<RunRequest>) {
    while let Some(req) = rx.recv().await {
        let item_id = match &req.kind {
            RunKind::Item { item_id } => Some(item_id.clone()),
            _ => None,
        };
        let skip = req.generation < app.engine.generation.load(Ordering::SeqCst);
        if !skip {
            let title = run_title(&app, &req.kind);
            app.set_agent_state("working", Some(RunInfo { id: new_id("run"), title, started_at: now_ms() }));
            if let Err(e) = run_one(&app, &req.kind).await {
                tracing::warn!("run failed: {e:#}");
                let text = format!("这次没能完成：{e}");
                let item = item_id.as_deref();
                let _ = app.post("system", &text, item, None);
                notify::notify(&app, "failure", item, "执行失败", &text, json!({})).await;
                if let Some(i) = item {
                    reschedule_after_failure(&app, i);
                }
            }
            *app.engine.current.lock().unwrap() = None;
            app.set_agent_state("idle", None);
        }
        if let Some(i) = item_id {
            app.engine.queued_items.lock().unwrap().remove(&i);
        }
    }
}

fn run_title(app: &App, kind: &RunKind) -> String {
    match kind {
        RunKind::Chat { text } => truncate(text, 40),
        RunKind::Answer { .. } => "处理你的回答".into(),
        RunKind::Item { item_id } => app
            .store
            .get_item(item_id)
            .ok()
            .flatten()
            .map(|i| format!("检查：{}", i.title))
            .unwrap_or_else(|| "检查事项".into()),
        RunKind::Brief { .. } => "整理汇报".into(),
    }
}

fn reschedule_after_failure(app: &App, item_id: &str) {
    if let Ok(Some(mut i)) = app.store.get_item(item_id) {
        if matches!(i.status.as_str(), "active" | "scheduled") {
            i.next_check_at = Some(now_ms() + DEFAULT_RECHECK_MIN * 60_000);
            i.updated_at = now_ms();
            let _ = app.store.save_item(&i);
            app.emit_item(&i);
        }
    }
}

/// Starts (or reuses) the Codex app-server.
pub async fn ensure_codex(app: &App) -> Result<Arc<CodexClient>> {
    let mut slot = app.engine.codex.lock().await;
    if let Some(c) = slot.as_ref() {
        if c.is_alive() {
            return Ok(c.clone());
        }
    }
    let settings = app.store.settings()?;
    let path = crate::codex::resolve_codex(settings.codex_path.as_deref())
        .ok_or_else(|| anyhow!("未找到 codex，请在 Mac 上安装 Codex CLI 或在设置里指定路径"))?;
    let client = CodexClient::spawn(&path, crate::app::VERSION).await?;
    *slot = Some(client.clone());
    // A new process does not know threads from the previous one.
    let _ = app.store.kv_delete("chat_thread_loaded");
    Ok(client)
}

// ---------------------------------------------------------------------------
// Prompts

const BASE_INSTRUCTIONS: &str = r#"你是 LoopMe：住在用户 Mac 上、会主动跟进事情的个人 Agent。用户在手机上和你聊天，你在这台 Mac 上用 Codex 的能力做事。

原则：
- 回复简短、直接，先给结论。用用户使用的语言回复。
- 用户说“帮我盯着 X”时，建立一个事项，说明什么时候检查、什么情况算完成、你会做什么。
- 主动检查时，没有变化就不要打扰用户。
- 记忆以 Profile 为准；用户写的和锁定的记忆你不能改；“不要再记”列表里的内容不要再写回。
- 需要用户拍板时，提一个问题（decision），不要自己做不可逆的决定。

每次回复的最后，必须附上一个控制块（用户看不到），格式严格如下，JSON 放在一行或多行都可以：
<loopme>{"notify": false, "items": [], "memory": []}</loopme>

字段说明：
- notify：这次是否值得推送通知用户（有结果、需要用户介入时为 true；没有变化为 false）。
- items：对事项的操作，每个元素是以下之一：
  {"op":"create","title":"短标题","detail":"要做什么、在哪个仓库/分支","doneWhen":"完成条件","nextCheckInMinutes":60}
  {"op":"update","id":"itm_…","status":"active|needsYou|done|dropped","detail":"…","nextCheckInMinutes":120}
  {"op":"checked","id":"itm_…","changed":true,"nextCheckInMinutes":120}
  {"op":"suggest","title":"…","detail":"为什么值得跟进","source":"codex:<会话 id>"}
  时间也可以用 "nextCheckAt":"2026-11-06T09:00:00+08:00"（RFC 3339）。
- memory：{"op":"add","text":"关于用户的一条稳定事实或偏好"} 或 {"op":"update","id":"mem_…","text":"…"}。只记长期有用的事。
- decision（可选）：{"title":"要用户决定的问题","detail":"背景和你的建议","itemId":"itm_…"}，用户会在手机上点“同意”或“拒绝”。
"#;

fn fmt_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn context_block(app: &App) -> Result<String> {
    let profile = app.store.profile()?;
    let memory = app.store.list_memory()?;
    let items = app.store.list_items()?;
    let now = chrono::Local::now();
    let mut s = String::from("[LoopMe 上下文]\n");
    s.push_str(&format!("现在：{}\n", now.format("%Y-%m-%d %H:%M %A (%:z)")));
    let mut p = vec![];
    if !profile.name.is_empty() {
        p.push(format!("称呼：{}", profile.name));
    }
    if !profile.timezone.is_empty() {
        p.push(format!("时区：{}", profile.timezone));
    }
    if !profile.work_hours.is_empty() {
        p.push(format!("工作时间：{}", profile.work_hours));
    }
    if !profile.language.is_empty() {
        p.push(format!("语言：{}", profile.language));
    }
    if !profile.about.is_empty() {
        p.push(format!("关于：{}", profile.about));
    }
    if !p.is_empty() {
        s.push_str(&format!("Profile（最高优先级）：{}\n", p.join("；")));
    }
    let active: Vec<_> = memory.iter().filter(|m| m.status != "deleted").collect();
    if !active.is_empty() {
        s.push_str("记忆：\n");
        for m in active {
            let tag = match (m.status.as_str(), m.author.as_str()) {
                ("locked", _) => "（锁定）",
                (_, "user") => "（用户写的）",
                _ => "",
            };
            s.push_str(&format!("- [{}] {}{}\n", m.id, m.text, tag));
        }
    }
    let deleted: Vec<_> = memory.iter().filter(|m| m.status == "deleted").collect();
    if !deleted.is_empty() {
        s.push_str("不要再记：\n");
        for m in deleted {
            s.push_str(&format!("- {}\n", m.text));
        }
    }
    let open: Vec<_> = items.iter().filter(|i| !matches!(i.status.as_str(), "done" | "dropped")).collect();
    if !open.is_empty() {
        s.push_str("事项：\n");
        for i in open {
            let mut line = format!("- [{}] {} · {}", i.id, i.title, i.status);
            if let Some(t) = i.next_check_at {
                line.push_str(&format!(" · 下次检查 {}", fmt_time(t)));
            }
            if let Some(d) = &i.done_when {
                line.push_str(&format!(" · 完成条件：{d}"));
            }
            s.push_str(&line);
            s.push('\n');
        }
    }
    s.push_str("[/LoopMe 上下文]\n\n");
    Ok(s)
}

async fn recent_sessions(app: &App, client: &CodexClient) -> Result<String> {
    let mut excluded = app.store.own_codex_threads(60)?;
    for s in app.store.suggested_sources()? {
        if let Some(id) = s.strip_prefix("codex:") {
            excluded.push(id.to_string());
        }
    }
    excluded.truncate(100);
    let res = client
        .request(
            "thread/list",
            json!({
                "limit": 40,
                "sortKey": "updated_at",
                "sourceKinds": ["cli", "vscode", "exec"],
                "excludedThreadIds": excluded,
            }),
        )
        .await?;
    let cutoff = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
    let mut lines = vec![];
    for t in res.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
        let updated = t.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
        if updated < cutoff {
            continue;
        }
        let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| t.get("preview").and_then(Value::as_str))
            .unwrap_or("");
        let cwd = t.get("cwd").and_then(Value::as_str).unwrap_or("");
        let branch = t.pointer("/gitInfo/branch").and_then(Value::as_str).unwrap_or("");
        let status = t.pointer("/status/type").and_then(Value::as_str).unwrap_or("");
        lines.push(format!(
            "- [codex:{id}] {} · {cwd}{} · {} · 更新于 {}",
            truncate(&name.replace('\n', " "), 100),
            if branch.is_empty() { String::new() } else { format!(" @{branch}") },
            status,
            fmt_time(updated * 1000)
        ));
    }
    Ok(lines.join("\n"))
}

fn sandbox_for(settings: &Settings) -> (&'static str, Value) {
    match settings.permission_level.as_str() {
        "readOnly" => ("read-only", json!({"type": "readOnly", "networkAccess": false})),
        "full" => ("danger-full-access", json!({"type": "dangerFullAccess"})),
        _ => (
            "workspace-write",
            json!({
                "type": "workspaceWrite",
                "writableRoots": settings.workspace_roots.iter().map(|r| crate::codex::expand_home(r).display().to_string()).collect::<Vec<_>>(),
                "networkAccess": true,
                "excludeTmpdirEnvVar": false,
                "excludeSlashTmp": false
            }),
        ),
    }
}

fn default_cwd(settings: &Settings) -> String {
    settings
        .workspace_roots
        .first()
        .map(|r| crate::codex::expand_home(r).display().to_string())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".into())
}

// ---------------------------------------------------------------------------
// Running a turn

struct TurnOutput {
    /// Final agent messages (display text) with the message id when shown.
    controls: Vec<Control>,
    /// Display text of the last agent message.
    last_text: String,
    /// Message ids created in the chat stream.
    message_ids: Vec<String>,
}

async fn start_thread(app: &App, client: &CodexClient, settings: &Settings, ephemeral: bool, purpose: &str) -> Result<String> {
    let (sandbox, _) = sandbox_for(settings);
    let mut params = json!({
        "cwd": default_cwd(settings),
        "approvalPolicy": "on-request",
        "sandbox": sandbox,
        "developerInstructions": BASE_INSTRUCTIONS,
        "ephemeral": ephemeral,
    });
    if let Some(m) = settings.model.as_ref().filter(|m| !m.is_empty()) {
        params["model"] = json!(m);
    }
    let res = client.request("thread/start", params).await?;
    let id = res
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("thread/start 没有返回 thread id"))?
        .to_string();
    app.store.record_codex_thread(&id, purpose)?;
    Ok(id)
}

/// The chat thread is kept for a day (or until memory is corrected), then rotated.
async fn chat_thread(app: &App, client: &CodexClient, settings: &Settings) -> Result<String> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let saved: Value = app.store.kv_get("chat_thread")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    if let (Some(id), Some(date)) = (saved.get("id").and_then(Value::as_str), saved.get("date").and_then(Value::as_str)) {
        if date == today {
            if app.store.kv_get("chat_thread_loaded")?.as_deref() == Some(id) {
                return Ok(id.to_string());
            }
            if client.request("thread/resume", json!({"threadId": id})).await.is_ok() {
                app.store.kv_set("chat_thread_loaded", id)?;
                return Ok(id.to_string());
            }
        }
    }
    let id = start_thread(app, client, settings, false, "chat").await?;
    app.store.set_json("chat_thread", &json!({"id": id, "date": today}))?;
    app.store.kv_set("chat_thread_loaded", &id)?;
    Ok(id)
}

async fn run_one(app: &Arc<App>, kind: &RunKind) -> Result<()> {
    let settings = app.store.settings()?;
    if app.status.lock().unwrap().codex.logged_in != Some(true) {
        app.refresh_codex_status().await;
        let c = app.status.lock().unwrap().codex.clone();
        if !c.found {
            bail!("这台 Mac 上没有找到 Codex，请先安装 Codex CLI");
        }
        if c.logged_in == Some(false) {
            bail!("Codex 还没有登录，请在 Mac 菜单栏的 LoopMe 面板里点“登录”");
        }
    }
    let client = ensure_codex(app).await?;
    let started = Instant::now();

    let (thread_id, prompt, visible, item_id) = match kind {
        RunKind::Chat { text } => {
            let t = chat_thread(app, &client, &settings).await?;
            (t, format!("{}用户：{}", context_block(app)?, text), true, None)
        }
        RunKind::Answer { decision_id, approved } => {
            let d = app.store.get_decision(decision_id)?.ok_or_else(|| anyhow!("decision not found"))?;
            let t = chat_thread(app, &client, &settings).await?;
            let p = format!(
                "{}用户在手机上回答了你的问题「{}」：{}。请据此继续。",
                context_block(app)?,
                d.title,
                if *approved { "同意" } else { "拒绝" }
            );
            (t, p, true, d.item_id)
        }
        RunKind::Item { item_id } => {
            let mut item = app.store.get_item(item_id)?.ok_or_else(|| anyhow!("item not found"))?;
            item.status = "active".into();
            item.next_check_at = None;
            item.updated_at = now_ms();
            app.store.save_item(&item)?;
            app.emit_item(&item);
            let t = start_thread(app, &client, &settings, true, "item").await?;
            let p = format!(
                "{}现在是一次主动检查（用户不在场）。事项 [{}]「{}」\n说明：{}\n完成条件：{}\n已检查 {} 次，其中连续 {} 次无变化。\n\
                 请实际去检查（读代码仓库、跑只读命令等），然后：用一两句话说明现在的状态；在控制块里用 checked 操作记录这次检查（changed 表示和上次相比有没有变化），并给出下次检查时间；如果已经满足完成条件，用 update 把状态设为 done；只有出现结果或需要用户介入时 notify 才为 true。",
                context_block(app)?,
                item.id,
                item.title,
                item.detail,
                item.done_when.clone().unwrap_or_else(|| "（未定）".into()),
                item.checks,
                item.unchanged_checks
            );
            (t, p, false, Some(item.id))
        }
        RunKind::Brief { manual } => {
            let sessions = if settings.session_analysis {
                recent_sessions(app, &client).await.unwrap_or_else(|e| format!("（读取 Codex 会话失败：{e}）"))
            } else {
                "（用户关闭了会话分析）".into()
            };
            let t = start_thread(app, &client, &settings, true, "brief").await?;
            let p = format!(
                "{}现在是{}。请做两件事：\n1. 用几句话汇总进行中的事项（有什么进展、什么需要用户）。\n\
                 2. 下面是这台 Mac 最近 7 天的 Codex 会话。挑出最多 3 件最值得跟进的开发任务（例如停在测试失败、分支几天没动、留下 TODO 的会话），用 suggest 操作提出，source 写成会话前的 codex:<id>。没有值得跟进的就不要提。\n{}\n\n\
                 {}",
                context_block(app)?,
                if *manual { "用户要求的一次会话分析" } else { "早间汇报" },
                if sessions.is_empty() { "（最近 7 天没有会话）".to_string() } else { sessions },
                if *manual { "请一定给出回复。" } else { "如果既没有进展也没有建议，notify 为 false，回复一句话即可。" }
            );
            (t, p, *manual, None)
        }
    };

    let out = run_turn(app, &client, &thread_id, &prompt, &settings, visible, item_id.as_deref()).await?;
    let mut merged = Control::default();
    for c in out.controls {
        merged.notify |= c.notify;
        merged.items.extend(c.items);
        merged.memory.extend(c.memory);
        if c.decision.is_some() {
            merged.decision = c.decision;
        }
    }

    // Wake-ups only show up in the chat when there is something to say.
    let mut shown_id = out.message_ids.last().cloned();
    let should_show = visible || merged.notify || merged.decision.is_some() || merged.items.iter().any(|o| o.op == "suggest");
    if !visible && should_show && !out.last_text.is_empty() {
        let m = app.post("agent", &out.last_text, item_id.as_deref(), None)?;
        shown_id = Some(m.id);
    }

    apply_control(app, &merged, item_id.as_deref(), shown_id.as_deref()).await?;

    if let RunKind::Item { item_id } = kind {
        finish_item_check(app, item_id, &merged, &settings).await?;
    }

    let long_chat = visible && started.elapsed() > Duration::from_secs(60);
    if (merged.notify || long_chat) && should_show {
        let push_kind = match kind {
            RunKind::Brief { .. } => "brief",
            _ => "result",
        };
        notify::notify(app, push_kind, item_id.as_deref(), "LoopMe", &out.last_text, json!({})).await;
    }
    Ok(())
}

async fn run_turn(
    app: &Arc<App>,
    client: &Arc<CodexClient>,
    thread_id: &str,
    prompt: &str,
    settings: &Settings,
    visible: bool,
    item_id: Option<&str>,
) -> Result<TurnOutput> {
    let (_, sandbox_policy) = sandbox_for(settings);
    let res = client
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt, "text_elements": []}],
                "approvalPolicy": "on-request",
                "sandboxPolicy": sandbox_policy,
            }),
        )
        .await?;
    let turn_id = res
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("turn/start 没有返回 turn id"))?
        .to_string();
    *app.engine.current.lock().unwrap() = Some((client.clone(), thread_id.to_string(), turn_id.clone()));

    let mut out = TurnOutput { controls: vec![], last_text: String::new(), message_ids: vec![] };
    // itemId -> (message id, raw text, last emit)
    let mut streams: HashMap<String, (Option<String>, String, Instant)> = HashMap::new();
    let mut last_error: Option<String> = None;
    let mut events = client.events.lock().await;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(6 * 3600), events.recv())
            .await
            .map_err(|_| anyhow!("Codex 长时间没有响应"))?;
        let Some(ev) = ev else { bail!("Codex 连接已断开") };
        match ev {
            CodexEvent::Exited => bail!("Codex 进程退出了"),
            CodexEvent::Request { id, method, params } => {
                handle_server_request(app, client, id, &method, &params, item_id).await;
            }
            CodexEvent::Notification { method, params } => {
                if params.get("turnId").and_then(Value::as_str).is_some_and(|t| t != turn_id) {
                    continue;
                }
                match method.as_str() {
                    "item/agentMessage/delta" if visible => {
                        let iid = params.get("itemId").and_then(Value::as_str).unwrap_or("").to_string();
                        let delta = params.get("delta").and_then(Value::as_str).unwrap_or("");
                        let e = streams.entry(iid).or_insert((None, String::new(), Instant::now() - Duration::from_secs(1)));
                        e.1.push_str(delta);
                        let shown = control::streaming_display(&e.1).to_string();
                        if e.0.is_none() && !shown.trim().is_empty() {
                            let m = app.store.insert_message("agent", &shown, item_id, None, true)?;
                            app.emit_message(&m);
                            out.message_ids.push(m.id.clone());
                            e.0 = Some(m.id);
                            e.2 = Instant::now();
                        } else if let Some(mid) = &e.0 {
                            if e.2.elapsed() > Duration::from_millis(120) {
                                app.emit_seq("messageDelta", app.store.current_seq()?, json!({"id": mid, "text": shown}));
                                e.2 = Instant::now();
                            }
                        }
                    }
                    "item/completed" => {
                        let item = params.get("item").cloned().unwrap_or(Value::Null);
                        match item.get("type").and_then(Value::as_str) {
                            Some("agentMessage") => {
                                let raw = item.get("text").and_then(Value::as_str).unwrap_or("");
                                let (display, control) = control::split(raw);
                                if let Some(c) = control {
                                    out.controls.push(c);
                                }
                                if !display.is_empty() {
                                    out.last_text = display.clone();
                                }
                                if visible {
                                    let iid = item.get("id").and_then(Value::as_str).unwrap_or("");
                                    let mid = streams.get(iid).and_then(|s| s.0.clone());
                                    match (mid, display.is_empty()) {
                                        (Some(mid), _) => {
                                            let m = app.store.update_message(&mid, &display, false)?;
                                            app.emit_message(&m);
                                        }
                                        (None, false) => {
                                            let m = app.post("agent", &display, item_id, None)?;
                                            out.message_ids.push(m.id);
                                        }
                                        (None, true) => {}
                                    }
                                }
                            }
                            Some("commandExecution") if visible && settings.show_steps => {
                                if let Some(cmd) = item.get("command").and_then(Value::as_str) {
                                    let _ = app.post("system", &format!("运行了 `{}`", truncate(cmd, 120)), item_id, None);
                                }
                            }
                            _ => {}
                        }
                    }
                    "turn/completed" => {
                        if params.pointer("/turn/id").and_then(Value::as_str) != Some(turn_id.as_str()) {
                            continue;
                        }
                        // Close any message still marked streaming.
                        for (mid, raw, _) in streams.values() {
                            if let Some(mid) = mid {
                                if let Ok(Some(m)) = app.store.get_message(mid) {
                                    if m.streaming {
                                        let (d, _) = control::split(raw);
                                        if let Ok(m) = app.store.update_message(mid, &d, false) {
                                            app.emit_message(&m);
                                        }
                                    }
                                }
                            }
                        }
                        expire_turn_approvals(app, &turn_id);
                        let status = params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("completed");
                        match status {
                            "failed" => {
                                let msg = params
                                    .pointer("/turn/error/message")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .or(last_error)
                                    .unwrap_or_else(|| "未知错误".into());
                                bail!("{msg}");
                            }
                            "interrupted" => {
                                let _ = app.post("system", "已停止", item_id, None);
                                return Ok(out);
                            }
                            _ => return Ok(out),
                        }
                    }
                    "error" => {
                        let msg = params.pointer("/error/message").and_then(Value::as_str).unwrap_or("");
                        let retry = params.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
                        tracing::warn!("codex error (willRetry={retry}): {msg}");
                        if !retry && !msg.is_empty() {
                            last_error = Some(msg.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Approvals and decisions

async fn handle_server_request(app: &Arc<App>, client: &Arc<CodexClient>, id: Value, method: &str, params: &Value, item_id: Option<&str>) {
    let (title, detail) = match method {
        "item/commandExecution/requestApproval" => {
            let cmd = params
                .get("command")
                .and_then(|c| c.as_str().map(str::to_string).or_else(|| c.as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))))
                .unwrap_or_default();
            let mut d = format!("命令：{cmd}");
            if let Some(cwd) = params.get("cwd").and_then(Value::as_str) {
                d.push_str(&format!("\n目录：{cwd}"));
            }
            if let Some(r) = params.get("reason").and_then(Value::as_str) {
                d.push_str(&format!("\n原因：{r}"));
            }
            ("允许执行这条命令？".to_string(), d)
        }
        "item/fileChange/requestApproval" => {
            let mut d = String::from("Agent 想修改文件。");
            if let Some(r) = params.get("reason").and_then(Value::as_str) {
                d.push_str(&format!("\n原因：{r}"));
            }
            if let Some(g) = params.get("grantRoot").and_then(Value::as_str) {
                d.push_str(&format!("\n范围：{g}"));
            }
            ("允许修改文件？".to_string(), d)
        }
        other if other.ends_with("requestApproval") => ("允许这个操作？".to_string(), format!("{other}\n{}", truncate(&params.to_string(), 400))),
        other => {
            tracing::info!("declining unsupported codex request {other}");
            let _ = client.respond(id, json!({"decision": "decline"})).await;
            return;
        }
    };
    let settings = app.store.settings().unwrap_or_default();
    let d = Decision {
        id: new_id("dec"),
        item_id: item_id.map(Into::into),
        title: title.clone(),
        detail: detail.clone(),
        kind: "approval".into(),
        status: "pending".into(),
        created_at: now_ms(),
        expires_at: now_ms() + settings.limits.decision_timeout_hours * 3600 * 1000,
        answered_at: None,
    };
    if app.store.save_decision(&d).is_err() {
        let _ = client.respond(id, json!({"decision": "decline"})).await;
        return;
    }
    app.engine.approvals.lock().unwrap().insert(
        d.id.clone(),
        PendingApproval {
            client: client.clone(),
            request_id: id,
            turn_id: params.get("turnId").and_then(Value::as_str).map(Into::into),
        },
    );
    app.emit_decision(&d);
    let _ = app.post("system", &title, item_id, Some(&d.id));
    notify::notify(app, "decision", item_id, &title, &detail, json!({"decisionId": d.id})).await;
}

fn expire_turn_approvals(app: &App, turn_id: &str) {
    let ids: Vec<String> = app
        .engine
        .approvals
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p)| p.turn_id.as_deref() == Some(turn_id))
        .map(|(k, _)| k.clone())
        .collect();
    for id in ids {
        app.engine.approvals.lock().unwrap().remove(&id);
        if let Ok(Some(mut d)) = app.store.get_decision(&id) {
            if d.status == "pending" {
                d.status = "expired".into();
                d.answered_at = Some(now_ms());
                let _ = app.store.save_decision(&d);
                app.emit_decision(&d);
            }
        }
    }
}

/// Answers a decision. `approved = None` means it timed out (treated as rejected).
pub async fn answer_decision(app: &Arc<App>, decision_id: &str, approved: Option<bool>) -> Result<Decision> {
    let mut d = app.store.get_decision(decision_id)?.ok_or_else(|| anyhow!("找不到这个决定"))?;
    if d.status != "pending" {
        return Ok(d); // idempotent: already answered
    }
    d.status = match approved {
        Some(true) => "approved",
        Some(false) => "rejected",
        None => "expired",
    }
    .into();
    d.answered_at = Some(now_ms());
    let yes = approved == Some(true);
    match d.kind.as_str() {
        "approval" => {
            let pending = app.engine.approvals.lock().unwrap().remove(decision_id);
            match pending {
                Some(p) => {
                    p.client
                        .respond(p.request_id, json!({"decision": if yes { "accept" } else { "decline" }}))
                        .await?;
                }
                None => {
                    if approved.is_some() {
                        d.status = "expired".into();
                    }
                }
            }
        }
        "continue" => {
            if let Some(iid) = &d.item_id {
                if let Some(mut i) = app.store.get_item(iid)? {
                    if yes {
                        i.status = "scheduled".into();
                        i.unchanged_checks = 0;
                        i.next_check_at = Some(now_ms() + DEFAULT_RECHECK_MIN * 60_000);
                    } else {
                        i.status = "dropped".into();
                        i.next_check_at = None;
                    }
                    i.updated_at = now_ms();
                    app.store.save_item(&i)?;
                    app.emit_item(&i);
                }
            }
        }
        _ => {
            if let Some(a) = approved {
                app.queue(RunKind::Answer { decision_id: d.id.clone(), approved: a });
            }
        }
    }
    app.store.save_decision(&d)?;
    app.emit_decision(&d);
    Ok(d)
}

/// Interrupts the running turn and drops queued work.
pub async fn stop(app: &Arc<App>) -> Result<()> {
    app.engine.generation.fetch_add(1, Ordering::SeqCst);
    let current = app.engine.current.lock().unwrap().clone();
    if let Some((client, thread_id, turn_id)) = current {
        client.request("turn/interrupt", json!({"threadId": thread_id, "turnId": turn_id})).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Control block effects

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// True when `text` is close enough to a deleted entry that it must not come back.
pub fn blocked_by_tombstone(text: &str, memory: &[MemoryEntry]) -> bool {
    let n = normalize(text);
    if n.is_empty() {
        return true;
    }
    memory.iter().filter(|m| m.status == "deleted").any(|m| {
        let t = normalize(&m.text);
        !t.is_empty() && (t == n || (t.chars().count() >= 4 && (n.contains(&t) || t.contains(&n))))
    })
}

pub fn apply_memory_op(app: &App, op: &MemoryOp) -> Result<bool> {
    let memory = app.store.list_memory()?;
    let text = op.text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    match op.op.as_str() {
        "add" => {
            if blocked_by_tombstone(text, &memory) {
                return Ok(false);
            }
            let n = normalize(text);
            if memory.iter().any(|m| m.status != "deleted" && normalize(&m.text) == n) {
                return Ok(false);
            }
            let now = now_ms();
            app.store.save_memory(&MemoryEntry {
                id: new_id("mem"),
                text: text.into(),
                author: "agent".into(),
                status: "active".into(),
                created_at: now,
                updated_at: now,
            })?;
            Ok(true)
        }
        "update" => {
            let Some(id) = &op.id else { return Ok(false) };
            let Some(mut m) = app.store.get_memory(id)? else { return Ok(false) };
            if m.author != "agent" || m.status != "active" || blocked_by_tombstone(text, &memory) {
                return Ok(false);
            }
            m.text = text.into();
            m.updated_at = now_ms();
            app.store.save_memory(&m)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

const ITEM_STATUSES: &[&str] = &["scheduled", "active", "needsYou", "snoozed", "done", "dropped", "suggested"];

async fn apply_control(app: &Arc<App>, c: &Control, run_item: Option<&str>, message_id: Option<&str>) -> Result<()> {
    let now = now_ms();
    let mut first_card = true;
    for op in &c.items {
        match op.op.as_str() {
            "create" | "suggest" => {
                let Some(title) = op.title.as_ref().filter(|t| !t.trim().is_empty()) else { continue };
                let suggest = op.op == "suggest";
                if suggest {
                    let Some(src) = op.source.as_ref() else { continue };
                    if app.store.suggested_sources()?.contains(src) {
                        continue;
                    }
                }
                let item = Item {
                    id: new_id("itm"),
                    title: title.trim().into(),
                    detail: op.detail.clone().unwrap_or_default(),
                    status: if suggest { "suggested".into() } else { "scheduled".into() },
                    next_check_at: if suggest {
                        None
                    } else {
                        Some(control::next_check_ms(op, now).unwrap_or(now + 60 * 60_000))
                    },
                    done_when: op.done_when.clone(),
                    source: op.source.clone(),
                    checks: 0,
                    unchanged_checks: 0,
                    created_at: now,
                    updated_at: now,
                };
                app.store.save_item(&item)?;
                app.emit_item(&item);
                // The first new item hangs off the agent's reply; others get their own card.
                match (first_card, message_id) {
                    (true, Some(mid)) if run_item.is_none() => {
                        app.store.set_message_item(mid, &item.id)?;
                        if let Some(m) = app.store.get_message(mid)? {
                            let m = app.store.update_message(&m.id, &m.text, false)?;
                            app.emit_message(&m);
                        }
                    }
                    _ => {
                        app.post("system", &item.title, Some(&item.id), None)?;
                    }
                }
                first_card = false;
            }
            "update" => {
                let Some(id) = &op.id else { continue };
                let Some(mut i) = app.store.get_item(id)? else { continue };
                if let Some(s) = op.status.as_ref().filter(|s| ITEM_STATUSES.contains(&s.as_str())) {
                    i.status = s.clone();
                    if matches!(s.as_str(), "done" | "dropped") {
                        i.next_check_at = None;
                    }
                }
                if let Some(t) = &op.title {
                    i.title = t.clone();
                }
                if let Some(d) = &op.detail {
                    i.detail = d.clone();
                }
                if let Some(d) = &op.done_when {
                    i.done_when = Some(d.clone());
                }
                if let Some(t) = control::next_check_ms(op, now) {
                    if !matches!(i.status.as_str(), "done" | "dropped") {
                        i.next_check_at = Some(t);
                        if i.status == "needsYou" || i.status == "snoozed" {
                            // keep status; the next check still happens
                        }
                    }
                }
                i.updated_at = now;
                app.store.save_item(&i)?;
                app.emit_item(&i);
            }
            "checked" => {} // handled in finish_item_check
            _ => {}
        }
    }

    let mut memory_changed = false;
    for op in &c.memory {
        memory_changed |= apply_memory_op(app, op)?;
    }
    if memory_changed {
        app.emit_memory();
    }

    if let Some(q) = &c.decision {
        if !q.title.trim().is_empty() {
            let settings = app.store.settings()?;
            let item_id = q.item_id.clone().or(run_item.map(Into::into));
            let d = Decision {
                id: new_id("dec"),
                item_id: item_id.clone(),
                title: q.title.clone(),
                detail: q.detail.clone(),
                kind: "question".into(),
                status: "pending".into(),
                created_at: now,
                expires_at: now + settings.limits.decision_timeout_hours * 3600 * 1000,
                answered_at: None,
            };
            app.store.save_decision(&d)?;
            app.emit_decision(&d);
            if let Some(iid) = &item_id {
                if let Some(mut i) = app.store.get_item(iid)? {
                    if !matches!(i.status.as_str(), "done" | "dropped") {
                        i.status = "needsYou".into();
                        i.updated_at = now;
                        app.store.save_item(&i)?;
                        app.emit_item(&i);
                    }
                }
            }
            app.post("system", &d.title, item_id.as_deref(), Some(&d.id))?;
            notify::notify(app, "decision", item_id.as_deref(), &d.title, &d.detail, json!({"decisionId": d.id})).await;
        }
    }
    Ok(())
}

async fn finish_item_check(app: &Arc<App>, item_id: &str, c: &Control, settings: &Settings) -> Result<()> {
    let Some(mut i) = app.store.get_item(item_id)? else { return Ok(()) };
    let now = now_ms();
    let checked = c.items.iter().find(|o| o.op == "checked" && o.id.as_deref() == Some(item_id));
    i.checks += 1;
    let changed = checked.and_then(|o| o.changed).unwrap_or(false);
    i.unchanged_checks = if changed { 0 } else { i.unchanged_checks + 1 };
    if matches!(i.status.as_str(), "done" | "dropped") {
        i.next_check_at = None;
    } else {
        if i.next_check_at.is_none() {
            let next = checked
                .and_then(|o| control::next_check_ms(o, now))
                .unwrap_or(now + DEFAULT_RECHECK_MIN * 60_000);
            i.next_check_at = Some(next);
        }
        if i.status == "active" {
            i.status = "scheduled".into();
        }
        if i.unchanged_checks >= settings.limits.max_unchanged_checks && i.status != "needsYou" {
            i.status = "needsYou".into();
            i.next_check_at = None;
            let d = Decision {
                id: new_id("dec"),
                item_id: Some(i.id.clone()),
                title: format!("「{}」连续 {} 次没有变化，还要继续跟进吗？", i.title, i.unchanged_checks),
                detail: "同意：继续定时检查；拒绝：不再跟进。".into(),
                kind: "continue".into(),
                status: "pending".into(),
                created_at: now,
                expires_at: now + settings.limits.decision_timeout_hours * 3600 * 1000,
                answered_at: None,
            };
            app.store.save_decision(&d)?;
            app.emit_decision(&d);
            app.post("system", &d.title, Some(&i.id), Some(&d.id))?;
            notify::notify(app, "decision", Some(&i.id), &d.title, &d.detail, json!({"decisionId": d.id})).await;
        }
    }
    i.updated_at = now;
    app.store.save_item(&i)?;
    app.emit_item(&i);
    Ok(())
}

/// User actions on an item card.
pub fn item_action(app: &Arc<App>, item_id: &str, action: &str, minutes: Option<i64>) -> Result<Item> {
    let mut i = app.store.get_item(item_id)?.ok_or_else(|| anyhow!("找不到这个事项"))?;
    let now = now_ms();
    match action {
        "done" => {
            i.status = "done".into();
            i.next_check_at = None;
        }
        "drop" | "ignore" => {
            i.status = "dropped".into();
            i.next_check_at = None;
        }
        "snooze" => {
            i.status = "snoozed".into();
            i.next_check_at = Some(now + minutes.unwrap_or(180).max(1) * 60_000);
        }
        "follow" | "resume" => {
            i.status = "scheduled".into();
            i.unchanged_checks = 0;
            i.next_check_at = Some(now);
        }
        "checkNow" => {
            i.next_check_at = Some(now);
            if matches!(i.status.as_str(), "done" | "dropped" | "suggested" | "needsYou") {
                i.status = "scheduled".into();
            }
        }
        other => bail!("未知操作 {other}"),
    }
    i.updated_at = now;
    app.store.save_item(&i)?;
    app.emit_item(&i);
    if i.next_check_at == Some(now) && !app.store.settings()?.paused {
        app.queue(RunKind::Item { item_id: i.id.clone() });
    }
    Ok(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(text: &str, status: &str) -> MemoryEntry {
        MemoryEntry { id: "m".into(), text: text.into(), author: "agent".into(), status: status.into(), created_at: 0, updated_at: 0 }
    }

    #[test]
    fn tombstones_block_similar_text() {
        let m = vec![mem("用户喜欢早上 9 点开会", "deleted")];
        assert!(blocked_by_tombstone("用户喜欢早上9点开会。", &m));
        assert!(!blocked_by_tombstone("用户用 Rust 写后端", &m));
        assert!(!blocked_by_tombstone("用户喜欢早上 9 点开会", &[mem("用户喜欢早上 9 点开会", "active")]));
    }
}
