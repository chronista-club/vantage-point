//! ClaudeHost — headless claude を lane 単位で常駐駆動する（gui engine host）
//!
//! doc 32 §3。`claude -p --input-format stream-json --output-format stream-json` を
//! piped stdio で spawn し、stdout を [`ClaudeTranslator`] に通して [`ConversationEvent`] を
//! `broadcast::Sender` に流す。tui（PtySlot + TUI）とは別系統の「headless engine」。
//!
//! PtySlot ⇄ terminal_pump と同型: host が producer（broadcast tx を持つ）、
//! `conversation_pump` が consumer（TopicRouter へ route）。engine プロセスは会話を保持するため
//! lane が gui の間は常駐する（demand-driven ではない）。
//!
//! ## in-flight tail（replay の冪等性を「生成中」まで広げる）
//!
//! claude は message を **完了時にしか** transcript(jsonl) へ flush しない。 一方 conversation topic は
//! 非 retained（`TopicRouter::route` はその時点の subscriber にだけ配る）。 よって assistant が
//! 生成中に WS/QUIC が瞬断すると、 demand 再発火で走る transcript replay は「生成中 message の
//! 直前まで」しか復元できず、 瞬断前に届いていた delta は永久に失われる。 GUI は `ReplayStart` で
//! items を reset しているため、 復帰後の `MessageChunk` が **文の途中から新しい assistant バブル**
//! を立ててしまう（旧 #699 の既知ギャップ）。
//!
//! そこで host は「まだ disk に無い増分」= **in-flight tail** を保持する。 transcript commit 境界
//! （`assistant` / `user` 行、[`super::claude_translate::Ingested::commits_transcript`]）で tail を捨て、
//! それ以降の [`ConversationEvent::MessageChunk`] / [`ConversationEvent::ThoughtChunk`] だけを積む。
//! replay 時に transcript の後ろへ tail を継げば、 生成の真っ最中に着地しても現在状態が厳密に
//! 再現される（= 冪等性が turn 境界に縛られなくなる）。
//!
//! ⚠️ tail に [`ConversationEvent::ToolCall`] を積んではならない。 translator は ToolCall を
//! `content_block_stop` で発火するが、 その block の commit（snapshot 行）は **1 行前に来ている**
//! ため、 ToolCall は既に transcript 側に存在する（`translate` の module doc / golden test 参照）。
//!
//! tail と transcript の間には「読んだ直後に commit が挟まる」窓がある。 [`Self::in_flight`] は
//! commit 世代 [`InFlight::seq`] を添えて返すので、 呼び手は transcript 読み後に seq を検算して
//! 世代が動いていたら読み直せる（[`crate::repo::unison_server`] の replay handler）。
//!
//! ## control protocol client（doc 35 PR1、HITL 面の質問レール）
//!
//! spawn に `--permission-prompt-tool stdio` を足すと、engine は逆方向の `can_use_tool`
//! control_request を **stdout**（ConversationEvent と同じ stream）に流す。stdout pump は translator の
//! **手前**でこれを検出し、`AskUserQuestion` を [`ConversationEvent::Question`] へ写して broadcast する。
//! GUI の回答は [`ClaudeHost::respond_permission`] が `control_response` として stdin へ書き戻す
//! （既存 stdin Mutex を submit と共有 = 混線しない、doc §2.3）。制御面を translator に流さないのは
//! ConversationEvent 語彙（会話の言葉）を engine 制御で汚さないため（責務分離、doc §2.4）。
//!
//! data / calculations / actions:
//! - calculations: stdin メッセージ JSON 生成（[`user_message_json`]）/ control frame の判定
//!   （[`classify_control_frame`]）/ 質問 input の写像（[`parse_question_specs`]）/ control_response
//!   の組み立て（[`build_permission_response`]）— いずれも純関数で単体テスト可能。
//! - actions: spawn / stdout→translate→broadcast / in-flight tail 更新 / cc_session 記録 /
//!   stdin 書き込み（user message・initialize handshake・control_response）

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;

use super::claude_translate::ClaudeTranslator;
use super::event::{ConversationEvent, QuestionOption, QuestionSpec};

/// 我々が spawn 直後に送る initialize control_request の request_id（応答照合用の固定値）。
const INIT_REQUEST_ID: &str = "vp-init-1";

/// disk（transcript）にまだ載っていない増分と、その commit 世代。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InFlight {
    /// commit 世代。 transcript に 1 行 flush されるたび +1。 transcript を読む側は
    /// 「読む前」と「読んだ後」で同値なら tail が transcript と整合すると判定できる。
    pub seq: u64,
    /// 直近 commit 以降の [`ConversationEvent::MessageChunk`] / [`ConversationEvent::ThoughtChunk`]。
    pub tail: Vec<ConversationEvent>,
}

impl InFlight {
    /// tail を捨てて commit 世代を進める。 世代を進めることで、 並行して transcript を読んでいる
    /// replay handler が「状態が動いた」ことを検知して読み直せる。
    fn reset(&mut self) {
        self.tail.clear();
        self.seq = self.seq.wrapping_add(1);
    }
}

/// tail に積んでよい event か（= commit 前にしか存在しない増分）。
///
/// ToolCall / ToolCallUpdate / Plan は commit 済み block 由来なので積まない（module doc の ⚠️）。
fn is_uncommitted_chunk(event: &ConversationEvent) -> bool {
    matches!(
        event,
        ConversationEvent::MessageChunk { .. } | ConversationEvent::ThoughtChunk { .. }
    )
}

/// stdout 1 行分の翻訳結果を in-flight tail に畳み込む（純粋 mutation = 単体テスト可能）。
///
/// - commit 境界（`assistant` / `user` 行）: tail を捨てて世代 +1。
/// - SessionInit（新 engine の起点）/ TurnCompleted / Error: 会話が確定するので同じく捨てる。
/// - それ以外の増分（MessageChunk / ThoughtChunk）だけを積む。
fn fold_in_flight(f: &mut InFlight, out: &super::claude_translate::Ingested) {
    if out.commits_transcript {
        f.reset();
    }
    for event in &out.events {
        match event {
            ConversationEvent::SessionInit { .. }
            | ConversationEvent::TurnCompleted { .. }
            | ConversationEvent::Error { .. }
            | ConversationEvent::EngineExited { .. } => f.reset(),
            e if is_uncommitted_chunk(e) => f.tail.push(e.clone()),
            _ => {}
        }
    }
}

