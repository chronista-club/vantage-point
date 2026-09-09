//! terminal ops — terminal demand / write / resize の Unison method handler（owner は `terminal_pump` と `lane/state`、doc 61）。
//!
//! demand hook（`terminal_demand_start` / `_stop`）は向きを信じず、`reconcile_terminal_pumps` 1 呼びで
//! 購読者数の level（`TopicRouter::demand_active`）に収束させる（doc 27 §4.1 → doc 53 R2）。
//! `reconcile_lane` は「動詞の末尾」として全群が呼ぶ収束点で、`AppState` の method（`state.reconcile_lane`）。
//! 受付は `unison_server::dispatch_repo_method`。

use super::state::AppState;

/// S2 (doc 27 §4.1) → doc 53 R2: terminal demand start / stop の共通ハンドラー。
///
/// daemon の TopicRouter demand hook が `repo/terminal/data/{lane}/out` の購読者 0↔1 を
/// 検知して撃つ。旧実装は start / stop がそれぞれ「張る」「畳む」を実行していたが、
/// reconcile 化で両者は**同じ操作**になった — demand の今（購読者数の level）は
/// `TopicRouter::demand_active` が答えるので、 hook の向きは信じず契機としてだけ使う
/// （start 側は「購読者が現れたので pump を揃えろ」、 stop 側は「消えたので畳め」、
/// どちらも reconcile 1 呼びで正しい方に収束する）。
pub(crate) async fn handle_terminal_demand(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand: lane 未指定".to_string());
    }
    if crate::repo::lane::parse_address(&lane).is_none() {
        return Err(format!("terminal_demand: lane パース失敗: {}", lane));
    }
    // client が「画面を持っていない」と名乗った場合は replay を必ず流す
    // （JS ready 後の catch-up。webview 準備前に届いた replay は捨てられている）。
    let force_replay = payload
        .get("replay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Lane 不在 / PtySlot 無でも受理（= Lane 起動後の再 demand 余地を残す）。
    let r = if force_replay {
        crate::repo::terminal_pump::reconcile_lane_pumps_forcing_replay(
            &state.lane_pool,
            &state.terminal_pumps,
            &state.topic_router,
            &lane,
        )
        .await
    } else {
        state.reconcile_terminal_pumps(&lane).await
    };
    Ok(serde_json::json!({
        "status": "reconciled", "lane": lane,
        "attached": r.attached, "removed": r.removed, "kept": r.kept,
    }))
}

// reconcile の収束点を `AppState` の method として持つ（doc 61、mako 2026-09-08）。
impl AppState {
    /// [`crate::repo::terminal_pump::reconcile_lane_pumps`] の AppState 版（呼び手の糖衣）。
    ///
    /// demand hook / 動詞の末尾（mode 切替・slot 追加・restart）/ boot 復元後 — pump に影響する
    /// あらゆる契機がこの 1 本を呼ぶ。旧 `respawn_terminal_pump` の `only` 引数（呼び手ごとの
    /// scope 判断）は廃止 — 「pid 一致は触らない」の照合が兄弟保護を構造で保証する。
    pub(crate) async fn reconcile_terminal_pumps(
        &self,
        lane: &str,
    ) -> crate::repo::terminal_pump::PumpReconcile {
        crate::repo::terminal_pump::reconcile_lane_pumps(
            &self.lane_pool,
            &self.terminal_pumps,
            &self.topic_router,
            lane,
        )
        .await
    }

    /// [`crate::repo::lane::reconcile::reconcile_lane`] の AppState 版（呼び手の糖衣）。
    ///
    /// **動詞の末尾はこれ 1 本**（doc 53 §12.4 / R3c）。registry に intent を書いた動詞は、
    /// 実体（PtySlot / chat engine / 代表値 / pump）を自分で動かさずにこれを呼ぶ。
    /// pump だけを合わせたい契機（demand hook）は [`Self::reconcile_terminal_pumps`] のまま —
    /// あちらは lane 全体の実体を触らない軽い経路。
    pub(crate) async fn reconcile_lane(
        &self,
        addr: &crate::repo::lane::LaneAddress,
    ) -> crate::repo::lane::reconcile::LaneReconcile {
        crate::repo::lane::reconcile::reconcile_lane(
            &self.lane_pool,
            &self.terminal_pumps,
            &self.topic_router,
            addr,
        )
        .await
    }
}

