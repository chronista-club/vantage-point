//! 能動の取得（actions 層）— engine ごとに 1 回問い合わせて [`EngineUsage`] を返す。
//!
//! [`UsageProbe`] は trait にしてある。daemon は [`CliProbe`]（本物の CLI / file を触る）を、
//! test は偽物を注入する — **test が本物の `claude` / `codex` / `agy` / `grok` を起動しない**ため
//! （spec の副作用ガード 6）。`mise run test` はオフライン・決定論的に通る。
//!
//! どの経路も **turn を起こさない**（= 枠を 1 トークンも使わない）:
//!
//! | engine | 経路 | 1 回のコスト |
//! |---|---|---|
//! | claude | `~/.claude.json` を読むだけ（**spawn しない**。mtime が同じなら読まない） | file 1 枚 |
//! | codex | `codex app-server` → `initialize` → `account/rateLimits/read` → kill | 数秒 |
//! | agy | `agy -p "/usage" --output-format json`（turn を起こさない — 実測 `num_turns` 0・token 0・0 秒） | 一瞬 |
//! | grok | `grok agent stdio` → ACP `initialize` → `_x.ai/billing` → kill | MCP server まで立つので重い |
//!
//! いつ・どの engine を問い合わせるか（grok は手動の時だけ、等）は [`super::service`] の方針が決める。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::model::{EngineUsage, UsageEngine};
use super::parse;

/// engine 1 つ分を問い合わせる口。
#[async_trait]
pub trait UsageProbe: Send + Sync {
    async fn probe(&self, engine: UsageEngine) -> EngineUsage;
}

/// 何も起動しない probe（test 用の `DaemonState`、usage を使わない構成）。
pub struct InertProbe;

#[async_trait]
impl UsageProbe for InertProbe {
    async fn probe(&self, engine: UsageEngine) -> EngineUsage {
        EngineUsage::failed(engine, "usage の取得は無効です")
    }
}

/// codex / agy の応答待ちの上限。
const QUICK_TIMEOUT: Duration = Duration::from_secs(25);
/// grok は MCP server の起動を待つことがあるので長め。
const GROK_TIMEOUT: Duration = Duration::from_secs(45);

/// 本物の CLI / file に当たる probe（daemon 本番用）。
#[derive(Default)]
pub struct CliProbe {
    /// `~/.claude.json` の (mtime, 前回の結果)。mtime が同じなら読み直さない。
    claude_cache: std::sync::Mutex<Option<(SystemTime, EngineUsage)>>,
}

impl CliProbe {
    pub fn new() -> Self {
        Self::default()
    }