/// ClaudeHost の起動設定。
#[derive(Debug, Clone)]
pub struct ClaudeHostConfig {
    /// engine の作業ディレクトリ（lane の repo dir）。
    pub cwd: String,
    /// cc_session 記録キー（repo 名）。
    pub repo: String,
    /// cc_session 記録キー（**store label**: session #1 = 素の lane 名 / #2 以降は `<lane>#<n>`）。
    /// ⚠️ env の `VP_LANE` には使わない — そちらは [`Self::lane_label`]（素の label）。
    pub lane: String,
    /// identity env（`VP_LANE`）用の素の lane label（doc 51 §1 A3b — tui の
    /// agent_spawner 注入と同じ契約。`vp now` / wire がこれを読んで宛先を名乗る）。
    pub lane_label: String,
    /// identity env（`VP_SESSION_KEY`）用の session key（doc 40 §4 の hook identity と同じ —
    /// gui の engine が「自分がどの session か」を名乗れるようにする）。
    pub session_key: crate::lane::session_registry::SessionKey,
    /// 再開する session id（`--resume`）。tui ⇄ gui 切替 / repo 再起動復帰に使う。
    pub resume_session_id: Option<String>,
    /// 使用モデル（`--model`）。None = claude default。
    pub model: Option<String>,
    /// claude CLI パス（未指定なら PATH / well-known から解決）。
    pub claude_cli_path: Option<String>,
}

/// 逆方向 `can_use_tool` への回答（doc 35 §2.3）。GUI の PromptCard 応答を control_response へ写す。
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    /// 許可。`answers` は AskUserQuestion の回答 map（`{question: label}`）で、原 input（questions を
    /// 含む）へマージして `updatedInput` にする。`None` は「原 input をそのまま許可」（tool 承認 = PR3）。
    Allow { answers: Option<serde_json::Value> },
    /// 拒否。`message` を engine に返す（PR3 で使う。PR1 の質問レールでは基本 allow）。
    Deny { message: String },
}

/// lane 単位の headless claude engine host。
///
/// doc 33: LanePool のエンジンスロットに保持される。`RwLock<LanePool>` の **read lock 下で
/// submit** できるよう、stdin は内部 `tokio::sync::Mutex` で持つ（`submit(&self)`）。
///
/// stdin は `Arc` 共有: host（submit / respond_permission）と stdout pump（handshake / 想定外
/// can_use_tool の自動 allow）の両方が書くため。各 write は 1 JSON 行 + flush で完結し Mutex が
/// 直列化するので、user message と control frame が混線しない（doc §2.3）。
pub struct ClaudeHost {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    event_tx: broadcast::Sender<ConversationEvent>,
    pid: Option<u32>,
    /// stdout ポンプが更新する in-flight tail（module doc）。 await を跨がないので std Mutex。
    in_flight: Arc<Mutex<InFlight>>,
    /// 逆方向 `can_use_tool` の pending 質問（request_id → 原 input）。回答時に原 questions を
    /// verbatim で `updatedInput` へ echo するため保持する（doc §8 の wire 契約）。await を跨がない。
    pending_permissions: Arc<Mutex<HashMap<String, serde_json::Value>>>,
}

/// claude CLI が `--forward-subagent-text`（2.1.211+）を受理するか。
///
/// 未対応の版に渡すと `error: unknown option` で **spawn 即死**し、gui engine が
/// 一切起動しなくなる（respawn しても即死のループ = 「送信しても復活しない」。
/// 2026-07-19 に claude 2.1.205 環境で実発生）。
///
/// probe 形は `-p --forward-subagent-text` + stdin 即 EOF（2.1.205 実測）:
/// - 未対応: argv パースで `error: unknown option` を stderr に出して即終了
/// - 対応: パース通過後「Input must be provided」で即終了（interactive 化せず API も呼ばれない）
/// - `--help` / `--version` 併用は unknown option が黙殺され exit 0 になるため使えない。
///   exit code は両形とも 1 なので **stderr の unknown option 文言**で判定する。
///
/// probe は repo プロセスごとに 1 回だけ（OnceLock。claude path の変更は repo 再起動で再評価）。
/// probe 失敗は「付けない」に倒す（fail-open: subagent 発話が出ない機能退化 >> engine 全死）。
fn supports_forward_subagent_text(claude_path: &str) -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let supported = probe_forward_subagent_text(claude_path);
        if !supported {
            tracing::warn!(
                "claude CLI が --forward-subagent-text 未対応（2.1.211 未満）— \
                 フラグ無しで起動します（gui に subagent 発話は出ません。claude update で解消）"
            );
        }
        supported
    })
}

/// probe 本体（[`supports_forward_subagent_text`] の実装）。
///
/// 呼び出し経路は `ensure_chat_engine` = LanePool の **repo 全体 write lock 保持下**なので、
/// 子プロセスを無期限に待つと probe hang = 当該 repo の全 lane 操作が凍る（spawn 即死より
/// 悪い障害モード）。`try_wait` polling に期限を張り、超過は kill して未対応扱いに倒す
/// （fail-open: 実測の probe は ~1s、期限は起動遅延マシンの余裕込み）。
fn probe_forward_subagent_text(claude_path: &str) -> bool {
    const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    let Ok(mut child) = std::process::Command::new(claude_path)
        .args(["-p", "--forward-subagent-text"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return false; // claude 不在等 — 実 spawn 側が同じ失敗を anyhow で報告する
    };
    let deadline = std::time::Instant::now() + PROBE_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                tracing::warn!(
                    "claude probe が {PROBE_DEADLINE:?} 以内に終了せず — kill して未対応扱い"
                );
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
    }
    // stderr は終了後に読む（エラーは 1 行だけなので pipe buffer 詰まりの懸念なし）。
    // 未対応版のみ `error: unknown option` を出す — exit code は既知/未知どちらも非 0 で
    // 判定に使えない（2.1.205 実測。probe doc の経緯参照）。
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stderr);
    }
    !stderr.contains("unknown option")
}

