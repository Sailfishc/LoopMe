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
const MAX_BACKOFF_MIN: i64 = 120;

pub async fn run_worker(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<RunRequest>) {
    while let Some(req) = rx.recv().await {
        let item_id = match &req.kind {
            RunKind::Item { item_id } => Some(item_id.clone()),
            _ => None,
        };
        let skip = req.generation < app.engine.generation.load(Ordering::SeqCst);
        if !skip {
            let title = run_title(&app, &req.kind);
            let message_id = match &req.kind {
                RunKind::Takeover { message_id } => Some(message_id.clone()),
                _ => None,
            };
            app.set_agent_state("working", Some(RunInfo { id: new_id("run"), title, started_at: now_ms(), step: None, message_id: message_id.clone() }));
            if let Err(e) = run_one(&app, &req.kind).await {
                tracing::warn!("run failed: {e:#}");
                let text = format!("这次没能完成：{e}");
                let item = item_id.as_deref();
                if let Some(mid) = &message_id {
                    let _ = update_offer(&app, mid, |o| {
                        o.state = "closed".into();
                        o.note = Some(format!("没做完：{}", truncate(&e.to_string(), 120)));
                    });
                }
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
        RunKind::Chat { text, .. } => truncate(text, 40),
        RunKind::Answer { .. } => "处理你的回答".into(),
        RunKind::Item { item_id } => app
            .store
            .get_item(item_id)
            .ok()
            .flatten()
            .map(|i| format!("检查：{}", i.title))
            .unwrap_or_else(|| "检查事项".into()),
        RunKind::Brief { .. } => "整理今天的事".into(),
        RunKind::Session { session_id, .. } => app
            .store
            .get_session(session_id)
            .ok()
            .flatten()
            .map(|s| format!("看看：{}", s.title))
            .unwrap_or_else(|| "看看你的 Codex 会话".into()),
        RunKind::Takeover { message_id } => app
            .store
            .get_message(message_id)
            .ok()
            .flatten()
            .and_then(|m| m.offer)
            .map(|o| o.title)
            .unwrap_or_else(|| "接手任务".into()),
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
- 每条主动消息都要让用户能点：你能替用户做完的事，用 offers 里的 takeover 提出（用户点“让我来”后你会在独立分支上做，不会推送）；只需要知会的，用一句话说清楚，不要长篇汇报。
- 检查频率按等待的东西来定：等 CI 之类的外部结果约 5 分钟；等别人 review 约 30 分钟；其它事项约 3 小时。没有变化时 LoopMe 会自动拉长间隔。

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
- offers（可选）：可以点的卡片，每个元素是以下之一：
  {"kind":"takeover","title":"一句话说要做的事","detail":"现在的情况（一两句）","why":"你看到的依据","sessionId":"会话 id","cwd":"仓库路径","task":"接手后具体要做什么、怎么验证"}
  {"kind":"info","title":"一句话","detail":"补充（可选）"}
- attention（只在查看用户自己的 Codex 会话时用）：{"kind":"stuck|question|done|milestone|none","summary":"一句话判断","evidence":"一行证据","cause":"test|dependency|permission|unclear"}
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
    let disliked = app.store.disliked_messages(5)?;
    if !disliked.is_empty() {
        s.push_str("用户点了👎的消息（少发类似的）：\n");
        for t in disliked {
            s.push_str(&format!("- {}\n", truncate(&t.replace('\n', " "), 80)));
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
                // CLI, IDE, `codex exec`, and app-server clients such as the Codex desktop app.
                "sourceKinds": ["cli", "vscode", "exec", "appServer"],
                "excludedThreadIds": excluded,
            }),
        )
        .await?;
    // LoopMe's own runs are app-server threads too: drop any the server-side exclusion missed.
    let own: std::collections::HashSet<String> = app.store.own_codex_threads(2000)?.into_iter().collect();
    let data_dir = app.data_dir.display().to_string();
    let cutoff = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
    let mut lines = vec![];
    for t in res.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
        let updated = t.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
        if updated < cutoff {
            continue;
        }
        let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
        let cwd = t.get("cwd").and_then(Value::as_str).unwrap_or("");
        if own.contains(id) || (!cwd.is_empty() && std::path::Path::new(cwd).starts_with(&data_dir)) {
            continue;
        }
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| t.get("preview").and_then(Value::as_str))
            .unwrap_or("");
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

/// Where a run works and what it may write besides the workspace roots.
#[derive(Default, Clone)]
struct RunEnv {
    cwd: Option<String>,
    extra_roots: Vec<String>,
    read_only: bool,
}

fn sandbox_for(settings: &Settings, env: &RunEnv) -> (&'static str, Value) {
    if env.read_only && settings.permission_level != "full" {
        return ("read-only", json!({"type": "readOnly", "networkAccess": false}));
    }
    match settings.permission_level.as_str() {
        "readOnly" if env.extra_roots.is_empty() => ("read-only", json!({"type": "readOnly", "networkAccess": false})),
        "full" => ("danger-full-access", json!({"type": "dangerFullAccess"})),
        _ => (
            "workspace-write",
            json!({
                "type": "workspaceWrite",
                "writableRoots": settings
                    .workspace_roots
                    .iter()
                    .map(|r| crate::codex::expand_home(r).display().to_string())
                    .chain(env.extra_roots.iter().cloned())
                    .collect::<Vec<_>>(),
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

async fn start_thread(app: &App, client: &CodexClient, settings: &Settings, ephemeral: bool, purpose: &str, env: &RunEnv) -> Result<String> {
    let (sandbox, _) = sandbox_for(settings, env);
    let mut params = json!({
        "cwd": env.cwd.clone().unwrap_or_else(|| default_cwd(settings)),
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
    let id = start_thread(app, client, settings, false, "chat", &RunEnv::default()).await?;
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
    let started_ms = now_ms();

    if let RunKind::Takeover { message_id } = kind {
        return run_takeover(app, &client, &settings, message_id).await;
    }
    let mut env = RunEnv::default();
    let (thread_id, prompt, visible, item_id) = match kind {
        RunKind::Chat { text, reply_to } => {
            let t = chat_thread(app, &client, &settings).await?;
            let quoted = reply_to
                .as_deref()
                .and_then(|id| app.store.get_message(id).ok().flatten())
                .map(|m| format!("用户在回复你之前的这条消息：「{}」\n", truncate(&m.text, 600)))
                .unwrap_or_default();
            (t, format!("{}{}用户：{}", context_block(app)?, quoted, text), true, None)
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
            let t = start_thread(app, &client, &settings, true, "item", &env).await?;
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
            let t = start_thread(app, &client, &settings, true, "brief", &env).await?;
            let p = format!(
                "{}现在是{}（用户刚{}）。请这样做：\n\
                 1. 回复只写一句话：打招呼，并说今天有几件事，例如「早上好。昨晚有 1 件事我可以接手，1 件等你决定。」不要逐条汇报，细节放进卡片。\n\
                 2. 下面是这台 Mac 最近 7 天的 Codex 会话。你能直接做完的（例如停在测试失败、只差一步的修复），用 offers 的 takeover 提出，cwd 写会话目录，sessionId 写 codex: 后面的 id；只值得跟进的，用 suggest 操作提出，source 写成 codex:<id>。合计最多 3 件，没有就不提。\n{}\n\n\
                 {}",
                context_block(app)?,
                if *manual { "用户要求的一次会话分析" } else { "今天第一次见面" },
                if *manual { "点了分析" } else { "回到 Mac 或打开 App" },
                if sessions.is_empty() { "（最近 7 天没有会话）".to_string() } else { sessions },
                if *manual { "请一定给出回复。" } else { "如果既没有进展也没有建议，notify 为 false，回复一句话即可。" }
            );
            (t, p, *manual, None)
        }
        RunKind::Session { session_id, reason } => {
            let sess = app.store.get_session(session_id)?.ok_or_else(|| anyhow!("会话不存在"))?;
            env = RunEnv { cwd: Some(sess.cwd.clone()), read_only: true, ..Default::default() };
            let t = start_thread(app, &client, &settings, true, "signal", &env).await?;
            let minutes = sess.turn_started_at.map(|t| (now_ms() - t) / 60_000);
            let p = format!(
                "{}用户自己的一个 Codex 会话{}。你要替用户判断：这件事现在需不需要他知道。用户最在意的是少被打扰，但不能漏掉真正需要他的事。\n\
                 - 会话 id：{}\n- 项目：{}\n- 目录：{}\n- 这一轮跑了：{}\n- 最后一次用户输入：{}\n- Codex 最后的回复：{}\n- 会话记录文件：{}\n\n\
                 只读查看会话记录末尾和仓库（可以看 git status / git diff --stat / 测试输出），不要修改任何文件。然后在控制块里给出 attention：\n\
                 - stuck：Codex 停在一个它自己解决不了的问题上（测试一直失败、缺依赖、没权限、需求不清）。cause 填 test / dependency / permission / unclear。\n\
                 - question：Codex 在等用户回答一个问题或做一个选择。\n\
                 - done：用户交代的事做完了，而且有证据（测试通过、改动完成）。\n\
                 - milestone：长任务完成了一个有意义的阶段，但还没全部做完。\n\
                 - none：其它情况（正常推进、闲聊、没做完也没卡住、只是回答了问题）。拿不准时选 none。\n\
                 summary 是一句话的判断（不超过 40 个字，直接说结果或卡在哪，不要以“Codex”开头）。evidence 是一行证据（例如“12 个测试通过，改了 6 个文件”或“npm test 里 2 个失败：auth.spec.ts”）。\n\
                 如果是 stuck 而且你能独立做完（比如差一步的修复），再用 offers 提一个 takeover：title 一句话说你要做什么，detail 说现在的情况，why 写依据，cwd 写 {}，sessionId 写 {}，task 写清楚接手后要做什么、怎么验证。\n\
                 回复正文写一句话即可，用户不会在对话里看到它。",
                context_block(app)?,
                if reason == "stalled" { "已经 4 个多小时没有动静" } else { "刚刚停下来了" },
                sess.id,
                crate::attention::project(&sess),
                sess.cwd,
                minutes.map(|m| format!("{m} 分钟")).unwrap_or_else(|| "（不知道）".into()),
                sess.last_prompt.as_deref().map(|p| truncate(p, 400)).unwrap_or_else(|| "（无）".into()),
                sess.last_message.as_deref().map(|p| truncate(p, 1200)).unwrap_or_else(|| "（无）".into()),
                sess.transcript_path.as_deref().unwrap_or("（无）"),
                sess.cwd,
                sess.id,
            );
            (t, p, false, None)
        }
        RunKind::Takeover { .. } => unreachable!(),
    };

    let out = run_turn(app, &client, &thread_id, &prompt, &settings, visible, item_id.as_deref(), &env).await?;
    let merged = merge_controls(out.controls);

    // Wake-ups only show up in the chat when there is something to say.
    let mut shown_id = out.message_ids.last().cloned();
    // A look at a Codex session speaks through its card in the "现在" bar, not the chat.
    let session_look = matches!(kind, RunKind::Session { .. });
    let should_show = !session_look
        && (visible
        || merged.notify
        || merged.decision.is_some()
        || !merged.offers.is_empty()
        || merged.items.iter().any(|o| o.op == "suggest"));
    if !visible && should_show && !out.last_text.is_empty() {
        let m = app.post("agent", &out.last_text, item_id.as_deref(), None)?;
        shown_id = Some(m.id);
    }

    let offer_ids = apply_control(app, &merged, item_id.as_deref(), shown_id.as_deref()).await?;

    if let RunKind::Item { item_id } = kind {
        finish_item_check(app, item_id, &merged, &settings).await?;
    }

    if let RunKind::Session { session_id, .. } = kind {
        if let Some(mut s) = app.store.get_session(session_id)? {
            // The user picked it up again while we were looking: their own eyes are on it.
            let answered = s.turn_started_at.is_some_and(|t| t > started_ms);
            let op = merged.attention.clone().unwrap_or_default();
            match crate::attention::level_for(&op.kind) {
                Some(level) if !answered && !op.summary.trim().is_empty() => {
                    let att = Attention {
                        kind: op.kind.clone(),
                        level: level.into(),
                        summary: truncate(op.summary.trim(), 80),
                        evidence: op.evidence.clone().filter(|e| !e.trim().is_empty()).map(|e| truncate(e.trim(), 160)),
                        cause: op.cause.clone().filter(|c| !c.trim().is_empty()),
                        at: now_ms(),
                        message_id: offer_ids.last().cloned(),
                        snoozed_until: None,
                    };
                    s.offered_at = Some(now_ms());
                    crate::attention::raise(app, &mut s, att).await;
                }
                _ if !answered => crate::attention::clear(&mut s),
                _ => {}
            }
            app.store.save_session(&s)?;
            app.emit("session", json!(s));
        }
    }

    let long_chat = visible && started.elapsed() > Duration::from_secs(60);
    if (merged.notify || long_chat) && should_show && !session_look {
        let push_kind = match kind {
            RunKind::Brief { .. } => "brief",
            _ => "result",
        };
        notify::notify(app, push_kind, item_id.as_deref(), "LoopMe", &out.last_text, json!({})).await;
    }
    Ok(())
}

fn merge_controls(controls: Vec<Control>) -> Control {
    let mut merged = Control::default();
    for c in controls {
        merged.notify |= c.notify;
        merged.items.extend(c.items);
        merged.memory.extend(c.memory);
        merged.offers.extend(c.offers);
        if c.decision.is_some() {
            merged.decision = c.decision;
        }
        if c.attention.is_some() {
            merged.attention = c.attention;
        }
    }
    merged
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    app: &Arc<App>,
    client: &Arc<CodexClient>,
    thread_id: &str,
    prompt: &str,
    settings: &Settings,
    visible: bool,
    item_id: Option<&str>,
    env: &RunEnv,
) -> Result<TurnOutput> {
    let (_, sandbox_policy) = sandbox_for(settings, env);
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
                    "item/started" => {
                        let item = params.get("item").cloned().unwrap_or(Value::Null);
                        match item.get("type").and_then(Value::as_str) {
                            Some("commandExecution") => {
                                if let Some(cmd) = item.get("command").and_then(Value::as_str) {
                                    app.set_run_step(&format!("运行 {}", truncate(cmd, 60)));
                                }
                            }
                            Some("fileChange") => {
                                let path = item
                                    .pointer("/changes/0/path")
                                    .and_then(Value::as_str)
                                    .map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                                app.set_run_step(&match path {
                                    Some(p) if !p.is_empty() => format!("修改 {p}"),
                                    _ => "修改文件".into(),
                                });
                            }
                            _ => {}
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
        "sessionApproval" => {
            crate::sessions::answer_session_approval(app, decision_id, approved);
        }
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

/// Applies a control block. Returns the ids of the offer messages it posted.
async fn apply_control(app: &Arc<App>, c: &Control, run_item: Option<&str>, message_id: Option<&str>) -> Result<Vec<String>> {
    let now = now_ms();
    let mut offer_ids = Vec::new();
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
                    interval_min: None,
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

    for op in &c.offers {
        if op.title.trim().is_empty() {
            continue;
        }
        let takeover = op.kind == "takeover" && op.cwd.as_deref().is_some_and(|d| std::path::Path::new(d).is_dir());
        let offer = Offer {
            kind: if takeover { "takeover".into() } else { "info".into() },
            label: if takeover { "可以接手".into() } else { "告诉你一声".into() },
            title: op.title.trim().into(),
            detail: op.detail.trim().into(),
            why: op.why.clone().filter(|w| !w.trim().is_empty()),
            session_id: op.session_id.clone().map(|s| s.trim_start_matches("codex:").to_string()),
            cwd: op.cwd.clone(),
            task: op.task.clone(),
            actions: if takeover {
                vec![
                    OfferAction::new("take", "让我来", "primary"),
                    OfferAction::new("mine", "我自己来", "secondary"),
                    OfferAction::new("ignore", "别管它", "plain"),
                ]
            } else {
                vec![]
            },
            state: "open".into(),
            ..Default::default()
        };
        let m = app.store.insert_message("agent", &offer.detail, run_item, None, false)?;
        let m = app.store.set_message_extras(&m.id, None, None, Some(&offer))?;
        app.emit_message(&m);
        offer_ids.push(m.id.clone());
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
    Ok(offer_ids)
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
            let asked = checked
                .and_then(|o| control::next_check_ms(o, now))
                .map(|t| ((t - now) / 60_000).max(1))
                .unwrap_or(DEFAULT_RECHECK_MIN);
            let interval = next_interval(asked, i.interval_min, changed);
            i.interval_min = Some(interval);
            i.next_check_at = Some(now + interval * 60_000);
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

// ---------------------------------------------------------------------------
// Offers and 让我来

/// Changes a message's offer card and tells the clients.
pub fn update_offer(app: &App, message_id: &str, f: impl FnOnce(&mut Offer)) -> Result<Message> {
    let m = app.store.get_message(message_id)?.ok_or_else(|| anyhow!("找不到这条消息"))?;
    let mut offer = m.offer.clone().ok_or_else(|| anyhow!("这条消息不能操作"))?;
    f(&mut offer);
    let m = app.store.set_message_extras(&m.id, m.reply_to.as_deref(), m.feedback.as_deref(), Some(&offer))?;
    app.emit_message(&m);
    Ok(m)
}

fn mute_session(app: &App, session_id: Option<&str>) -> Result<()> {
    if let Some(id) = session_id {
        if let Some(mut s) = app.store.get_session(id)? {
            s.muted = true;
            s.eval_at = None;
            app.store.save_session(&s)?;
            app.emit("session", json!(s));
        }
    }
    Ok(())
}

/// A tap on an offer card's button.
pub async fn offer_act(app: &Arc<App>, message_id: &str, action: &str) -> Result<Message> {
    let m = app.store.get_message(message_id)?.ok_or_else(|| anyhow!("找不到这条消息"))?;
    let offer = m.offer.clone().ok_or_else(|| anyhow!("这条消息不能操作"))?;
    if !offer.actions.iter().any(|a| a.id == action) && action != "stop" {
        bail!("这张卡片没有这个操作");
    }
    match action {
        "take" => {
            if offer.state != "open" {
                return Ok(m);
            }
            let m = update_offer(app, message_id, |o| {
                o.state = "working".into();
                o.chosen = Some("take".into());
                o.note = Some("你选了：让我来".into());
            })?;
            app.queue(RunKind::Takeover { message_id: message_id.into() });
            Ok(m)
        }
        "mine" | "ignore" => {
            mute_session(app, offer.session_id.as_deref())?;
            update_offer(app, message_id, |o| {
                o.state = "closed".into();
                o.chosen = Some(action.into());
                o.note = Some(if action == "mine" { "你选了：我自己来 · 不再提醒这件事".into() } else { "你选了：别管它".into() });
            })
        }
        "stop" => {
            if offer.state == "working" {
                stop(app).await?;
            }
            Ok(m)
        }
        "push" => {
            let (Some(wt), Some(branch)) = (offer.worktree.clone(), offer.branch.clone()) else { bail!("没有可推送的分支") };
            let b = branch.clone();
            tokio::task::spawn_blocking(move || crate::takeover::push(&wt, &b)).await??;
            update_offer(app, message_id, |o| {
                o.state = "closed".into();
                o.chosen = Some("push".into());
                o.note = Some(format!("已推送到 origin/{branch}"));
            })
        }
        "undo" => {
            let (Some(repo), Some(wt), Some(branch)) = (offer.cwd.clone(), offer.worktree.clone(), offer.branch.clone()) else {
                bail!("没有可撤销的改动")
            };
            tokio::task::spawn_blocking(move || crate::takeover::discard(&repo, &wt, &branch)).await??;
            update_offer(app, message_id, |o| {
                o.state = "closed".into();
                o.chosen = Some("undo".into());
                o.note = Some("已撤销 · 分支已删除，你的工作区没动过".into());
            })
        }
        other => bail!("未知操作 {other}"),
    }
}

/// The agent's changes behind a result card (看改动).
pub async fn offer_diff(app: &App, message_id: &str) -> Result<String> {
    let m = app.store.get_message(message_id)?.ok_or_else(|| anyhow!("找不到这条消息"))?;
    let o = m.offer.ok_or_else(|| anyhow!("这条消息没有改动"))?;
    let (Some(wt), Some(base)) = (o.worktree, o.base) else { bail!("这条消息没有改动") };
    tokio::task::spawn_blocking(move || crate::takeover::diff(&wt, &base, 200_000)).await?
}

async fn run_takeover(app: &Arc<App>, client: &Arc<CodexClient>, settings: &Settings, message_id: &str) -> Result<()> {
    let m = app.store.get_message(message_id)?.ok_or_else(|| anyhow!("找不到这条消息"))?;
    let offer = m.offer.clone().ok_or_else(|| anyhow!("这条消息不能接手"))?;
    let cwd = offer.cwd.clone().ok_or_else(|| anyhow!("不知道在哪个仓库里做"))?;
    app.set_run_step("准备独立分支");
    let (data_dir, title) = (app.data_dir.clone(), offer.title.clone());
    let ws = tokio::task::spawn_blocking(move || crate::takeover::prepare(&cwd, &data_dir, &title)).await??;
    update_offer(app, message_id, |o| {
        o.cwd = Some(ws.repo.clone());
        o.branch = Some(ws.branch.clone());
        o.base = Some(ws.base.clone());
        o.worktree = Some(ws.worktree.clone());
    })?;
    let env = RunEnv { cwd: Some(ws.worktree.clone()), extra_roots: vec![ws.worktree.clone(), ws.git_dir.clone()], read_only: false };
    let thread = start_thread(app, client, settings, true, "takeover", &env).await?;
    let session = offer.session_id.as_deref().and_then(|id| app.store.get_session(id).ok().flatten());
    let prompt = format!(
        "{}用户在手机上点了「让我来」，请你把这件事做完。\n\
         任务：{}\n情况：{}\n{}{}{}\n\
         你在独立的 git worktree {} 里工作（分支 {}，从用户的 {} 分出来{}），改动不会影响用户自己的工作区。\n\
         要求：只在这个目录里改；改完运行相关测试；用 git commit 提交；不要 push，不要切换分支。\n\
         最后用两三句话说明改了什么、测试结果怎样。控制块里 notify 为 true。",
        context_block(app)?,
        offer.title,
        offer.detail,
        offer.task.as_deref().map(|t| format!("具体要求：{t}\n")).unwrap_or_default(),
        offer.why.as_deref().map(|t| format!("依据：{t}\n")).unwrap_or_default(),
        session
            .as_ref()
            .and_then(|s| s.transcript_path.as_deref())
            .map(|t| format!("原会话记录：{t}\n"))
            .unwrap_or_default(),
        ws.worktree,
        ws.branch,
        ws.base_branch,
        if ws.carried_changes { "，已带上会话里未提交的改动" } else { "" },
    );
    let generation = app.engine.generation.load(Ordering::SeqCst);
    let out = run_turn(app, client, &thread, &prompt, settings, false, None, &env).await;
    let interrupted = app.engine.generation.load(Ordering::SeqCst) != generation;
    let out = match out {
        Ok(o) if !interrupted => o,
        other => {
            let (repo, wt, branch) = (ws.repo.clone(), ws.worktree.clone(), ws.branch.clone());
            let _ = tokio::task::spawn_blocking(move || crate::takeover::discard(&repo, &wt, &branch)).await;
            update_offer(app, message_id, |o| {
                o.state = "closed".into();
                o.note = Some("已停止 · 改动已丢弃".into());
            })?;
            return other.map(|_| ());
        }
    };
    let merged = merge_controls(out.controls);
    apply_control(app, &Control { offers: vec![], ..merged }, None, None).await?;
    app.set_run_step("整理改动");
    let (wt, base, title) = (ws.worktree.clone(), ws.base.clone(), offer.title.clone());
    let stat = tokio::task::spawn_blocking(move || crate::takeover::finish(&wt, &base, &title)).await??;
    update_offer(app, message_id, |o| {
        o.state = "done".into();
        o.note = Some("你选了：让我来 · 已完成".into());
    })?;
    let changed = !stat.trim().is_empty();
    let summary = if out.last_text.is_empty() { "做完了。".to_string() } else { out.last_text.clone() };
    let result = Offer {
        kind: "result".into(),
        label: "做完了".into(),
        title: offer.title.clone(),
        detail: if changed { summary.clone() } else { format!("{summary}\n（没有产生改动）") },
        why: changed.then(|| format!("分支 {}，还没推送\n{}", ws.branch, stat)),
        session_id: offer.session_id.clone(),
        cwd: Some(ws.repo.clone()),
        branch: Some(ws.branch.clone()),
        base: Some(ws.base.clone()),
        worktree: Some(ws.worktree.clone()),
        actions: if changed {
            vec![
                OfferAction::new("diff", "看改动", "secondary"),
                OfferAction::new("push", "推送", "primary"),
                OfferAction::new("undo", "撤销", "plain"),
            ]
        } else {
            vec![OfferAction::new("undo", "清理分支", "plain")]
        },
        state: "open".into(),
        ..Default::default()
    };
    let rm = app.store.insert_message("agent", &result.detail, None, None, false)?;
    let rm = app.store.set_message_extras(&rm.id, None, None, Some(&result))?;
    app.emit_message(&rm);
    notify::notify(app, "result", None, &format!("做完了：{}", offer.title), &summary, json!({})).await;
    Ok(())
}

/// Check interval in minutes: what the agent asked for, doubled while nothing
/// changes, capped at 2 hours (or the agent's own interval when that is longer).
pub fn next_interval(asked: i64, previous: Option<i64>, changed: bool) -> i64 {
    let asked = asked.max(1);
    if changed {
        return asked;
    }
    let cap = asked.max(MAX_BACKOFF_MIN);
    previous.map(|p| (p * 2).clamp(asked, cap)).unwrap_or(asked)
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
    fn backoff_doubles_and_caps() {
        assert_eq!(next_interval(5, None, false), 5);
        assert_eq!(next_interval(5, Some(5), false), 10);
        assert_eq!(next_interval(5, Some(80), false), 120);
        assert_eq!(next_interval(5, Some(120), false), 120);
        assert_eq!(next_interval(5, Some(120), true), 5);
        assert_eq!(next_interval(180, Some(180), false), 180);
    }

    #[test]
    fn tombstones_block_similar_text() {
        let m = vec![mem("用户喜欢早上 9 点开会", "deleted")];
        assert!(blocked_by_tombstone("用户喜欢早上9点开会。", &m));
        assert!(!blocked_by_tombstone("用户用 Rust 写后端", &m));
        assert!(!blocked_by_tombstone("用户喜欢早上 9 点开会", &[mem("用户喜欢早上 9 点开会", "active")]));
    }
}