    async fn claude(&self) -> EngineUsage {
        let path = claude_config_path();
        let mtime = match tokio::fs::metadata(&path).await.and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                return EngineUsage::failed(
                    UsageEngine::Claude,
                    format!(
                        "{} がありません（Claude Code 未ログイン）",
                        display_path(&path)
                    ),
                );
            }
        };
        if let Some((seen, usage)) = self.claude_cache.lock().expect("claude cache").as_ref()
            && *seen == mtime
        {
            return usage.clone();
        }
        let usage = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(config) => parse::claude_from_config(&config),
                Err(e) => EngineUsage::failed(
                    UsageEngine::Claude,
                    format!("{} を JSON として読めません: {e}", display_path(&path)),
                ),
            },
            Err(e) => EngineUsage::failed(
                UsageEngine::Claude,
                format!("{} を読めません: {e}", display_path(&path)),
            ),
        };
        *self.claude_cache.lock().expect("claude cache") = Some((mtime, usage.clone()));
        usage
    }

    async fn agy(&self) -> EngineUsage {
        let program = crate::conversation::acp_host::AcpEngine::Agy.cli_path();
        let mut cmd = base_command(&program);
        cmd.args(["-p", "/usage", "--output-format", "json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return EngineUsage::failed(UsageEngine::Agy, spawn_error("agy", &e)),
        };
        // timeout で wait を捨てた時も、agy が起こした子まで process group ごと落とす。
        let _group = GroupKill(child.id());
        let output = match tokio::time::timeout(QUICK_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return EngineUsage::failed(UsageEngine::Agy, format!("agy の待機に失敗: {e}"));
            }
            Err(_) => {
                return EngineUsage::failed(UsageEngine::Agy, "agy /usage が時間内に返りません");
            }
        };
        match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(v) => parse::agy_from_usage(&v),
            Err(_) => EngineUsage::failed(
                UsageEngine::Agy,
                format!("agy /usage の出力を読めません（exit {}）", output.status),
            ),
        }
    }

    async fn codex(&self) -> EngineUsage {
        let program = crate::lane::codex_session::codex_cli_path();
        let mut cmd = base_command(&program);
        cmd.arg("app-server");
        let steps = [
            RpcStep::Request {
                id: 0,
                line: serde_json::json!({
                    "id": 0,
                    "method": "initialize",
                    "params": {
                        "capabilities": { "experimentalApi": true },
                        "clientInfo": {
                            "name": "vantage_point_usage",
                            "title": "Vantage Point usage meter",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }
                }),
            },
            RpcStep::Notify(serde_json::json!({ "method": "initialized", "params": {} })),
            RpcStep::Request {
                id: 1,
                line: serde_json::json!({
                    "id": 1,
                    "method": "account/rateLimits/read",
                    "params": { "excludeResetCreditDetails": true }
                }),
            },
        ];
        match stdio_rpc(cmd, &steps, QUICK_TIMEOUT).await {
            Ok(result) => parse::codex_from_rate_limits_read(&result),
            Err(e) => EngineUsage::failed(UsageEngine::Codex, format!("codex: {e}")),
        }
    }

    async fn grok(&self) -> EngineUsage {
        let program = crate::conversation::acp_host::AcpEngine::Grok.cli_path();
        let mut cmd = base_command(&program);
        // session を作らないので `--always-approve` は要らない（tool を 1 つも走らせない）。
        cmd.args(["agent", "stdio"]);
        let steps = [
            RpcStep::Request {
                id: 0,
                line: serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 0,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": 1,
                        "clientCapabilities": {
                            "fs": { "readTextFile": false, "writeTextFile": false },
                            "terminal": false
                        },
                        "clientInfo": {
                            "name": "vantage-point-usage",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }
                }),
            },
            // 拡張 method は先頭の `_` が要る（`x.ai/billing` は Method not found、実測）。
            RpcStep::Request {
                id: 1,
                line: serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "_x.ai/billing", "params": {}
                }),
            },
        ];
        match stdio_rpc(cmd, &steps, GROK_TIMEOUT).await {
            Ok(result) => parse::grok_from_billing(&result),
            Err(e) => EngineUsage::failed(UsageEngine::Grok, format!("grok: {e}")),
        }
    }
}

#[async_trait]
impl UsageProbe for CliProbe {
    async fn probe(&self, engine: UsageEngine) -> EngineUsage {
        match engine {
            UsageEngine::Claude => self.claude().await,
            UsageEngine::Codex => self.codex().await,
            UsageEngine::Agy => self.agy().await,
            UsageEngine::Grok => self.grok().await,
        }
    }
}

/// Claude Code の設定 file。`CLAUDE_CONFIG_DIR` があれば その下（CC と同じ解決）。
fn claude_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join(".claude.json");
    }
    dirs::home_dir().unwrap_or_default().join(".claude.json")
}

/// probe 用の子プロセスの共通設定。
///
/// - cwd = temp dir: repo の中で起動すると project 単位の設定（MCP server / hook）まで拾うため
/// - 継承 env の剥がし: (1) daemon 自体が Claude Code の中から起動された時に継承する
///   `CLAUDECODE` / `CLAUDE_CODE_*` 等（host と同じ `inherited_agent_host_env_to_strip`）と、
///   (2) VP の identity env（[`IDENTITY_ENV`]）。daemon が lane の shell から起動されていると
///   これを継承していて、probe の子（とその hook）が「自分はその lane の engine だ」と名乗って
///   `vp now` / wire を撃ちうる
/// - kill_on_drop: timeout で future を捨てたら子も道連れにする
/// - unix は自前の process group で起こし、終わる時に [`GroupKill`] でグループごと落とす: grok は
///   MCP server を孫として立ち上げるので、親だけ kill すると孫が孤児で残る。Windows は group を
///   作らないので直接の子しか落とせない（Windows の整備は Windows 上で、CLAUDE.md の方針）
fn base_command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.current_dir(std::env::temp_dir())
        .env("PATH", crate::spawn_env::augmented_spawn_path())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    for key in crate::spawn_env::inherited_agent_host_env_to_strip() {
        cmd.env_remove(key);
    }
    for key in IDENTITY_ENV {
        cmd.env_remove(key);
    }
    cmd
}