impl ClaudeHost {
    /// headless claude を spawn し、stdout ポンプを起動する。
    ///
    /// stdout の各行は [`ClaudeTranslator`] を通り、[`ConversationEvent`] として broadcast される。
    /// `SessionInit` を観測したら session id を cc_session に記録する（resume の SSOT）。
    pub fn spawn(config: ClaudeHostConfig) -> anyhow::Result<Self> {
        let claude_path = crate::agent::get_claude_cli_path(config.claude_cli_path.as_deref());
        let mut cmd = Command::new(&claude_path);
        // 親（repo）の env を継承 — spawn_env 済みの PATH 等を引き継ぐ。
        cmd.envs(std::env::vars());
        // identity env（doc 51 §1 A3b）: tui の agent_spawner と同じ契約を gui にも。
        // engine（とその shell tool の子プロセス）が `vp now` / wire で自分を名乗るための口。
        cmd.env("VP_REPO", &config.repo);
        cmd.env("VP_LANE", &config.lane_label);
        cmd.env("VP_SESSION_KEY", config.session_key.to_string());
        // cwd 空は「継承」（呼び元の cwd を使う）— test / repo_dir 未解決時の防御。
        if !config.cwd.is_empty() {
            cmd.current_dir(&config.cwd);
        }

        // 双方向 stream-json + partial（Step 0 で確定した gui 駆動形、doc 32 §10）。
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--include-partial-messages")
            .arg("--verbose");

        // subagent（Agent tool が回した子）の発話を stream に載せる（claude 2.1.211+）。
        // 無いと gui で Agent 行は「実行中…→✓」の黒箱になり、子が何を考え何をしたかが
        // 一切見えない（VP は subagent を多用する開発フローなので損失が大きい）。
        //
        // 出てくる形（実測 2026-07-17、claude 2.1.212）:
        //   - 行 top-level に `parent_tool_use_id` が付き、値は親の `Agent` tool_use の id と一致
        //   - 担い手は **assistant スナップショット行のみ**（parent 付きの `stream_event` は 0 本 =
        //     delta では来ない）→ 翻訳は ClaudeTranslator 側で snapshot から取り出す
        // 翻訳器が parent 付き行を親から隔離する前提の flag（孤児 ToolCallUpdate / 誤 commit 境界 /
        // 親の発話への混入を防ぐ）。 translate.rs の RawLine 分岐と対で読むこと。
        // ⚠️ 2.1.211 未満は unknown option で spawn 即死するため、受理可否を probe して出し分ける
        // （probe 詳細と実障害の経緯は supports_forward_subagent_text の doc）。
        if supports_forward_subagent_text(&claude_path) {
            cmd.arg("--forward-subagent-text");
        }

        // tui（TUI）が bypassPermissions で全ツール素通しなのに gui を揃える（doc 33 §9、
        // user 要件 2026-07-09「mode I レベルにここも合わせよう」）。bypassPermissions で TUI と
        // 同じ体験にする（通常 tool は素通し）。
        cmd.arg("--permission-mode").arg("bypassPermissions");

        // doc 35 PR1: 質問/承認を stdio control channel へ routing する点火条件（決定 spike §8 で
        // 実測）。`--permission-prompt-tool stdio` は claude 2.1.197 の `--help` に出ない hidden flag
        // だが受理・機能する（調査 §10.1 の「削除済み」は非表示からの誤推論を spike が反証）。
        // bypassPermissions のまま stdio を足すと、通常 tool（Write/Bash）は素通しのまま
        // AskUserQuestion だけが逆方向 can_use_tool として飛ぶ → 既定を維持したまま質問だけ拾える。
        cmd.arg("--permission-prompt-tool").arg("stdio");

        if let Some(ref model) = config.model {
            cmd.arg("--model").arg(model);
        }
        // resume: session id 保持で文脈継続（Step 0 Spike C で実証）。
        if let Some(ref sid) = config.resume_session_id {
            cmd.arg("--resume").arg(sid);
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // host が drop されたら engine プロセスも殺す（map から外れた headless claude を
        // orphan にしない）。stop() を明示しない撤収経路の安全網。
        cmd.kill_on_drop(true);

        tracing::info!(
            "ClaudeHost spawn (repo={}, lane={}, resume={:?})",
            config.repo,
            config.lane,
            config.resume_session_id.as_deref().unwrap_or("new")
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("headless claude の起動に失敗（PATH に claude があるか確認）: {e}")
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdin のキャプチャに失敗"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout のキャプチャに失敗"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("stderr のキャプチャに失敗"))?;

        // broadcast: 複数 consumer（conversation_pump / 将来の tui mirror）に配れる。
        let (event_tx, _rx) = broadcast::channel::<ConversationEvent>(256);

        let in_flight = Arc::new(Mutex::new(InFlight::default()));
        // stdin を Arc 共有（host + pump が書く、doc §2.3）。
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let pending_permissions: Arc<Mutex<HashMap<String, serde_json::Value>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // initialize handshake（control channel 確立、doc §2.2）。pump 開始と競うが、submit は
        // spawn 後の別 dispatch（network 往復先）なので実質先行する。応答は pump が request_id で確認。
        let handshake_stdin = stdin.clone();
        tokio::spawn(async move {
            let frame = initialize_handshake_json();
            if let Err(e) = write_json_line(&handshake_stdin, &frame).await {
                tracing::warn!("conversation: initialize handshake 送信失敗: {e}");
            }
        });

        // stdout ポンプ: 行 → (control frame なら分岐) → translate → in-flight tail 更新 → broadcast
        //（+ SessionInit で cc_session 記録）。
        let tx = event_tx.clone();
        let repo = config.repo.clone();
        let lane = config.lane.clone();
        let pump_in_flight = in_flight.clone();
        let pump_pending = pending_permissions.clone();
        tokio::spawn(async move {
            let mut translator = ClaudeTranslator::new();
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // 制御面は translator の手前で抜く（ConversationEvent 語彙を engine 制御で汚さない）。
                if let Some(frame) = classify_control_frame(&line) {
                    handle_control_frame(frame, &tx, &pump_pending);
                    continue;
                }
                let out = translator.ingest(&line);
                // tail 更新は broadcast より先。 replay handler が「配信済みだが tail に無い」
                // 増分を取りこぼさない順序（tail ⊇ 配信済み未 commit 分 を保つ）。
                fold_in_flight(&mut pump_in_flight.lock().expect("in_flight lock"), &out);
                for event in out.events {
                    if let ConversationEvent::SessionInit { session_id, .. } = &event {
                        record_session(&repo, &lane, session_id);
                    }
                    // 受信者不在（Closed）は無視 — engine は購読者に依存せず動く。
                    let _ = tx.send(event);
                }
            }
            tracing::debug!("ClaudeHost stdout ポンプ終了（repo={repo}, lane={lane}）");
            // stream 途絶（engine crash / stop）を GUI に可視化する。stdout close =
            // engine プロセス終了だが、従来は debug log に落ちるだけで購読者（chatview）に
            // 届かず「止まった?」が見えなかった（tui は PTY 切断が xterm に即見える）。
            // 既存 Error 経路に相乗りして chatview に途絶を出す。
            // （stop/crash の区別・EngineExited 専用 variant・再起動ボタンは後続 PR）
            // engine が死んだ = tail の続きも pending 質問への応答先ももう無い。 掃除する。
            pump_in_flight.lock().expect("in_flight lock").reset();
            pump_pending.lock().expect("pending lock").clear();
            let _ = tx.send(ConversationEvent::EngineExited {
                message: "エンジンが休眠しました（メッセージ送信で再開します）".to_string(),
            });
        });

        // stderr はログのみ。
        let lane_err = config.lane.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("headless claude stderr (lane={lane_err}): {line}");
            }
        });

        let pid = child.id();
        Ok(Self {
            child,
            stdin,
            event_tx,
            pid,
            in_flight,
            pending_permissions,
        })
    }

    /// ConversationEvent の broadcast receiver を得る（conversation_pump などが購読）。
    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.event_tx.subscribe()
    }

    /// disk にまだ載っていない増分（+ commit 世代）のスナップショット（module doc）。
    ///
    /// replay handler はこれを transcript の後ろへ継ぐ。 transcript 読みの前後で
    /// [`InFlight::seq`] が変わっていなければ、 tail と transcript は重複も欠落もしない。
    pub fn in_flight(&self) -> InFlight {
        self.in_flight.lock().expect("in_flight lock").clone()
    }

    /// 現在の commit 世代のみ（transcript 読み後の検算用）。
    pub fn commit_seq(&self) -> u64 {
        self.in_flight.lock().expect("in_flight lock").seq
    }

    /// engine プロセスの pid（spawn 直後に採取。LaneInfo.pid に載せる）。
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// ユーザープロンプトを engine に投入する（1 ターン開始）。
    ///
    /// `&self`: 内部 Mutex で stdin を直列化（LanePool read lock 下から呼べる）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        let json = user_message_json(prompt);
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 逆方向 `can_use_tool`（[`ConversationEvent::Question`]）への回答を stdin へ書き戻す（doc §2.3）。
    ///
    /// `request_id` は Question event 経由で GUI から戻る。pending から原 input（questions を含む）を
    /// 引き、`decision` に応じて `control_response{behavior, updatedInput|message}` を組み立てて 1 行書く。
    /// `&self`: 既存 stdin Mutex を submit と共有（LanePool read lock 下から呼べる）。
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> anyhow::Result<()> {
        // pending から原 input を取り出す（回答は 1 回きり = remove）。無ければ空 object にフォールバック。
        let original = self
            .pending_permissions
            .lock()
            .expect("pending lock")
            .remove(request_id);
        let frame = build_permission_response(request_id, &decision, original.as_ref());
        write_json_line(&self.stdin, &frame).await
    }

    /// 実行中 turn を中断する（control interrupt、doc 35 §5）。engine は turn を止め、次の submit を
    /// 受けられる（プロセスは生存）。fire-and-forget: 停止結果は通常の result → TurnCompleted/Error
    /// として stream に流れるので、専用の応答待ちは持たない。`&self`: stdin Mutex を submit と共有。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        let seq = CONTROL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let frame = interrupt_request_json(&format!("interrupt-{seq}"));
        write_json_line(&self.stdin, &frame).await
    }

    /// permission mode を動的に切替える（doc 35 §2.5 / PR3）。`"default"` で tool 承認要求が飛ぶ
    /// ように、`"bypassPermissions"` で素通しに戻す。fire-and-forget（効果は次の can_use_tool 有無で
    /// 観測）。`&self`: stdin Mutex を submit と共有。
    pub async fn set_permission_mode(&self, mode: &str) -> anyhow::Result<()> {
        let seq = CONTROL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let frame = set_permission_mode_json(&format!("set-mode-{seq}"), mode);
        write_json_line(&self.stdin, &frame).await
    }

    /// engine プロセスを停止する（drop でも kill_on_drop が効くが、明示停止用）。
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        self.child.kill().await?;
        Ok(())
    }
}