/// S3 (doc 27 §4.1, 経路 B): terminal 入力。
///
/// surface (vp-app) → daemon canvas channel (upstream request) → repo control → 本 dispatch。
/// `data` は base64 (出力 pump の encoding と対称、 任意バイトを JSON で運ぶため)。 decode して
/// 当該 slot の PtySlot に書き込む (`session` 省略 = root、doc 46 P5)。
pub(crate) async fn handle_terminal_write(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_write: lane 未指定".to_string());
    }
    let data_b64 = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("terminal_write: base64 decode 失敗: {}", e))?;
    vp_paths::term_trace("B:repo-recv", lane, &bytes);
    let Some(addr) = crate::repo::lane::parse_address(lane) else {
        return Err(format!("terminal_write: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .write_to_lane(
            &addr,
            super::unison_server::payload_session_key("terminal_write", &payload)?,
            &bytes,
        )
        .map_err(|e| format!("terminal_write 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// S3: terminal resize。 PtySlot (+ TermAttach grid) を cols×rows に同期する。
pub(crate) async fn handle_terminal_resize(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_resize: lane 未指定".to_string());
    }
    // bounds check: 0 / 極大値で PTY に不正 dims を渡さない (u64→u16 silent wrap も防ぐ)。
    // 旧 daemon "terminal" channel の resize 経路と同じ範囲 (1..=1000)。
    let cols = payload.get("cols").and_then(|v| v.as_u64()).unwrap_or(80);
    let rows = payload.get("rows").and_then(|v| v.as_u64()).unwrap_or(24);
    if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
        return Err(format!(
            "terminal_resize: 不正な dims (cols={cols} rows={rows})"
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);
    let Some(addr) = crate::repo::lane::parse_address(lane) else {
        return Err(format!("terminal_resize: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .resize_lane(
            &addr,
            super::unison_server::payload_session_key("terminal_resize", &payload)?,
            cols,
            rows,
        )
        .map_err(|e| format!("terminal_resize 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "cols": cols, "rows": rows}))
}

#[cfg(test)]
mod tests {
    use crate::repo::state::default_test_shell;

    // =========================================================================
    // S2 (doc 27 §4.1): demand-driven terminal pump の repo 側 e2e
    // =========================================================================

    /// 実 PtySlot を lane_pool に仕込み、 demand_start → pump 起動 → PTY 出力が
    /// per-lane terminal topic に届く → demand_stop → pump 除去、 を 1 本で検証する
    /// (daemon 側 demand hook の reverse-route 先 = repo dispatch の責務範囲)。
    #[tokio::test]
    async fn terminal_demand_start_routes_pty_output_then_stop() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string(); // "vp/main"

        // 実 PtySlot を attach (subscribe_output が Some を返す前提を作る)。
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // surface 相当: repo topic_router に per-lane terminal topic を購読
        // （= demand の level が立つ。 doc 53 R2: reconcile は購読者数を直読する）。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start（= reconcile 契機）→ pump 起動。
        let started = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(started["status"], "reconciled");
        assert_eq!(started["attached"], 1);
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump が登録される"
        );

        // shell プロンプト等の PTY 出力が terminal topic に流れてくる。
        let (rtopic, msg) = tokio::time::timeout(Duration::from_secs(5), srx.recv())
            .await
            .expect("PTY 出力が terminal topic に届かない (timeout)")
            .expect("topic channel closed");
        assert_eq!(rtopic, topic);
        assert!(matches!(msg, RepoMessage::LaneTerminalOutput { .. }));

        // 購読を畳んでから demand_stop（production の hook は 1→0 遷移の後に撃つ —
        // reconcile は edge の向きを信じず level を読むので、購読が残ったままの stop は
        // no-op になるのが正。 その裏は下の keeps_pumps テストが固定する）。
        state.topic_router.unsubscribe(sub_id).await;
        let stopped = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_stop");
        assert_eq!(stopped["status"], "reconciled");
        assert_eq!(stopped["removed"], 1);
        assert!(
            !state.terminal_pumps.read().await.contains_key(&lane),
            "pump が除去される"
        );

        // 嘘の edge への耐性: 購読が生きているのに stop が届いても pump は畳まれない
        // （level が真実源）。再購読して demand を立て直し、start で 1 本張ってから検証する。
        let (sub_id2, _srx2) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start 2");
        let lying_stop = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("lying stop");
        assert_eq!(lying_stop["removed"], 0, "購読が残る限り stop は畳まない");
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump は維持される"
        );
        state.topic_router.unsubscribe(sub_id2).await;
    }

    /// PtySlot を持たない Lane への demand_start は graceful（受理して何も張らない —
    /// Lane 起動後の再 demand 余地を残す）。
    #[tokio::test]
    async fn terminal_demand_start_without_lane_is_graceful() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 0);
        assert!(state.terminal_pumps.read().await.is_empty());
    }

    /// S3: terminal_write の base64 入力が実 PTY に届き (echo 出力で確認)、 terminal_resize が
    /// status ok を返す。 surface→Daemon→repo control の終端 = repo dispatch の責務範囲を検証する。
    #[tokio::test]
    async fn terminal_write_reaches_pty_and_resize_ok() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // PTY 出力を write 前に購読 (echo を取りこぼさない)。
        let mut out = state
            .lane_pool
            .read()
            .await
            .subscribe_output(&addr, None)
            .expect("subscribe_output");

        // シェル初期化待ち。
        tokio::time::sleep(Duration::from_millis(500)).await;

        // terminal_write: "echo VP_S3_OK" を PtySlot に届ける。 行確定の改行は OS 依存
        // (Unix shell は LF、 cmd.exe(ConPTY) は Enter=CR、 pty_slot の write test と同方針)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo VP_S3_OK\r"
        } else {
            b"echo VP_S3_OK\n"
        };
        let data = base64::engine::general_purpose::STANDARD.encode(echo_cmd);
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": lane, "data": data }),
        )
        .await
        .expect("terminal_write");
        assert_eq!(res["status"], "ok");

        // 出力に "VP_S3_OK" が現れる (= 入力が実 PTY に届いた)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), out.recv()).await {
                Ok(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    // ConPTY は DSR (`\x1b[6n` = カーソル位置問い合わせ) の応答を端末側から
                    // 受け取るまで描画を進めない。 本番は xterm.js が応答するが、 test では
                    // 端末役として terminal_write 経由で応答する (pty_slot の write test と同型)。
                    if text.contains("\u{1b}[6n") {
                        let dsr = base64::engine::general_purpose::STANDARD.encode(b"\x1b[1;1R");
                        let _ = dispatch_repo_method(
                            &state,
                            "terminal_write",
                            serde_json::json!({ "lane": lane, "data": dsr }),
                        )
                        .await;
                    }
                    if text.contains("VP_S3_OK") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(found, "terminal_write の入力が PTY 出力に反映されない");

        // terminal_resize: status ok + cols/rows echo。
        let res = dispatch_repo_method(
            &state,
            "terminal_resize",
            serde_json::json!({ "lane": lane, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("terminal_resize");
        assert_eq!(res["status"], "ok");
        assert_eq!(res["cols"], 120);
        assert_eq!(res["rows"], 40);
    }

    /// replay-on-attach を **sub lane** で end-to-end 検証する。
    ///
    /// 「vp-app 再起動 → 新 xterm が後発 subscribe → 前回画面が replay で戻る」を再現:
    /// PTY 出力を先に発生させ (= replay buffer に溜める)、 その **後で** topic を新規購読し、
    /// demand_start (= reconcile_terminal_pumps → attach_output) を撃つ。 購読が出力より後でも
    /// replay snapshot 経由でマーカーが届けば、 sub でも画面復元が効くことの証明になる。
    /// sub は main と別 topic key (`vp~sub~<name>`) に載るため、 main
    /// テストとは別に経路を固める価値がある。
    #[tokio::test]
    async fn replay_on_attach_restores_screen_for_sub_lane() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // sub lane (main とは別 topic key になる)
        let addr = LaneAddress::sub("vp", "feat-replay");
        let lane = addr.to_string();
        assert_eq!(lane, "vp/lane/feat-replay"); // canonical（名前空間つき）

        // 実 PtySlot を sub address で登録
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // マーカーを PTY に出力させる (echo)。 この出力は「過去」= replay buffer に溜まる。
        // 改行は OS 依存 (S3 test と同方針)。 ConPTY の DSR gating は reader task が起動時に
        // 自己応答する (pty_slot の Windows 分岐) ため、 ここでは端末役の応答は不要。
        let marker = "VP_SUB_REPLAY_MARKER";
        let echo_cmd: Vec<u8> = if cfg!(windows) {
            format!("echo {marker}\r").into_bytes()
        } else {
            format!("echo {marker}\n").into_bytes()
        };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, &echo_cmd)
                .expect("write to PTY");
        }

        // マーカーが replay buffer に確実に入るまで待つ (PtySlot が echo を読み終える猶予)。
        tokio::time::sleep(Duration::from_millis(800)).await;

        // ここで初めて topic を新規購読する (= 再起動後の新 xterm。 出力より後発)。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → reconcile_terminal_pumps → attach_output → replay 先頭配送。
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 1, "sub lane に pump が張れるはず");

        // 後発購読でも replay 経由でマーカーが届く (= 画面復元)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = String::new();
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((
                    got_topic,
                    RepoMessage::LaneTerminalOutput {
                        lane: l,
                        session: _,
                        data,
                    },
                ))) => {
                    assert_eq!(got_topic, topic);
                    assert_eq!(l, lane, "message は full lane address を載せる");
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                    if seen.contains(marker) {
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            found,
            "sub lane の後発 attach で replay されず画面が復元しない (seen={seen:?})"
        );
    }

    /// doc 50 §4.6 A6: demand_start が lane の**各 session** に pump を張り、共有 topic に
    /// session stamp 付きで route する（Design B）。2 slot を立て、両 session の出力が
    /// それぞれ正しい `session` field で届くことを検証する。
    #[tokio::test]
    async fn reconcile_covers_all_sessions() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-multi");
        let lane = addr.to_string();

        // root（None）と 2 枚目の session（Some(2)）を立てる。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn root");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
        }
        // root の実 key（fresh lane の既定）を控える。もう片方は 2。
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        assert_eq!(sessions.len(), 2, "root + session 2 の 2 枚");
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");

        // 各 slot に別マーカーを echo（過去出力 = replay buffer に溜まる）。
        let nl = if cfg!(windows) { "\r" } else { "\n" };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, format!("echo VP_ROOT_MARK{nl}").as_bytes())
                .expect("write root");
            pool.write_to_lane(&addr, Some(2), format!("echo VP_SESS2_MARK{nl}").as_bytes())
                .expect("write s2");
        }
        tokio::time::sleep(Duration::from_millis(800)).await;

        // 後発 subscribe → demand_start → 全 session に pump。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");

        // session 別に受信を畳み、両マーカーが正しい session field で届くまで待つ。
        let mut by_session: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, data, .. }))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    by_session
                        .entry(session)
                        .or_default()
                        .push_str(&String::from_utf8_lossy(&bytes));
                    let root_ok = by_session
                        .get(&root_key)
                        .is_some_and(|s| s.contains("VP_ROOT_MARK"));
                    let s2_ok = by_session
                        .get(&2)
                        .is_some_and(|s| s.contains("VP_SESS2_MARK"));
                    if root_ok && s2_ok {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_ROOT_MARK")),
            "root session の出力が root_key stamp で届く (got={by_session:?})"
        );
        assert!(
            by_session
                .get(&2)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "session 2 の出力が session=2 stamp で届く (got={by_session:?})"
        );
        // マーカーが session をまたいで混ざらない（振り分けの健全性）。
        assert!(
            !by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "root stream に session 2 のマーカーが混ざらない"
        );
    }

    /// reconcile が **差し替わった slot だけ**に replay を撃ち、兄弟 pane を乱さないこと
    /// （team-b 10 回目の regression の doc 53 R2 版）。
    ///
    /// pump の張り直しは client に `REPLAY_CLEAR_PREFIX` + 全 replay を送る。旧実装は呼び手が
    /// `only` 引数で範囲を判断していた（判断を間違えると**触っていない pane が clear されて
    /// scroll 位置も飛ぶ**）。reconcile では「pid 一致は触らない」の照合が同じ保護を構造で
    /// 与える — 呼び手は範囲を指定できない（引数が存在しない）。
    ///
    /// 観測は **client が見るもの**で行う: slot 差替 + reconcile 後に流れてくる出力の
    /// session stamp が、差し替わった session だけであること。
    /// **GUI が再起動したら replay を流し直す**（doc 53 §6.5.0 の最終段、2026-07-26）。
    ///
    /// GUI プロセスが入れ替わっても **slot は生きたまま**なので、pump の identity（slot pid）は
    /// 一致する。旧実装はそれを「変化なし」と判定して attach を skip し、**新しい GUI に過去の
    /// 画面が届かなかった**（console が黒いまま = 実機で観測）。
    ///
    /// pump が答えるべきは 2 つの別の問い:
    /// - **張り直すべきか**（server 側の生産）→ slot pid
    /// - **replay を流すべきか**（client が画面を持っているか）→ **購読の世代**
    ///
    /// 1 つの述語で兼ねていたのを分けた（[[one-predicate-three-properties]]）。
    ///
    /// 壊し方: attach 判定を `current.get(s) != Some(pid)`（pid だけ）に戻すと、②の
    /// 「再購読後に replay が届く」が落ちる。
    #[cfg(unix)]
    #[tokio::test]
    async fn reconnecting_client_gets_replay_even_when_slot_is_unchanged() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-reconnect");
        let lane = addr.to_string();
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));

        // slot を 1 本立てて、識別できる出力を出しておく（= replay の中身になる）。
        {
            let mut pool = state.lane_pool.write().await;
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn");
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
            pool.write_to_lane(&addr, None, b"echo VP_RECONNECT_MARK\n")
                .expect("write");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // ① 最初の client が購読 → pump が張られ replay が流れる。
        let (sub1, _rx1) = state.topic_router.subscribe(&topic).await;
        state.reconcile_terminal_pumps(&lane).await;
        let pid_before = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert!(pid_before.is_some(), "前提: pump が張られている");

        // ① の client に replay が届ききるのを待ってから捨てる（= 以降 live 出力は流れない）。
        // ⚠️ ここで待たないと ② の観測が **replay ではなく live 出力**を拾ってしまい、
        // pid だけの旧判定でも緑になる（テストが性質を守らない）。
        tokio::time::sleep(Duration::from_millis(800)).await;
        drop(_rx1);

        // ② client が入れ替わる（GUI 再起動）。**slot は触らない** = pid は変わらない。
        //    旧購読の掃除は QUIC idle timeout 待ちで遅れるので、ここでは外さない（実機と同じ形）。
        let (_sub2, mut rx2) = state.topic_router.subscribe(&topic).await;
        state.reconcile_terminal_pumps(&lane).await;

        // 新しい購読者に **過去の画面（replay）が届く**こと。
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), rx2.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { data, .. }))) => {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                        seen.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    if seen.contains("VP_RECONNECT_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        assert!(
            seen.contains("VP_RECONNECT_MARK"),
            "再購読した client に replay が届く（slot は同じでも購読者が変われば流し直す）: got={seen:?}"
        );

        // slot は張り替えていない（兄弟保護 = R2 の性質を壊していない）。
        let pid_after = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert_eq!(
            pid_after, pid_before,
            "slot は差し替えていない（pump の張り直しだけ）"
        );
        state.topic_router.unsubscribe(sub1).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_touches_only_the_swapped_slot_leaving_siblings_alone() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-scoped");
        let lane = addr.to_string();

        // root + 2 枚目。どちらも出力を持たせて replay buffer を非空にする。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, None, b"echo VP_A\n")
                .expect("w root");
            pool.write_to_lane(&addr, Some(2), b"echo VP_B\n")
                .expect("w s2");
        }
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 初回 demand で両方に pump を張る。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        // 初回 replay を吸い切る（ここまでは両 session に流れて正常）。
        //
        // ⚠️ 時間だけで待ってはいけない: login shell は rc 読み込みの分だけ起動が遅れ、root の
        // 初回出力が後段の観測窓へ漏れる。すると「触っていない root にも流れた」と誤検出して
        // 間欠的に落ちる（v0.60.0 の release ゲートで顕在化。product は正しく、測り方の race
        // だった）。まず **両 session の実出力を見る = readiness を待ち**、その後で無音まで
        // drain する。
        let mut warmed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let warm_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while warmed.len() < 2 && tokio::time::Instant::now() < warm_deadline {
            if let Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) =
                tokio::time::timeout(Duration::from_millis(500), srx.recv()).await
            {
                warmed.insert(session);
            }
        }
        assert_eq!(
            warmed.len(),
            2,
            "両 session の初回出力が揃ってから観測に入る (warmed={warmed:?})"
        );
        while tokio::time::timeout(Duration::from_millis(400), srx.recv())
            .await
            .is_ok()
        {}

        // 変化が無ければ reconcile は何もしない（= 契機が重なっても pane は無傷）。
        let idle = state.reconcile_terminal_pumps(&lane).await;
        assert_eq!(
            (idle.attached, idle.removed, idle.kept),
            (0, 0, 2),
            "無変化の reconcile は全 pump を keep する"
        );

        // session 2 の slot を差し替える（= restart / mode 切替で実際に起きること）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2b, rx2b) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2b");
            pool.insert_pty_slot(addr.clone(), Some(2), s2b, rx2b);
            pool.write_to_lane(&addr, Some(2), b"echo VP_B2\n")
                .expect("w s2b");
        }
        let swapped = state.reconcile_terminal_pumps(&lane).await;
        assert_eq!(
            (swapped.attached, swapped.kept),
            (1, 1),
            "pid 照合で差し替わった session 2 だけ attach、root は keep"
        );

        // 以後届く出力の session stamp を集める。root が混ざったら兄弟を乱している。
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(300), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) => {
                    seen.insert(session);
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen.contains(&2),
            "差し替わった session 2 には replay が流れる (seen={seen:?})"
        );
        assert!(
            !seen.contains(&root_key),
            "**触っていない root には何も流れない**（lane 全体を張り直すと clear + 全 replay が \
             飛んで scroll 位置が消える）(seen={seen:?})"
        );

        // 両方の pump が生きている（片方を差し替えても他方を落とさない）。
        let pumps = state.terminal_pumps.read().await;
        let lane_pumps = pumps.get(&lane).expect("lane の pump 集合");
        assert!(
            lane_pumps.contains_key(&2) && lane_pumps.contains_key(&root_key),
            "session ごとに 1 本ずつ残る (keys={:?})",
            lane_pumps.keys().collect::<Vec<_>>()
        );
    }

    /// doc 53 R2 受け入れ条件①（doc 50 §4.7「直さないと決めた 1 件」の根治）:
    /// **demand が立った後から現れた slot** にも、次の reconcile 契機で pump が張られる。
    ///
    /// 実機の形: daemon 再起動 → GUI が購読（demand edge）→ boot 復元が 800ms×N で遅れて
    /// slot を立てる → 旧実装ではその slot に pump が張られず**永久に沈黙**した。
    /// reconcile は「復元完了 = 動詞の末尾」（lane_spawn_actor / server boot）で呼ばれ、
    /// 現在の demand（level）× 現在の slot で収束する — edge の順序に依存しない。
    #[cfg(unix)]
    #[tokio::test]
    async fn late_restored_slot_gets_pump_on_next_reconcile() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lane::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-late");
        let lane = addr.to_string();

        // boot 途中の姿: root slot だけが立った時点で GUI が購読 → demand edge が先に立つ。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
        }
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(
            state
                .terminal_pumps
                .read()
                .await
                .get(&lane)
                .map(|m| m.len()),
            Some(1),
            "edge 時点では root の 1 本だけ"
        );

        // 復元の続き: session 2 の slot が後から立つ（edge はもう来ない）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, Some(2), b"echo VP_LATE_MARK\n")
                .expect("w s2");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 復元完了の契機（lane_spawn_actor / server boot 相当）→ 不足分だけ attach。
        let r = state.reconcile_terminal_pumps(&lane).await;
        assert_eq!(
            (r.attached, r.removed, r.kept),
            (1, 0, 1),
            "後から現れた slot だけ attach、既存 root pump は無傷"
        );

        // session 2 の出力（replay 込み）が届く = 沈黙しない。
        let mut seen2 = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), srx.recv()).await {
                Ok(Some((
                    _t,
                    RepoMessage::LaneTerminalOutput {
                        session: 2, data, ..
                    },
                ))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen2.push_str(&String::from_utf8_lossy(&bytes));
                    if seen2.contains("VP_LATE_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen2.contains("VP_LATE_MARK"),
            "後から復元された slot の出力が届く（旧実装は永久に沈黙）(seen={seen2:?})"
        );
    }

    /// PtySlot を持たない Lane への terminal_write は Err (lane 不在を上位に伝える)。
    #[tokio::test]
    async fn terminal_write_unknown_lane_errs() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use base64::Engine;

        let state = build_test_app_state(None).await;
        let data = base64::engine::general_purpose::STANDARD.encode(b"x");
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": "vp/main", "data": data }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への write は Err");
    }
}