/// VP が engine に焼く identity env（`conversation/codex_host.rs` 等の spawn と同じ 3 つ）。
/// probe の子は lane の engine ではないので持たせない。
const IDENTITY_ENV: [&str; 3] = ["VP_REPO", "VP_LANE", "VP_SESSION_KEY"];

/// エラー文に出す path。home は `~` にする（error は wire と state zone の file に乗るので、
/// user 名を含む絶対 path を残さない）。
fn display_path(path: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(PathBuf::from)) {
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

/// 相手（codex / grok）が返したエラー文から、メールアドレスらしき語を伏せる。
/// エラー文は wire と state zone の file に乗るので、念のため（ログイン状態のエラーは
/// account を名指しすることがある）。
fn redact_emails(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            if word.contains('@') && word.contains('.') {
                "<redacted>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// drop で子の process group ごと SIGKILL する（正常終了・timeout・future の破棄のどれでも）。
/// group を作っていない子（[`base_command`] を通らない test 等）でも、自分の pid の group が
/// 無ければ `killpg` が ESRCH で何もしないだけ。
struct GroupKill(Option<u32>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: killpg は signal を送るだけで、メモリ安全性に関わる前提を持たない。
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
        // Windows は process group を作っていない（直接の子は kill_on_drop が落とす）。
        #[cfg(not(unix))]
        let _ = self.0;
    }
}

fn spawn_error(name: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("{name} が見つかりません（未インストール）")
    } else {
        format!("{name} を起動できません: {e}")
    }
}

/// stdio JSON-RPC の 1 手。
enum RpcStep {
    /// 送って、同じ id の応答を待つ。最後の Request の result が [`stdio_rpc`] の戻り値。
    Request { id: i64, line: Value },
    /// 送るだけ（通知）。
    Notify(Value),
}

/// 子プロセスと stdio の JSON-RPC を順に交わし、最後の request の `result` を返す。
///
/// 相手から来る通知や、自分宛てでない応答は読み捨てる。`error` 応答・stdout の close・
/// timeout はすべて Err。終わったら子を kill する（usage を読む以外のことはさせない）。
async fn stdio_rpc(
    mut cmd: tokio::process::Command,
    steps: &[RpcStep],
    timeout: Duration,
) -> Result<Value, String> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // エラー文に出すので home は `~` に（grok の既定は `~/.grok/bin/grok` のような絶対 path）。
    let program = display_path(std::path::Path::new(cmd.as_std().get_program()));
    let mut child = cmd.spawn().map_err(|e| spawn_error(&program, &e))?;
    let group = GroupKill(child.id());
    let mut stdin = child.stdin.take().ok_or("stdin が取れません")?;
    let stdout = child.stdout.take().ok_or("stdout が取れません")?;
    let mut lines = BufReader::new(stdout).lines();

    let exchange = async {
        let mut last = Value::Null;
        for step in steps {
            let (line, want) = match step {
                RpcStep::Request { id, line } => (line, Some(*id)),
                RpcStep::Notify(line) => (line, None),
            };
            let mut text = line.to_string();
            text.push('\n');
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|e| format!("送信に失敗: {e}"))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("送信に失敗: {e}"))?;
            let Some(want) = want else {
                continue;
            };
            last = loop {
                let Some(raw) = lines
                    .next_line()
                    .await
                    .map_err(|e| format!("受信に失敗: {e}"))?
                else {
                    return Err("応答の前に終了しました（未ログインの可能性）".to_string());
                };
                let Ok(msg) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                if msg["id"].as_i64() != Some(want) || msg.get("method").is_some() {
                    continue;
                }
                if let Some(err) = msg.get("error") {
                    let text = err["message"].as_str().unwrap_or("不明なエラー");
                    return Err(redact_emails(text));
                }
                break msg["result"].clone();
            };
        }
        Ok(last)
    };
    let result = match tokio::time::timeout(timeout, exchange).await {
        Ok(r) => r,
        Err(_) => Err("時間内に応答がありません".to_string()),
    };
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本物の CLI を起動する smoke（**既定では走らない**）。手で確かめる時だけ:
    /// `VP_USAGE_REAL_PROBE=1 cargo test -p vantage-point --lib usage::probe -- --ignored --nocapture`
    ///
    /// grok は MCP server まで立ち上がるので `VP_USAGE_REAL_PROBE=grok` を足した時だけ。
    /// claude は file を読むだけ（spawn しない）。
    #[tokio::test]
    #[ignore = "本物の CLI を起動する（手動の確認用）"]
    async fn real_cli_probe_smoke() {
        let Ok(mode) = std::env::var("VP_USAGE_REAL_PROBE") else {
            return;
        };
        let probe = CliProbe::new();
        let mut engines = vec![UsageEngine::Claude, UsageEngine::Agy, UsageEngine::Codex];
        if mode.contains("grok") {
            engines.push(UsageEngine::Grok);
        }
        for engine in engines {
            let usage = probe.probe(engine).await;
            println!("{}", serde_json::to_string_pretty(&usage).unwrap());
        }
    }

    /// stdio JSON-RPC の往復を、本物の CLI ではなく shell の偽サーバーで確かめる。
    /// 通知と他 id の応答を読み捨て、目当ての id の result を返すこと。
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_rpc_skips_notifications_and_returns_last_result() {
        let script = r#"
            read init
            echo '{"method":"remoteControl/status/changed","params":{}}'
            echo '{"id":0,"result":{"ok":true}}'
            read notify
            read req
            echo '{"id":7,"result":{"wrong":true}}'
            echo '{"id":1,"result":{"rateLimits":{"usedPercent":5}}}'
            sleep 5
        "#;
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", script]).kill_on_drop(true);
        let steps = [
            RpcStep::Request {
                id: 0,
                line: serde_json::json!({"id":0,"method":"initialize"}),
            },
            RpcStep::Notify(serde_json::json!({"method":"initialized"})),
            RpcStep::Request {
                id: 1,
                line: serde_json::json!({"id":1,"method":"x"}),
            },
        ];
        let result = stdio_rpc(cmd, &steps, Duration::from_secs(5))
            .await
            .expect("result");
        assert_eq!(result["rateLimits"]["usedPercent"], 5);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_rpc_surfaces_error_and_early_exit() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            r#"read l; echo '{"id":0,"error":{"code":-32601,"message":"Method not found"}}'"#,
        ]);
        let steps = [RpcStep::Request {
            id: 0,
            line: serde_json::json!({"id":0}),
        }];
        let err = stdio_rpc(cmd, &steps, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err, "Method not found");

        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "read l; exit 0"]);
        let err = stdio_rpc(cmd, &steps, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("終了"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_rpc_times_out_and_kills_the_child() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "read l; sleep 30"]);
        let steps = [RpcStep::Request {
            id: 0,
            line: serde_json::json!({"id":0}),
        }];
        let started = std::time::Instant::now();
        let err = stdio_rpc(cmd, &steps, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.contains("時間内"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill して待たない"
        );
    }

    /// grok は MCP server を孫として立ち上げる。終わったら孫まで残さないこと（process group ごと kill）。
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_rpc_kills_grandchildren_too() {
        let mut cmd = base_command("/bin/sh");
        cmd.args([
            "-c",
            r#"sleep 30 & read l; echo "{\"id\":0,\"result\":{\"grandchild\":$!}}"; wait"#,
        ]);
        let steps = [RpcStep::Request {
            id: 0,
            line: serde_json::json!({"id":0}),
        }];
        let result = stdio_rpc(cmd, &steps, Duration::from_secs(5))
            .await
            .expect("result");
        let pid = result["grandchild"].as_i64().expect("pid") as i32;
        let mut alive = true;
        for _ in 0..40 {
            // SAFETY: signal 0 は存在確認だけ（何も送らない）。
            alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "孫（pid {pid}）が残っている");
    }

    #[test]
    fn error_text_hides_home_and_email() {
        let home = dirs::home_dir().expect("home");
        assert_eq!(display_path(&home.join(".claude.json")), "~/.claude.json");
        assert_eq!(
            display_path(std::path::Path::new("/etc/x.json")),
            "/etc/x.json"
        );
        assert_eq!(
            redact_emails("Not logged in as someone@example.com (run login)"),
            "Not logged in as <redacted> (run login)"
        );
        assert_eq!(redact_emails("Method not found"), "Method not found");
    }

    /// identity env（`VP_LANE` 等）を probe の子に渡さない。
    #[test]
    fn probe_children_do_not_inherit_lane_identity() {
        let cmd = base_command("/bin/sh");
        let removed: Vec<String> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().to_string())
            .collect();
        for key in IDENTITY_ENV {
            assert!(
                removed.iter().any(|k| k == key),
                "{key} を剥がしていない: {removed:?}"
            );
        }
    }

    #[test]
    fn missing_binary_is_reported_as_not_installed() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(spawn_error("grok", &e).contains("未インストール"));
    }
}