/// stdin 用 user メッセージ JSON を生成する（純関数）。
///
/// 形式: `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
fn user_message_json(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
    .to_string()
}

/// SessionInit で観測した session id を session registry に記録する（doc 40 §4 — 会話 id の
/// SSOT は registry）。`lane` は host config の session label（`conductor` / `conductor#2`）
/// なので registry の (lane, key) へ逆引きして書く。headless spawn に `|| claude` fallback は
/// 無い（doc 33 C2 pre-flight 済み）ため guard 不要の無条件 authoritative 書き込み。
fn record_session(repo: &str, lane: &str, session_id: &str) {
    let (lane_label, key) = crate::lane::session_registry::parse_session_label(lane);
    if let Err(e) = crate::lane::session_registry::set_conversation(
        repo,
        lane_label,
        "claude",
        key,
        Some(session_id),
    ) {
        tracing::warn!("会話 id の registry 記録失敗（repo={repo}, lane={lane}）: {e}");
    }
}

// =============================================================================
// control protocol（doc 35 PR1）— 判定・写像・組み立ては純関数、書き込みだけ action
// =============================================================================

/// stdin へ 1 JSON 行 + `\n` + flush で書く（action）。submit / respond_permission / handshake /
/// 自動 allow が共用。各 write を Mutex が直列化するので混線しない（doc §2.3）。
async fn write_json_line(
    stdin: &tokio::sync::Mutex<tokio::process::ChildStdin>,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let line = serde_json::to_string(value)?;
    let mut guard = stdin.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await?;
    Ok(())
}

/// 能動 control（interrupt 等）の request_id 採番用グローバル counter。fire-and-forget なので
/// host 跨ぎでも衝突は無害だが、hygiene のため単調増加させる。
static CONTROL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// interrupt control_request（純関数、doc §5）。engine は現在の turn を中断する（spikeB 実測）。
fn interrupt_request_json(request_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" }
    })
}

/// set_permission_mode control_request（純関数、doc §2.5）。mode 動的切替（default / bypassPermissions…）。
fn set_permission_mode_json(request_id: &str, mode: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "set_permission_mode", "mode": mode }
    })
}

/// initialize handshake の control_request（純関数、doc §2.2）。`hooks` は空（null）で足りる。
fn initialize_handshake_json() -> serde_json::Value {
    serde_json::json!({
        "type": "control_request",
        "request_id": INIT_REQUEST_ID,
        "request": { "subtype": "initialize", "hooks": null }
    })
}

/// stdout 1 行を control frame として分類した結果（translator の手前で抜く対象）。
#[derive(Debug, PartialEq)]
enum ControlFrame {
    /// 逆方向 `can_use_tool`（claude → 我々）。tool_name で GUI へ写す or 自動応答する。
    CanUseTool {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    /// 我々の能動 control（initialize / interrupt 等）への応答。GUI には出さない。
    ControlResponse { request_id: Option<String> },
}

/// stdout 1 行が control frame なら分類する（純関数、doc §2.4）。
///
/// - `type=="control_request"` && `request.subtype=="can_use_tool"` → [`ControlFrame::CanUseTool`]。
/// - `type=="control_response"` → [`ControlFrame::ControlResponse`]。
/// - それ以外（会話本文 / 未知）は `None` = translator に流す。
fn classify_control_frame(line: &str) -> Option<ControlFrame> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    match v.get("type").and_then(|t| t.as_str())? {
        "control_request" => {
            let req = v.get("request")?;
            // can_use_tool 以外の control_request（稀）は translator の Other が握り潰すので流す。
            if req.get("subtype").and_then(|s| s.as_str())? != "can_use_tool" {
                return None;
            }
            let request_id = v.get("request_id").and_then(|r| r.as_str())?.to_string();
            let tool_name = req.get("tool_name").and_then(|t| t.as_str())?.to_string();
            let input = req
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(ControlFrame::CanUseTool {
                request_id,
                tool_name,
                input,
            })
        }
        "control_response" => Some(ControlFrame::ControlResponse {
            request_id: v
                .get("request_id")
                .or_else(|| v.get("response").and_then(|r| r.get("request_id")))
                .and_then(|r| r.as_str())
                .map(str::to_string),
        }),
        _ => None,
    }
}

/// AskUserQuestion の逆方向 input（`{questions:[{question,header,options,multiSelect}]}`、camelCase）を
/// GUI 語彙 [`QuestionSpec`] 列へ写す（純関数、doc §3 / §8 の生 wire）。
fn parse_question_specs(input: &serde_json::Value) -> Vec<QuestionSpec> {
    let Some(questions) = input.get("questions").and_then(|q| q.as_array()) else {
        return Vec::new();
    };
    questions
        .iter()
        .map(|q| QuestionSpec {
            question: str_field(q, "question"),
            header: str_field(q, "header"),
            options: q
                .get("options")
                .and_then(|o| o.as_array())
                .map(|opts| {
                    opts.iter()
                        .map(|o| QuestionOption {
                            label: str_field(o, "label"),
                            description: str_field(o, "description"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            multi_select: q
                .get("multiSelect")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
        .collect()
}

/// JSON object の string field を取り出す（無ければ空文字）。
fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// 逆方向 `can_use_tool` への control_response JSON を組み立てる（純関数、doc §2.3 / §8 の wire）。
///
/// - allow: `updatedInput` = 原 input（AskUserQuestion なら questions を含む）。`answers` があれば
///   マージ → `{questions, answers}` になる（spike の echo 契約）。原 input 不在なら空 object 起点。
/// - deny: `{behavior:"deny", message}`。
fn build_permission_response(
    request_id: &str,
    decision: &PermissionDecision,
    original_input: Option<&serde_json::Value>,
) -> serde_json::Value {
    let inner = match decision {
        PermissionDecision::Allow { answers } => {
            let mut updated = original_input
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(answers) = answers
                && let Some(obj) = updated.as_object_mut()
            {
                obj.insert("answers".to_string(), answers.clone());
            }
            serde_json::json!({ "behavior": "allow", "updatedInput": updated })
        }
        PermissionDecision::Deny { message } => {
            serde_json::json!({ "behavior": "deny", "message": message })
        }
    };
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": inner,
        }
    })
}

/// 分類済み control frame を処理する（action = broadcast / pending 登録）。純同期 — stdin へは
/// 書かない（回答は GUI → respond_permission が担う）。
///
/// - AskUserQuestion → 原 input を pending へ退避（回答時に questions を verbatim echo）+
///   [`ConversationEvent::Question`] を broadcast。
/// - その他の can_use_tool（permission-mode=default 時の tool 承認）→ 同じく pending 退避 +
///   [`ConversationEvent::PermissionRequest`] を broadcast（PR3）。
/// - control_response（我々の能動 control への応答）→ init のみ log、他は無視（GUI に出さない）。
fn handle_control_frame(
    frame: ControlFrame,
    tx: &broadcast::Sender<ConversationEvent>,
    pending: &Arc<Mutex<HashMap<String, serde_json::Value>>>,
) {
    match frame {
        ControlFrame::CanUseTool {
            request_id,
            tool_name,
            input,
        } => {
            if tool_name == "AskUserQuestion" {
                let questions = parse_question_specs(&input);
                pending
                    .lock()
                    .expect("pending lock")
                    .insert(request_id.clone(), input);
                let _ = tx.send(ConversationEvent::Question {
                    request_id,
                    questions,
                });
            } else {
                // PR3: tool 承認要求（permission-mode=default 時に飛ぶ）。AskUserQuestion と同じレール —
                // 原 input を pending へ退避（allow 時に verbatim echo）+ PermissionRequest を broadcast。
                // GUI が allow/deny を respond_permission で戻し、host が control_response を書く。
                pending
                    .lock()
                    .expect("pending lock")
                    .insert(request_id.clone(), input.clone());
                let _ = tx.send(ConversationEvent::PermissionRequest {
                    request_id,
                    tool_name,
                    input,
                });
            }
        }
        ControlFrame::ControlResponse { request_id } => {
            // PR1 の能動 control は initialize のみ。応答を request_id で確認する（doc §2.2）。
            if request_id.as_deref() == Some(INIT_REQUEST_ID) {
                tracing::debug!(
                    "conversation: initialize handshake 確認（request_id={INIT_REQUEST_ID}）"
                );
            }
            // interrupt / set_permission_mode の応答（PR2+）はまだ送らないので無視。
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stream-json の行列を translator + fold_in_flight に通し、最終 tail を得る helper。
    /// 実 engine の stdout ポンプと同じ順序で畳む。
    fn tail_after(lines: &[&str]) -> InFlight {
        let mut translator = ClaudeTranslator::new();
        let mut f = InFlight::default();
        for line in lines {
            fold_in_flight(&mut f, &translator.ingest(line));
        }
        f
    }

    fn text_delta(text: &str) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}}}"#
        )
    }

    const TEXT_BLOCK_START: &str = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
    const ASSISTANT_SNAPSHOT: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"完成"}]},"session_id":"s"}"#;

    /// 生成中（commit 前）の delta は tail に積まれる = replay で継げる。
    #[test]
    fn in_flight_tail_accumulates_uncommitted_text() {
        let f = tail_after(&[TEXT_BLOCK_START, &text_delta("こん"), &text_delta("にちは")]);
        assert_eq!(
            f.tail,
            vec![
                ConversationEvent::MessageChunk {
                    text: "こん".into()
                },
                ConversationEvent::MessageChunk {
                    text: "にちは".into()
                },
            ]
        );
    }

    /// commit 境界（assistant スナップショット）で tail は捨てられ、世代が進む。
    /// disk に載った内容を tail が二重に持たないことの担保。
    #[test]
    fn commit_clears_tail_and_bumps_seq() {
        let f = tail_after(&[TEXT_BLOCK_START, &text_delta("完成"), ASSISTANT_SNAPSHOT]);
        assert_eq!(f.tail, Vec::new(), "commit 済み text は tail に残さない");
        assert_eq!(f.seq, 1, "commit 世代が進む");
    }

    /// commit 後に始まった次 block の delta だけが tail に残る（transcript の続きになる）。
    #[test]
    fn tail_holds_only_chunks_after_last_commit() {
        let f = tail_after(&[
            TEXT_BLOCK_START,
            &text_delta("前"),
            ASSISTANT_SNAPSHOT,
            TEXT_BLOCK_START,
            &text_delta("後"),
        ]);
        assert_eq!(
            f.tail,
            vec![ConversationEvent::MessageChunk { text: "後".into() }]
        );
    }

    /// 回帰: ToolCall は tail に載らない。 translator は `content_block_stop` で ToolCall を出すが、
    /// その block の commit（snapshot 行）は 1 行前に来ているので transcript 側に既に居る。
    /// tail にも積むと replay で tool カードが二重化する。
    #[test]
    fn tool_call_is_never_buffered_in_tail() {
        let f = tail_after(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-1","name":"Bash","input":{}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}}"#,
            // 実測順: snapshot(commit) → content_block_stop(ToolCall 発火)
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu-1","name":"Bash","input":{"command":"ls"}}]},"session_id":"s"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ]);
        assert_eq!(f.tail, Vec::new(), "ToolCall は tail 対象外");
    }

    /// tool_result（user 行）も commit 境界。 ToolCallUpdate は tail に積まない。
    #[test]
    fn tool_result_commits_and_is_not_buffered() {
        let f = tail_after(&[
            TEXT_BLOCK_START,
            &text_delta("途中"),
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"ok","is_error":false}]}}"#,
        ]);
        assert_eq!(f.tail, Vec::new());
        assert_eq!(f.seq, 1);
    }

    /// turn 完了 / engine error / 新 session は tail を捨てる（会話が確定 or 起点リセット）。
    #[test]
    fn terminal_events_reset_tail() {
        for last in [
            r#"{"type":"result","subtype":"success","session_id":"s","is_error":false}"#,
            r#"{"type":"result","subtype":"error","session_id":"s","is_error":true,"result":"boom"}"#,
            r#"{"type":"system","subtype":"init","session_id":"s2","cwd":"/tmp"}"#,
        ] {
            let f = tail_after(&[TEXT_BLOCK_START, &text_delta("途中"), last]);
            assert_eq!(f.tail, Vec::new(), "tail が残っている: {last}");
            assert!(f.seq > 0, "世代が進む: {last}");
        }
    }

    #[test]
    fn user_message_json_is_valid_stream_json() {
        let json = user_message_json("こんにちは");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(v["message"]["content"][0]["text"], "こんにちは");
    }

    #[test]
    fn user_message_json_escapes_quotes_and_newlines() {
        // quote / 改行 / バックスラッシュを含むプロンプトでも valid JSON 1 行になる。
        let tricky = "say \"hi\"\nnext\tline \\ end";
        let json = user_message_json(tricky);
        assert!(
            !json.contains('\n'),
            "serialized JSON は 1 行（生改行なし）"
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["message"]["content"][0]["text"], tricky);
    }

    // =========================================================================
    // control protocol（doc 35 PR1）— 純関数の単体テスト
    // =========================================================================

    /// interrupt control_request（doc 35 §5 / PR2）の wire 形。
    #[test]
    fn interrupt_request_json_is_wellformed() {
        let v = interrupt_request_json("interrupt-7");
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "interrupt-7");
        assert_eq!(v["request"]["subtype"], "interrupt");
    }

    /// set_permission_mode control_request（PR3）の wire 形。
    #[test]
    fn set_permission_mode_json_is_wellformed() {
        let v = set_permission_mode_json("set-mode-1", "default");
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "set_permission_mode");
        assert_eq!(v["request"]["mode"], "default");
    }

    /// PR3: 非 AskUserQuestion の can_use_tool は PermissionRequest として broadcast + pending 退避。
    #[test]
    fn handle_control_frame_non_question_emits_permission_request() {
        let (tx, mut rx) = broadcast::channel::<ConversationEvent>(4);
        let pending: Arc<Mutex<HashMap<String, serde_json::Value>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let frame = ControlFrame::CanUseTool {
            request_id: "r-perm".to_string(),
            tool_name: "Bash".to_string(),
            input: serde_json::json!({ "command": "ls" }),
        };
        handle_control_frame(frame, &tx, &pending);
        assert!(
            pending.lock().unwrap().contains_key("r-perm"),
            "原 input が pending に退避される"
        );
        match rx.try_recv().expect("event") {
            ConversationEvent::PermissionRequest {
                request_id,
                tool_name,
                ..
            } => {
                assert_eq!(request_id, "r-perm");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("PermissionRequest を期待: {other:?}"),
        }
    }

    /// 決定 spike §8 の生 wire（AskUserQuestion の逆方向 can_use_tool）。
    const CAN_USE_TOOL_ASK: &str = r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","tool_use_id":"toolu_1","input":{"questions":[{"question":"Which language?","header":"Greeting Language","options":[{"label":"English","description":"en"},{"label":"Japanese (日本語)","description":"ja"}],"multiSelect":false}]}}}"#;

    /// can_use_tool(AskUserQuestion) を CanUseTool として分類し、tool_name / input を保つ。
    #[test]
    fn classify_detects_can_use_tool() {
        match classify_control_frame(CAN_USE_TOOL_ASK) {
            Some(ControlFrame::CanUseTool {
                request_id,
                tool_name,
                input,
            }) => {
                assert_eq!(request_id, "req-1");
                assert_eq!(tool_name, "AskUserQuestion");
                assert!(input.get("questions").is_some(), "input.questions を保持");
            }
            other => panic!("expected CanUseTool, got {other:?}"),
        }
    }

    /// control_response は request_id 付きで分類される（init 確認用）。
    #[test]
    fn classify_detects_control_response() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"vp-init-1","response":{"commands":[]}}}"#;
        assert_eq!(
            classify_control_frame(line),
            Some(ControlFrame::ControlResponse {
                request_id: Some("vp-init-1".to_string())
            })
        );
    }

    /// 会話本文・未知 type・非 can_use_tool の control_request は None（translator に流す）。
    #[test]
    fn classify_passes_through_non_control_lines() {
        for line in [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[]},"session_id":"s"}"#,
            r#"{"type":"result","subtype":"success","session_id":"s","is_error":false}"#,
            // can_use_tool 以外の control_request（稀）は translator の Other 任せ。
            r#"{"type":"control_request","request_id":"x","request":{"subtype":"mcp_message"}}"#,
            "not json",
        ] {
            assert_eq!(
                classify_control_frame(line),
                None,
                "translator に流すべき: {line}"
            );
        }
    }

    /// 生 wire（camelCase multiSelect）を QuestionSpec（snake_case）へ正しく写す。
    #[test]
    fn parse_question_specs_maps_wire_to_vocab() {
        let input: serde_json::Value = serde_json::from_str(
            r#"{"questions":[{"question":"Q?","header":"H","options":[{"label":"A","description":"da"},{"label":"B"}],"multiSelect":true}]}"#,
        )
        .unwrap();
        let specs = parse_question_specs(&input);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].question, "Q?");
        assert_eq!(specs[0].header, "H");
        assert!(specs[0].multi_select, "multiSelect→multi_select");
        assert_eq!(specs[0].options.len(), 2);
        assert_eq!(specs[0].options[0].label, "A");
        assert_eq!(specs[0].options[0].description, "da");
        assert_eq!(specs[0].options[1].label, "B");
        assert_eq!(specs[0].options[1].description, "", "description 欠落は空");
    }

    /// questions 不在の input は空列（防御）。
    #[test]
    fn parse_question_specs_handles_missing_questions() {
        assert!(parse_question_specs(&serde_json::json!({})).is_empty());
    }

    /// allow: updatedInput = 原 input（questions）に answers をマージ = spike の echo 契約。
    #[test]
    fn build_allow_response_merges_answers_into_original_input() {
        let original: serde_json::Value =
            serde_json::from_str(r#"{"questions":[{"question":"Q?"}]}"#).unwrap();
        let answers = serde_json::json!({ "Q?": "English" });
        let frame = build_permission_response(
            "req-1",
            &PermissionDecision::Allow {
                answers: Some(answers.clone()),
            },
            Some(&original),
        );
        assert_eq!(frame["type"], "control_response");
        let resp = &frame["response"];
        assert_eq!(resp["subtype"], "success");
        assert_eq!(resp["request_id"], "req-1");
        assert_eq!(resp["response"]["behavior"], "allow");
        let updated = &resp["response"]["updatedInput"];
        // 原 questions が verbatim で残り、answers が差し込まれている。
        assert_eq!(updated["questions"][0]["question"], "Q?");
        assert_eq!(updated["answers"]["Q?"], "English");
    }

    /// deny: {behavior:"deny", message}（原 input は使わない）。
    #[test]
    fn build_deny_response_carries_message() {
        let frame = build_permission_response(
            "req-2",
            &PermissionDecision::Deny {
                message: "却下".to_string(),
            },
            None,
        );
        let inner = &frame["response"]["response"];
        assert_eq!(inner["behavior"], "deny");
        assert_eq!(inner["message"], "却下");
        assert!(inner.get("updatedInput").is_none());
    }

    /// answers=None（tool 承認 = PR3 の形）: updatedInput = 原 input そのまま。
    #[test]
    fn build_allow_response_without_answers_conversation_original() {
        let original = serde_json::json!({ "command": "ls" });
        let frame = build_permission_response(
            "req-3",
            &PermissionDecision::Allow { answers: None },
            Some(&original),
        );
        assert_eq!(
            frame["response"]["response"]["updatedInput"]["command"],
            "ls"
        );
    }

    /// initialize handshake frame は固定 request_id + hooks:null。
    #[test]
    fn initialize_handshake_is_well_formed() {
        let f = initialize_handshake_json();
        assert_eq!(f["type"], "control_request");
        assert_eq!(f["request_id"], INIT_REQUEST_ID);
        assert_eq!(f["request"]["subtype"], "initialize");
        assert!(f["request"]["hooks"].is_null());
    }

    /// 実機統合: headless claude を spawn → submit → ConversationEvent 列を受け取り、
    /// SessionInit / MessageChunk(PONG) / TurnCompleted が揃うことを確認する。
    /// `cargo test -p vantage-point --ignored conversation_host_roundtrip` で実行（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn conversation_host_roundtrip() {
        use std::time::Duration;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut host = ClaudeHost::spawn(ClaudeHostConfig {
            cwd: tmp.path().to_string_lossy().to_string(),
            repo: "vp-test".to_string(),
            lane: "spike".to_string(),
            lane_label: "spike".to_string(),
            session_key: 1,
            resume_session_id: None,
            model: Some("haiku".to_string()),
            claude_cli_path: None,
        })
        .expect("spawn host");

        let mut rx = host.subscribe();
        host.submit("Reply with exactly: PONG")
            .await
            .expect("submit");

        let mut got_init = false;
        let mut text = String::new();
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), rx.recv()).await {
                Ok(Ok(ev)) => match ev {
                    ConversationEvent::SessionInit { session_id, .. } => {
                        assert!(!session_id.is_empty());
                        got_init = true;
                    }
                    ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                    ConversationEvent::TurnCompleted { .. } => {
                        got_done = true;
                        break;
                    }
                    ConversationEvent::Error { message } => panic!("engine error: {message}"),
                    _ => {}
                },
                _ => break,
            }
        }
        host.stop().await.ok();

        assert!(got_init, "SessionInit を受信");
        assert!(got_done, "TurnCompleted を受信");
        assert!(
            text.to_uppercase().contains("PONG"),
            "本文に PONG を含む: {text:?}"
        );
    }

    /// 実機統合（HITL 質問レール、doc 35 PR1）: AskUserQuestion を誘発する prompt を投入し、
    /// 逆方向 can_use_tool → [`ConversationEvent::Question`] を受信 → [`ClaudeHost::respond_permission`]
    /// で回答 → turn が継続して TurnCompleted に到達することを確認する。
    /// `cargo test -p vantage-point --ignored conversation_host_question_roundtrip`（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn conversation_host_question_roundtrip() {
        use std::time::Duration;

        let tmp = tempfile::tempdir().expect("tempdir");
        let host = ClaudeHost::spawn(ClaudeHostConfig {
            cwd: tmp.path().to_string_lossy().to_string(),
            repo: "vp-test".to_string(),
            lane: "spike-q".to_string(),
            lane_label: "spike-q".to_string(),
            session_key: 1,
            resume_session_id: None,
            model: Some("haiku".to_string()),
            claude_cli_path: None,
        })
        .expect("spawn host");

        let mut rx = host.subscribe();
        // spike E の question prompt と同型: AskUserQuestion を 1 問誘発して待たせる。
        host.submit(
            "I want you to greet me, but I have not decided the language. Use the AskUserQuestion \
             tool to ask whether the greeting should be in English or Japanese. Ask exactly one \
             question with those two options, then wait for my answer. Do not guess.",
        )
        .await
        .expect("submit");

        let mut got_question = false;
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), rx.recv()).await {
                Ok(Ok(ConversationEvent::Question {
                    request_id,
                    questions,
                })) => {
                    got_question = true;
                    assert!(!questions.is_empty(), "questions が空でない");
                    let first = &questions[0];
                    assert!(!first.options.is_empty(), "選択肢がある");
                    // 最初の選択肢の label で回答（answers map = {question: label}）。
                    let answers = serde_json::json!({
                        first.question.clone(): first.options[0].label.clone(),
                    });
                    host.respond_permission(
                        &request_id,
                        PermissionDecision::Allow {
                            answers: Some(answers),
                        },
                    )
                    .await
                    .expect("respond_permission");
                }
                Ok(Ok(ConversationEvent::TurnCompleted { .. })) => {
                    got_done = true;
                    break;
                }
                Ok(Ok(ConversationEvent::Error { message })) => panic!("engine error: {message}"),
                Ok(Ok(_)) => {}
                _ => break,
            }
        }

        assert!(got_question, "Question event を受信");
        assert!(got_done, "回答後 turn が継続して TurnCompleted に到達");
    }
}
