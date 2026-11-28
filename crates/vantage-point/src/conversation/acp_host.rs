//! AcpAgentHost — ACP engine（grok / opencode / agy / muse）を lane 単位で**常駐**駆動する host（gui、doc 42・43・88・91）
//!
//! `grok agent stdio`（doc 42）・`opencode acp`（doc 43）・`agy acp`（doc 88）はいずれも
//! **ACP（Agent Client Protocol、protocolVersion 1）**を実装している。接続・型・順序の管理と、
//! `session/update` の翻訳、設定面、履歴の控えは、共有 crate `agent-chat-acp` の
//! [`AcpClientHost`]（公式 SDK `agent-client-protocol`）が持つ（agent-chat 設計書 01、VP design 86）。
//! 2026-11-20 に、VP の手書きの host（JSON-RPC を手で組んでいた）からこれに切り替えた。
//!
//! 本 module は VP 固有のものを注入する薄い adapter:
//!
//! - **どの CLI をどう起動するか**（[`AcpEngine`] — CLI の path、引数、名乗り）と identity env
//!   （`VP_REPO` / `VP_LANE` / `VP_SESSION_KEY`、doc 51 §1 A3b）
//! - **会話 id の永続**: 会話が確定したら registry に書く（doc 40 §4 — grok / opencode / agy は
//!   registry-native で、旧 store が最初から無い）
//! - **`session/load` に失敗した時の方針**: 新規の会話にする（[`LoadFallback::NewSession`]）。
//!   新しい id が registry を上書きする。手書きの host と同じ動き
//! - **permission の方針**: 自動で許可する（claude bypassPermissions / codex approvalPolicy=never と
//!   同じ tui/gui parity。grok は spawn flag `--always-approve`、agy は
//!   `--dangerously-skip-permissions` と二段防御、opencode は自動の許可の一段のみ — doc 43 §6・doc 88 §2）
//! - **設定の意図**: session の確定の後に当て直す `(config id, 値)`（agy は切り替えた値を
//!   `session/load` の後に保たない — doc 88 §6）
//!
//! 履歴: `session/load` が流す過去の会話は host が控えに持ち、pump が [`ReplaySource::attach`] で
//! 受け取って `ReplayStart` 〜 `ReplayEnd` で包んで配る（[`super::replay_source`]）。VP の
//! `replay_log` は ACP engine では使わない — console で進めた分も engine から戻る。
//!
//! stderr: 1 行ずつ daemon の log に出す（`on_stderr`）。末尾は共有 host が持ち、終了の文に付く。
//!
//! 途絶の自己修復: engine が終了した後の `submit` は Err を返し、`ensure_and_submit_chat` が
//! drop → 再 ensure → retry する。chat を開き直した時は、終了した host（[`AcpAgentHost::exited`]）を
//! pool が作り直す（agent-chat 設計書 01「呼び手の規則」— 終了した host はもう使えない）。
//!
//! 会話を確定できなかった時（認証切れ、CLI の不調など）は、共有 host が理由を `Error` で出してから
//! `EngineExited` を出す。engine が終了して、送られなかった発話があった時も `Error` が出る。
//! `EngineExited` の文（stderr の末尾つき）は GUI に出ない（「休眠」とだけ出る）ので、daemon の log
//! にも出す（[`watch_exit`]）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_chat_acp::{
    AcpClientHost, HostError, HostOptions, LaunchContext, LoadFallback, PermissionDecision,
    PromptImage,
};
use agent_client_protocol::v1::schema::RequestPermissionRequest;
use tokio::sync::broadcast;

use super::acp_config::{AcpConfigOption, AcpConfigView};
use super::event::ConversationEvent;
use super::host::{ImageInput, InFlight};
use super::replay_source::{AttachFuture, ReplayAttach, ReplayPort, ReplayRequester, ReplaySource};

/// AcpAgentHost が駆動する ACP engine の種別（spawn command + 名乗りの差分だけを持つ）。
///
/// host のロジックは engine 非依存（共有 crate）。engine 別に変わるのは「どの CLI をどう起動するか」と
/// 「registry / ログ / エラー文言の名乗り」だけなので、その差分をここに集約する（doc 43 §2・doc 88 §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpEngine {
    /// `grok agent stdio --always-approve`（xAI Grok CLI、doc 42）。CLI パス = PATH → `~/.grok/bin/grok`。
    Grok,
    /// `opencode acp`（opencode、doc 43）。CLI パス = PATH → `/opt/homebrew/bin/opencode`（brew）。
    /// approve 相当 flag は無い（permission の自動の許可の一段のみ）。model / provider は
    /// opencode config が SSOT で、VP は spawn に env も flag も注入しない（doc 43 §3）。
    OpenCode,
    /// `agy acp --dangerously-skip-permissions`（Google Antigravity CLI、doc 88）。
    /// CLI パス = PATH → `/opt/homebrew/bin/agy`（brew）。permission 二段防御（flag + 自動の許可）。
    Agy,
    /// Meta Muse Code (`muse acp`), PATH then ~/.local/bin/muse.
    Muse,
}

impl AcpEngine {
    /// registry / ログ / エラー文言の名乗り（[`super::EngineKind::agent_name`] と一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
            Self::Agy => "agy",
            Self::Muse => "muse",
        }
    }

    /// CLI 実行パスの解決（launchd 起動 daemon の細い PATH 対策 — `session_store::resolve_cli`）。
    /// usage meter の probe（`usage::probe`）も同じ解決を使う。
    pub(crate) fn cli_path(self) -> String {
        match self {
            Self::Muse => {
                let home = std::env::var("HOME").unwrap_or_default();
                crate::lane::session_store::resolve_cli(
                    "muse",
                    &[PathBuf::from(home).join(".local/bin/muse")],
                )
            }
            Self::Grok => {
                let home = std::env::var("HOME").unwrap_or_default();
                crate::lane::session_store::resolve_cli(
                    "grok",
                    &[PathBuf::from(home).join(".grok/bin/grok")],
                )
            }
            Self::OpenCode => crate::lane::session_store::resolve_cli(
                "opencode",
                &[PathBuf::from("/opt/homebrew/bin/opencode")],
            ),
            Self::Agy => crate::lane::session_store::resolve_cli(
                "agy",
                &[PathBuf::from("/opt/homebrew/bin/agy")],
            ),
        }
    }

    /// spawn 時の固定引数（cwd / stdio piping は共有 host が付ける）。
    fn spawn_args(self) -> &'static [&'static str] {
        match self {
            // grok: agent サブコマンド + `--always-approve`（permission 二段防御、doc 42）。
            Self::Grok => &["agent", "--always-approve", "stdio"],
            // opencode: `acp` サブコマンドのみ（flag / env / config 注入なし — model は opencode
            //  config が SSOT、doc 43 §3）。
            Self::OpenCode => &["acp"],
            // agy: `acp` サブコマンド + `--dangerously-skip-permissions`（permission 二段防御、doc 88）。
            Self::Agy => &["acp", "--dangerously-skip-permissions"],
            Self::Muse => &["acp", "--disable-sandbox", "--trust-workspace"],
        }
    }
}

/// AcpAgentHost の起動設定。
#[derive(Debug, Clone)]
pub struct AcpHostConfig {
    /// 駆動する ACP engine（spawn command + 名乗りの差分）。
    pub engine: AcpEngine,
    /// 会話の作業ディレクトリ（lane の repo dir、絶対 path）。
    pub cwd: String,
    /// registry 書き込みキー（repo 名）。
    pub repo: String,
    /// registry 書き込みキー（session label: `main` / `main#2` …）。
    /// ⚠️ env の `VP_LANE` には使わない — そちらは [`Self::lane_label`]（素の label）。
    pub lane: String,
    /// identity env（`VP_LANE`）用の素の lane label（doc 51 §1 A3b — tui と同じ契約）。
    pub lane_label: String,
    /// identity env（`VP_SESSION_KEY`）用の session key（doc 40 §4 の hook identity と同じ）。
    pub session_key: crate::lane::session_registry::SessionKey,
    /// 再開する ACP sessionId（doc 40 registry の `conversation`）。None = 新規 session。
    pub session_id: Option<String>,
    /// session 確定後に `set_config_option` で当て直す `(config id, 値)` 列（registry の意図、
    /// doc 88 §6）。agy は切り替えた値を `session/load` の後に保たない（実測）ので、spawn の
    /// たびに戻す。engine 別の意図の形は pool が engine settings から組む（host は engine 非依存）。
    pub config_intent: Vec<(String, String)>,
}

/// 起動の文脈（どの CLI を、どの引数・cwd・env で起動するか）。純関数。
fn launch_context(config: &AcpHostConfig) -> LaunchContext {
    LaunchContext {
        program: PathBuf::from(config.engine.cli_path()),
        args: config
            .engine
            .spawn_args()
            .iter()
            .map(|arg| (*arg).to_string())
            .collect(),
        cwd: PathBuf::from(&config.cwd),
        // identity env（doc 51 §1 A3b）: engine（とその shell tool の子）が `vp now` /
        // wire で自分を名乗る口。tui の agent_spawner / claude host と同じ契約。
        env: vec![
            ("VP_REPO".to_string(), config.repo.clone()),
            ("VP_LANE".to_string(), config.lane_label.clone()),
            ("VP_SESSION_KEY".to_string(), config.session_key.to_string()),
        ],
        env_remove: crate::spawn_env::inherited_agent_host_env_to_strip(),
    }
}

/// permission の方針: 自動で許可する（bypass parity、doc 42 §2）。
///
/// 許可の option（`allow_once` → `allow_always`）を選ぶ。許可の option が無ければ先頭の option。
///
/// 手書きの host（`build_permission_allow`）との関係: 「無ければ先頭」は同じ（共有 crate の既定は
/// 「無ければ取り消す」）。違いは 2 つ — 許可の option が複数ある時、手書きは**並びの先頭**の
/// allow を選んでいたが、今は kind の順で狭い方（`allow_once`）を選ぶ。option が 1 つも無い時、
/// 手書きは `optionId: null` の「選んだ」を返していたが、今は取り消す。
fn auto_allow(request: &RequestPermissionRequest) -> PermissionDecision {
    match agent_chat_acp::permission::allow(request) {
        PermissionDecision::Cancelled => request
            .options
            .first()
            .map_or(PermissionDecision::Cancelled, |option| {
                PermissionDecision::Select(option.option_id.to_string())
            }),
        selected => selected,
    }
}

/// 共有 host への注入（会話の紐付け・永続・permission・設定の意図）。
fn host_options(config: &AcpHostConfig) -> HostOptions {
    let engine = config.engine;
    let repo = config.repo.clone();
    let lane = config.lane.clone();
    HostOptions {
        resume: config.session_id.clone(),
        // load 空振り → 新規の会話（新 sessionId が registry を上書き）。手書きの host と同じ。
        on_load_failed: LoadFallback::NewSession,
        config_intent: config.config_intent.clone(),
        permission: Arc::new(auto_allow),
        // stderr は daemon の log に 1 行ずつ出す（auth の失効の前触れ、provider の接続失敗など、
        // engine が生きたまま書く警告を拾う）。末尾は共有 host が持ち、終了の文に付く。
        on_stderr: Arc::new(move |line| tracing::warn!("{} stderr: {line}", engine.name())),
        // doc 40 §4: registry 直結（grok / opencode / agy は registry-native — 旧 store が最初から無い）。
        on_session: Arc::new(move |session_id| {
            let (lane_label, key) = crate::lane::session_registry::parse_session_label(&lane);
            crate::lane::session_registry::set_conversation(
                &repo,
                lane_label,
                engine.name(),
                key,
                Some(session_id),
            )
            .map(|_| ())
            .map_err(|e| {
                tracing::warn!(
                    "{} sessionId の registry 記録失敗（repo={repo}, lane={lane}）: {e}",
                    engine.name()
                );
                e.to_string()
            })
        }),
        ..HostOptions::default()
    }
}

/// lane 単位の常駐 ACP host（gui、doc 42・43・88・91）。grok / opencode / agy / muse の 4 engine を
/// [`AcpEngine`] パラメータ（cli_path / spawn_args / name）で共有する。
pub struct AcpAgentHost {
    engine: AcpEngine,
    /// log 用の名乗り（`repo=…, lane=…`）。
    whom: String,
    host: Arc<AcpClientHost>,
    grok: Arc<super::grok_settings::GrokConfigState>,
    replay: ReplayRequester,
    /// pump に渡す口（pump の起動時に 1 度だけ取り出す）。
    port: Mutex<Option<ReplayPort>>,
}

impl AcpAgentHost {
    /// engine の ACP プロセス（grok = `grok agent --always-approve stdio` / opencode =
    /// `opencode acp` / agy = `agy acp --dangerously-skip-permissions`）を起動し、
    /// handshake（initialize → session/load|new）を開始する。
    /// 完了前の submit は queue に積まれ自動送出される。tokio の runtime の中で呼ぶ。
    pub fn spawn(config: AcpHostConfig) -> anyhow::Result<Self> {
        let engine = config.engine;
        let host = AcpClientHost::spawn(launch_context(&config), host_options(&config))
            .map_err(|e| spawn_error(engine, e))?;
        tracing::info!(
            "AcpAgentHost spawn（常駐 {} ACP、repo={}, lane={}, resume={}）",
            engine.name(),
            config.repo,
            config.lane,
            config.session_id.as_deref().unwrap_or("new"),
        );
        Ok(Self::wrap(&config, host))
    }

    /// 接続の作り方を差し替えて起動する（テストが偽の agent をつなぐ）。
    #[cfg(test)]
    pub(crate) fn spawn_with<C: agent_chat_acp::Connector>(
        config: AcpHostConfig,
        connector: C,
    ) -> anyhow::Result<Self> {
        let engine = config.engine;
        let host =
            AcpClientHost::spawn_with(connector, PathBuf::from(&config.cwd), host_options(&config))
                .map_err(|e| spawn_error(engine, e))?;
        Ok(Self::wrap(&config, host))
    }

    fn wrap(config: &AcpHostConfig, host: AcpClientHost) -> Self {
        let engine = config.engine;
        let host = Arc::new(host);
        let (replay, requests) = ReplayRequester::channel();
        // log に「どの lane の engine か」を残す（同じ engine が複数の lane で動く）。
        let whom = format!("repo={}, lane={}", config.repo, config.lane);
        tokio::spawn(watch_exit(engine, whom.clone(), host.subscribe()));
        let grok = Arc::default();
        let handle = AcpConfigHandle {
            engine,
            host: Arc::clone(&host),
            grok: Arc::clone(&grok),
        };
        Self {
            engine,
            whom,
            replay,
            port: Mutex::new(Some(ReplayPort {
                source: Arc::new(AcpReplaySource(handle)),
                requests,
            })),
            host,
            grok,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.host.subscribe()
    }

    /// host が終了したか（process の終了、接続が切れた、会話を確定できなかった、[`Self::stop`]）。
    ///
    /// 終了した host は履歴も渡せず、発話も受けない。chat を開き直す時に pool がこれを見て作り直す。
    /// 立て直し（[`Self::restart`]）の途中は false。
    ///
    /// `stop()` で止めた host でも true になるが、止めるのは slot を畳む時だけ
    /// （[`super::engine::ChatEngineSlot`] の Drop が唯一の呼び手）なので、pool が見る slot に
    /// 止めた host が残ることは無い。
    pub fn exited(&self) -> bool {
        self.host.is_finished()
    }

    /// pump に渡す履歴の口。1 度だけ取り出せる（2 度目は None）。
    pub fn take_replay_port(&self) -> Option<ReplayPort> {
        self.port
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// 履歴の配り直しを pump に頼む（chat を開いた時）。pump が居なければ false。
    pub fn request_replay(&self) -> bool {
        self.replay.request()
    }

    /// engine の設定面（roster の `acp_config` の供給源、doc 88 §6）。
    pub fn config_view(&self) -> Option<AcpConfigView> {
        self.host.config_view().map(|v| {
            let mut view = AcpConfigView::from(v);
            if self.engine == AcpEngine::Grok {
                let feedback = self.grok.feedback.lock().unwrap();
                view.notice = feedback.notice.clone();
                if feedback.error.is_some() {
                    view.error.clone_from(&feedback.error);
                }
                drop(feedback);
                if view.options.iter().any(|o| {
                    o.id == "reasoning_effort"
                        && !o.choices.iter().any(|c| c.value == o.current_value)
                }) {
                    let handle = self.config_handle();
                    // roster を待たせずに観測時の正規化。結果は次の snapshot に載る。
                    tokio::spawn(async move {
                        let _ = handle.normalize_grok().await;
                    });
                }
            }
            view
        })
    }

    /// user の設定切替の口（pool の lock を離してから応答を待つため、host の外へ clone して持ち出す）。
    pub fn config_handle(&self) -> AcpConfigHandle {
        AcpConfigHandle {
            engine: self.engine,
            host: Arc::clone(&self.host),
            grok: Arc::clone(&self.grok),
        }
    }

    pub fn session_init(&self) -> Option<ConversationEvent> {
        self.host.session_init()
    }

    /// transcript に載っていない増分。ACP engine は transcript を使わない（履歴は host の控えから
    /// 配る）ので、常に空。読み手は Claude の transcript replay だけ。
    pub fn in_flight(&self) -> InFlight {
        InFlight::default()
    }

    /// [`Self::in_flight`] の commit 世代。ACP engine では意味を持たない。
    pub fn commit_seq(&self) -> u64 {
        0
    }

    /// engine の process id（roster の `LaneInfo.pid`）。立て直しで替わり、engine が終了したら None。
    pub fn pid(&self) -> Option<u32> {
        self.host.pid()
    }

    /// ユーザープロンプトを投入する（idle なら即 session/prompt、それ以外は queue）。
    /// 途絶時は Err（`ensure_and_submit_chat` の自己修復に繋ぐ — doc 42 冒頭）。
    pub async fn submit(&self, prompt: &str) -> anyhow::Result<()> {
        self.submit_with_images(prompt, &[]).await
    }

    /// 画像を添えて投入する（ACP の image content block、doc 88 §6）。画像を運べるかは
    /// engine の能力（[`super::EngineKind::image_capable`]）で、呼び手（ChatHost）が gate する。
    pub async fn submit_with_images(
        &self,
        prompt: &str,
        images: &[ImageInput],
    ) -> anyhow::Result<()> {
        let images: Vec<PromptImage> = images
            .iter()
            .map(|image| PromptImage {
                media_type: image.media_type.clone(),
                data: image.data_base64.clone(),
            })
            .collect();
        self.host.submit(prompt, &images).map_err(|e| {
            anyhow::anyhow!(
                "{} へ送れません（engine 再起動で復旧）: {e}",
                self.engine.name()
            )
        })
    }

    /// 実行中 turn を中断する（`session/cancel` notification）。engine が答えれば process は止めない。
    /// prompt response が stopReason=cancelled で返り、通常の完了経路で streaming が畳まれる。
    ///
    /// engine が中断に答えない時（agy が thinking のまま固まった、2026-11-24）は、共有 host が
    /// 待ち切り（`HostOptions::cancel_grace`、既定 3 秒）で process を止めて終了し（[`Self::exited`]）、
    /// `TurnCompleted` → `EngineExited` を出す（stderr の読み切りを最大 1 秒待つので、中断から 3〜4 秒）。
    /// 次の submit で pool が作り直し、registry の会話 id で `session/load` する（止めた engine の会話を
    /// load し直せるかは実機で確かめていない。load に失敗すると `LoadFallback::NewSession` で新しい
    /// 会話になる）。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        self.host
            .interrupt()
            .map_err(|e| anyhow::anyhow!("{} を中断できません: {e}", self.engine.name()))
    }

    /// process を立て直して load し直す（agent-chat 設計書 01「呼び手の規則」）。
    ///
    /// 他の書き手（console）が会話を進めた後、次の発話の前に呼ぶための口。生きている engine が
    /// 他の書き手の分を知っているかは engine による（実測: agy 1.5.11 は知らず、保存からも消える。
    /// grok 1.0.89 は 1 回の測定で答えなかった。opencode 2.3.0 は知っていた — doc 88 §8.4）。
    /// turn の最中は Err。
    /// 成功したら履歴を配り直す。
    ///
    /// ⚠️ **呼び手は無い**。VP は console に切り替える時に chat の engine を止めるので
    /// （`repo::lane::reconcile` の ①）、生きたまま他の書き手に進められる場面が無い（doc 88 §8.4）。
    pub async fn restart(&self) -> Result<(), String> {
        self.host.restart().await.map_err(|e| e.to_string())?;
        self.replay.request();
        Ok(())
    }

    /// engine の構造化質問（ACP `elicitation/create` の form）に答える。
    ///
    /// 質問は共有 host が `ConversationEvent::Question` として出し、PromptCard の回答がここへ戻る
    /// （claude の AskUserQuestion と同じレール）。`Allow { answers }` は `accept` + content、
    /// `Deny` は `cancel`（ACP の elicitation に拒否理由を運ぶ欄は無いので `message` は捨てる）。
    /// 未知・使用済みの `request_id` と、engine が終了した後は Err。
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: super::host::PermissionDecision,
    ) -> anyhow::Result<()> {
        let (answers, cancel) = match decision {
            super::host::PermissionDecision::Allow { answers } => (answers, false),
            super::host::PermissionDecision::Deny { .. } => (None, true),
        };
        self.host
            .respond_permission(request_id, answers, cancel)
            .await
            .map_err(|e| anyhow::anyhow!("{} の質問に答えられません: {e}", self.engine.name()))
    }

    /// 明示 teardown（[`super::engine::ChatEngineSlot`] Drop から呼ぶ）。会話の確定や設定の当て直しを
    /// 待っている最中でも、すぐに process を止める。先に積んであった発話は送らない。
    pub fn stop(&mut self) {
        self.host.stop();
        tracing::info!("AcpAgentHost stop（{}、{}）", self.engine.name(), self.whom);
    }
}

/// 起動の失敗の文。共有 host の文（「engine を起動できません: …」）の中身だけを取り出して、
/// engine の名前を 1 回だけ付ける。
fn spawn_error(engine: AcpEngine, error: HostError) -> anyhow::Error {
    let detail = match error {
        HostError::Spawn(detail) => detail,
        other => other.to_string(),
    };
    anyhow::anyhow!("{} を起動できません: {detail}", engine.name())
}

/// engine の終了の理由を daemon の log に残す。
///
/// 理由（stderr の末尾つき）は `EngineExited` の文にしか載らず、GUI はその文を出さない（「休眠」と
/// だけ出る）。受け口だけを持つ（host を握らない）ので、host が落とされれば終わる。
async fn watch_exit(
    engine: AcpEngine,
    whom: String,
    mut events: broadcast::Receiver<ConversationEvent>,
) {
    loop {
        match events.recv().await {
            Ok(ConversationEvent::EngineExited { message }) => {
                tracing::warn!("{} が終了しました（{whom}）: {message}", engine.name());
                return;
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// pump に渡す履歴の出どころ（共有 host の `attach()`）。
struct AcpReplaySource(AcpConfigHandle);

impl ReplaySource for AcpReplaySource {
    fn attach(&self) -> AttachFuture {
        let handle = self.0.clone();
        Box::pin(async move {
            let (history, rx) = handle.host.attach().await.map_err(|e| e.to_string())?;
            let _ = handle.normalize_grok().await;
            Ok(ReplayAttach {
                events: history.events,
                in_flight: history.in_flight,
                truncated: history.truncated,
                rx,
            })
        })
    }

    fn session_init(&self) -> Option<ConversationEvent> {
        self.0.host.session_init()
    }
}

/// user の設定切替の口（[`AcpAgentHost::config_handle`]）。host を共有する薄い clone。
///
/// pool の lock を握ったまま engine の応答を待たないために、host から切り離して持ち出す。
#[derive(Clone)]
pub struct AcpConfigHandle {
    engine: AcpEngine,
    host: Arc<AcpClientHost>,
    grok: Arc<super::grok_settings::GrokConfigState>,
}

impl AcpConfigHandle {
    pub(super) fn options(&self) -> Option<Vec<AcpConfigOption>> {
        self.host
            .config_view()
            .map(|v| v.options.into_iter().map(Into::into).collect())
    }

    /// Grok の候補外 effort を一度だけそろえる。
    pub(crate) async fn normalize_grok(&self) -> Result<(), String> {
        if self.engine != AcpEngine::Grok {
            return Ok(());
        }
        self.grok.normalize(self).await
    }

    /// Grok の model → effort の適用を host 単位で直列化する。
    pub(crate) async fn apply_grok(
        &self,
        desired: &super::grok_settings::GrokSettings,
    ) -> Result<(), String> {
        if self.engine != AcpEngine::Grok {
            return Err("grok 以外の host には適用できません".into());
        }
        self.grok.apply(self, desired).await
    }

    /// `session/set_config_option` を送り、engine の応答まで待つ（上限は共有 host の
    /// `config_timeout`、既定 15 秒）。
    ///
    /// 成功で configOptions（候補を含む全体）が更新され、それを返す（model を替えると他の軸の
    /// 候補が変わるため、呼び手が意図を整えるのに使う）。engine が断った時は engine のエラー文を
    /// 返し、roster の `acp_config.error` にも残る。応答が上限までに来なかった時は Err を返すだけで、
    /// `acp_config.error` には残らない（手書きの host は残していた）。遅れて届いた応答は共有 host が
    /// 設定面と意図に取り込む。永続（registry）は呼び手の仕事（engine 別の形）。
    pub async fn set(&self, config_id: &str, value: &str) -> Result<Vec<AcpConfigOption>, String> {
        self.host
            .set_config(config_id, value)
            .await
            .map(|options| options.into_iter().map(Into::into).collect())
            .map_err(|e| format!("{}: {e}", self.engine.name()))
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;

    fn config(engine: AcpEngine) -> AcpHostConfig {
        AcpHostConfig {
            engine,
            cwd: "/tmp/vp-acp".into(),
            repo: "vptest-acp".into(),
            lane: "lead#2".into(),
            lane_label: "lead".into(),
            session_key: 2,
            session_id: Some("sess-1".into()),
            config_intent: vec![("model".into(), "gemini-3.8-flash".into())],
        }
    }

    /// AcpEngine ごとに spawn_args / name が変わる（host 共有の engine 差分）。grok =
    /// `agent --always-approve stdio`（二段防御）、opencode = `acp` のみ
    /// （flag 注入なし — model は opencode config が SSOT）、agy = `acp --dangerously-skip-permissions`
    /// （permission 二段防御）。名乗りは registry / ログ / エラー文言に載る agent 名（EngineKind::agent_name と一致）。
    #[test]
    fn acp_engine_spawn_args_and_name_are_engine_specific() {
        assert_eq!(
            AcpEngine::Grok.spawn_args(),
            &["agent", "--always-approve", "stdio"],
            "grok は agent サブコマンド + 二段防御 flag"
        );
        assert_eq!(
            AcpEngine::OpenCode.spawn_args(),
            &["acp"],
            "opencode は acp サブコマンドのみ（flag なし — doc 43 §3）"
        );
        assert_eq!(
            AcpEngine::Agy.spawn_args(),
            &["acp", "--dangerously-skip-permissions"],
            "agy は acp サブコマンド + 二段防御 flag（doc 88 §2）"
        );
        // agent 名（EngineKind）と registry / ログの名乗り（AcpEngine）が一致する不変条件。
        assert_eq!(
            AcpEngine::Grok.name(),
            super::super::EngineKind::Grok.agent_name()
        );
        assert_eq!(
            AcpEngine::OpenCode.name(),
            super::super::EngineKind::OpenCode.agent_name()
        );
        assert_eq!(
            AcpEngine::Agy.name(),
            super::super::EngineKind::Agy.agent_name()
        );
    }

    /// 起動の文脈: 引数は engine の表、cwd は lane の dir、identity env は 3 つ。
    /// `VP_LANE` は素の lane label（session label `lead#2` ではない）。
    /// 落ちるべき壊し方: `VP_LANE` に `config.lane`（session label）を渡す。
    #[test]
    fn launch_context_carries_engine_args_cwd_and_identity_env() {
        let launch = launch_context(&config(AcpEngine::Agy));
        assert_eq!(launch.args, ["acp", "--dangerously-skip-permissions"]);
        assert_eq!(launch.cwd, PathBuf::from("/tmp/vp-acp"));
        assert_eq!(
            launch.env,
            [
                ("VP_REPO".to_string(), "vptest-acp".to_string()),
                ("VP_LANE".to_string(), "lead".to_string()),
                ("VP_SESSION_KEY".to_string(), "2".to_string()),
            ]
        );
    }

    /// 注入: 再開する id、load に失敗したら新規、設定の意図。
    /// 落ちるべき壊し方: `on_load_failed` を既定（`Fail`）のままにする — 消えた会話 id を持つ
    /// session で chat が開けなくなる。
    #[test]
    fn host_options_resume_fall_back_to_new_session_and_carry_intent() {
        let options = host_options(&config(AcpEngine::Agy));
        assert_eq!(options.resume.as_deref(), Some("sess-1"));
        assert_eq!(options.on_load_failed, LoadFallback::NewSession);
        assert_eq!(
            options.config_intent,
            [("model".to_string(), "gemini-3.8-flash".to_string())]
        );
    }

    fn permission_request(options: serde_json::Value) -> RequestPermissionRequest {
        serde_json::from_value(serde_json::json!({
            "sessionId": "s",
            "toolCall": {"toolCallId": "t1"},
            "options": options,
        }))
        .expect("permission の要求")
    }

    /// permission は自動で許可する。許可の option が無ければ先頭（手書きの host と同じ）。
    /// 落ちるべき壊し方: 共有 crate の既定（`permission::allow`）をそのまま渡す — 許可の option を
    /// 出さない engine で、tool の実行が取り消される。
    #[test]
    fn permission_policy_allows_and_falls_back_to_the_first_option() {
        // 注入した方針そのものを呼ぶ（`auto_allow` を直接呼ぶと、注入を外した壊し方で落ちない）。
        let policy = host_options(&config(AcpEngine::OpenCode)).permission;
        assert_eq!(
            policy(&permission_request(serde_json::json!([
                {"optionId": "rej", "name": "No", "kind": "reject_once"},
                {"optionId": "yes", "name": "Yes", "kind": "allow_once"},
            ]))),
            PermissionDecision::Select("yes".into())
        );
        assert_eq!(
            policy(&permission_request(serde_json::json!([
                {"optionId": "first", "name": "No", "kind": "reject_once"},
            ]))),
            PermissionDecision::Select("first".into())
        );
        assert_eq!(
            policy(&permission_request(serde_json::json!([]))),
            PermissionDecision::Cancelled
        );
    }

    use super::testing::{FakeAgent, Script, agent_text, user_text};

    fn fake_config(engine: AcpEngine, repo: &str, resume: Option<&str>) -> AcpHostConfig {
        AcpHostConfig {
            engine,
            cwd: "/tmp/vp-acp".into(),
            repo: repo.into(),
            lane: "lead".into(),
            lane_label: "lead".into(),
            session_key: 1,
            session_id: resume.map(str::to_string),
            config_intent: Vec::new(),
        }
    }

    // mem_1ChKFMEwUYWj5Qs6VLVkU4: 8×8 は Grok に弾かれたため、実測済みの 256×256 を使う。
    fn red_image_256() -> ImageInput {
        use base64::Engine as _;
        let image = image::RgbImage::from_pixel(256, 256, image::Rgb([255, 0, 0]));
        let mut png = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG");
        ImageInput {
            media_type: "image/png".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
        }
    }

    #[test]
    fn muse_launch_contract() {
        assert_eq!(AcpEngine::Muse.name(), "muse");
        assert_eq!(
            launch_context(&config(AcpEngine::Muse)).args,
            ["acp", "--disable-sandbox", "--trust-workspace"]
        );
    }

    // mem_1ChTGsc5BoJHRTqsjN5M4p: real Muse is never launched by unit tests.
    #[tokio::test]
    async fn muse_acp_new_load_intent_images_and_replay() {
        use crate::conversation::muse_settings::MuseSettings;
        let _state = crate::test_env::state_dir_async().await;
        let intent = MuseSettings {
            model: Some("muse-spark-1.3".into()),
            mode: Some("allowAll".into()),
        };
        for resume in [None, Some("01a21c8b-8008-7091-aa5d-2dafd79b8b0b")] {
            let (agent, connector) = FakeAgent::new(Script {
                config: serde_json::from_value(serde_json::json!([
                    {"id":"model","name":"Model","type":"select","currentValue":"muse-wonder-1.0",
                     "options":[{"value":"muse-wonder-1.0","name":"muse-wonder-1.0"},{"value":"muse-spark-1.3","name":"muse-spark-1.3"}]},
                    {"id":"mode","name":"Mode","type":"select","currentValue":"onRequest",
                     "options":[{"value":"onRequest","name":"onRequest"},{"value":"allowAll","name":"allowAll"}]}
                ]))
                .unwrap(),
                replay: vec![user_text("earlier prompt"), agent_text("earlier answer")],
                ..Script::default()
            });
            let mut cfg = fake_config(AcpEngine::Muse, "vptest-muse-acp", resume);
            cfg.config_intent = intent.config_pairs();
            let adapter = AcpAgentHost::spawn_with(cfg, connector).unwrap();
            let mut host = super::super::ChatHost::Muse(adapter);
            let replay = host
                .take_replay_port()
                .unwrap()
                .source
                .attach()
                .await
                .unwrap();
            if resume.is_some() {
                assert!(replay.events.iter().any(|event| matches!(event,
                    ConversationEvent::UserMessage { text } if text == "earlier prompt")));
            }
            let view = host.acp_config().expect("roster config");
            assert_eq!(
                view.options
                    .iter()
                    .map(|o| (o.id.as_str(), o.current_value.as_str()))
                    .collect::<Vec<_>>(),
                [("model", "muse-spark-1.3"), ("mode", "allowAll")]
            );
            let image = red_image_256();
            host.submit_with_images("Describe.", std::slice::from_ref(&image))
                .await
                .unwrap();
            agent.wait_for("session/prompt").await;
            {
                let log = agent.log.lock().unwrap();
                let configs: Vec<_> = log
                    .iter()
                    .filter(|(m, _)| m == "session/set_config_option")
                    .map(|(_, p)| {
                        (
                            p["configId"].as_str().unwrap(),
                            p["value"].as_str().unwrap(),
                        )
                    })
                    .collect();
                assert_eq!(configs, [("model", "muse-spark-1.3"), ("mode", "allowAll")]);
                assert_eq!(
                    log[1].0,
                    if resume.is_some() {
                        "session/load"
                    } else {
                        "session/new"
                    }
                );
                let prompt = &log.iter().find(|(m, _)| m == "session/prompt").unwrap().1["prompt"];
                assert_eq!(
                    *prompt,
                    serde_json::json!([
                        {"type":"text","text":"Describe."},
                        {"type":"image","mimeType":"image/png","data":image.data_base64}
                    ])
                );
            }
            let registry = crate::lane::session_registry::load("vptest-muse-acp", "lead", "muse");
            assert_eq!(
                registry.sessions[0].conversation.as_deref(),
                Some(resume.unwrap_or("new-1"))
            );
            host.stop();
        }
    }

    /// ChatHost の分岐から ACP の実際の要求まで画像を運ぶ。能力表だけ true にしても落ちる。
    #[tokio::test]
    async fn grok_chat_host_sends_image_content_block() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script::default());
        let adapter = AcpAgentHost::spawn_with(
            fake_config(AcpEngine::Grok, "vptest-grok-image", None),
            connector,
        )
        .expect("起動");
        let mut host = super::super::ChatHost::Grok(adapter);
        let image = red_image_256();
        host.submit_with_images("Describe the image.", std::slice::from_ref(&image))
            .await
            .expect("送信");
        agent.wait_for("session/prompt").await;
        let prompt = agent
            .log
            .lock()
            .unwrap()
            .iter()
            .find(|(method, _)| method == "session/prompt")
            .expect("prompt")
            .1["prompt"]
            .clone();
        host.stop();
        assert_eq!(
            prompt,
            serde_json::json!([
                {"type": "text", "text": "Describe the image."},
                {"type": "image", "mimeType": "image/png", "data": image.data_base64}
            ]),
            "Grok へ text と image の両方を渡す"
        );
    }

    /// agy / grok の構造化質問（ACP `elicitation/create`）が PromptCard のレールに乗る:
    /// engine の form → `Question` → ChatHost::respond_permission → `CreateElicitationResponse`
    /// （Allow = accept + content / Deny = cancel）→ turn が完了する。ChatHost の分岐を通すので、
    /// engine.rs の ACP 枝が bail のままだと落ちる。
    #[tokio::test]
    async fn elicitation_reaches_question_and_answer_returns_to_engine() {
        use super::super::host::PermissionDecision as Decision;
        let _state = crate::test_env::state_dir_async().await;
        for (engine, deny) in [(AcpEngine::Agy, false), (AcpEngine::Grok, true)] {
            let (agent, connector) = FakeAgent::new(Script::default());
            let adapter = AcpAgentHost::spawn_with(
                fake_config(engine, &format!("vptest-elicit-{}", engine.name()), None),
                connector,
            )
            .expect("起動");
            let mut events = adapter.subscribe();
            let mut host = match engine {
                AcpEngine::Agy => super::super::ChatHost::Agy(adapter),
                _ => super::super::ChatHost::Grok(adapter),
            };
            host.submit("ask").await.expect("送信");
            agent.wait_for("session/prompt").await;
            agent.elicit(serde_json::json!({
                "mode": "form",
                "message": "Choose",
                "requestedSchema": {"type": "object", "properties": {
                    "color": {"type": "string", "enum": ["red", "blue"]}
                }}
            }));
            let (request_id, question) =
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        if let ConversationEvent::Question {
                            request_id,
                            questions,
                        } = events.recv().await.expect("event")
                        {
                            return (request_id, questions[0].question.clone());
                        }
                    }
                })
                .await
                .expect("Question が出る");

            let decision = if deny {
                Decision::Deny {
                    message: "やめる".into(),
                }
            } else {
                Decision::Allow {
                    answers: Some(serde_json::json!({ question: "blue" })),
                }
            };
            host.respond_permission(&request_id, decision)
                .await
                .expect("回答を返せる");
            agent.wait_for("elicitation/response").await;
            let response = agent
                .log
                .lock()
                .unwrap()
                .iter()
                .find(|(method, _)| method == "elicitation/response")
                .expect("応答")
                .1["result"]
                .clone();
            if deny {
                assert_eq!(response["action"], "cancel", "Deny は cancel: {response}");
                assert!(response.get("content").is_none());
            } else {
                assert_eq!(response["action"], "accept", "Allow は accept: {response}");
                assert_eq!(response["content"], serde_json::json!({"color": "blue"}));
            }

            agent.end_turn();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !matches!(
                    events.recv().await.expect("event"),
                    ConversationEvent::TurnCompleted { .. }
                ) {}
            })
            .await
            .expect("turn が完了する");
            assert!(
                host.respond_permission(&request_id, Decision::Allow { answers: None })
                    .await
                    .is_err(),
                "使用済みの request_id は Err"
            );
            host.stop();
        }
    }

    #[tokio::test]
    async fn muse_chat_host_sends_image_content_block() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script::default());
        let adapter = AcpAgentHost::spawn_with(
            fake_config(AcpEngine::Muse, "vptest-muse-image", None),
            connector,
        )
        .expect("起動");
        let mut host = super::super::ChatHost::Muse(adapter);
        let image = red_image_256();
        host.submit_with_images("Describe the image.", std::slice::from_ref(&image))
            .await
            .expect("送信");
        agent.wait_for("session/prompt").await;
        let prompt = agent
            .log
            .lock()
            .unwrap()
            .iter()
            .find(|(method, _)| method == "session/prompt")
            .expect("prompt")
            .1["prompt"]
            .clone();
        host.stop();
        assert_eq!(
            prompt,
            serde_json::json!([
                {"type": "text", "text": "Describe the image."},
                {"type": "image", "mimeType": "image/png", "data": image.data_base64}
            ]),
            "Muse へ text と image の両方を渡す"
        );
    }

    /// opencode 2.3.0 が出す設定面の形（実測 2026-11-20、既定の model `lmstudio/…` の時:
    /// model / effort / mode の 3 軸。model を `opencode/big-pickle` に替えると effort の軸が消えて
    /// 2 軸になる — 軸の数は model による）。
    fn opencode_like_config() -> Vec<serde_json::Value> {
        serde_json::from_value(serde_json::json!([
            {"id": "model", "name": "Model", "type": "select",
             "currentValue": "lmstudio/qwen/qwen2.5-coder-14b",
             "options": [{"value": "lmstudio/qwen/qwen2.5-coder-14b", "name": "Qwen"}]},
            {"id": "effort", "name": "Effort", "type": "select", "currentValue": "default",
             "options": [{"value": "low", "name": "low"}, {"value": "default", "name": "default"}]},
            {"id": "mode", "name": "Mode", "type": "select", "currentValue": "build",
             "options": [{"value": "build", "name": "build"}, {"value": "plan", "name": "plan"}]}
        ]))
        .unwrap()
    }

    /// `session/load` が流した過去の会話が、履歴の口（pump が読む）から出る。会話 id は registry に
    /// 書かれ、engine が出した設定面は roster 用の view に載り、pid も読める。
    /// 落ちるべき壊し方: 手書きの host のように load の流れを捨てる — `events` が空になる
    /// （= console で進めた分が chat に出ない、の元の症状）。
    #[tokio::test]
    async fn load_replay_reaches_the_replay_port_with_registry_config_and_pid() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script {
            replay: vec![user_text("前の発話"), agent_text("前の答え")],
            config: opencode_like_config(),
            ..Script::default()
        });
        let host = AcpAgentHost::spawn_with(
            // opencode の会話 id は `ses_` + 英数字（registry が形式を検査する、doc 43 §1）。
            fake_config(
                AcpEngine::OpenCode,
                "vptest-acp-load",
                Some("ses_e097a9a7affeaFoxUCE8kUecOO"),
            ),
            connector,
        )
        .expect("起動");

        let port = host.take_replay_port().expect("履歴の口");
        assert!(host.take_replay_port().is_none(), "口は 1 度だけ取り出せる");
        let attached = port.source.attach().await.expect("attach");
        assert_eq!(
            attached.events,
            [
                ConversationEvent::UserMessage {
                    text: "前の発話".into()
                },
                ConversationEvent::MessageChunk {
                    text: "前の答え".into()
                },
            ]
        );
        assert!(!attached.in_flight && !attached.truncated);
        assert_eq!(agent.methods(), ["initialize", "session/load"]);

        // roster の `acp_config`: grok / opencode が出す設定面も、agy と同じく載る。
        let view = host.config_view().expect("設定面");
        assert_eq!(
            view.options
                .iter()
                .map(|o| (o.id.as_str(), o.current_value.as_str()))
                .collect::<Vec<_>>(),
            [
                ("model", "lmstudio/qwen/qwen2.5-coder-14b"),
                ("effort", "default"),
                ("mode", "build")
            ]
        );
        assert_eq!(host.pid(), Some(4242));

        let registry = crate::lane::session_registry::load("vptest-acp-load", "lead", "opencode");
        assert_eq!(
            registry.sessions[0].conversation.as_deref(),
            Some("ses_e097a9a7affeaFoxUCE8kUecOO")
        );
    }

    /// `session/load` に失敗したら新規の会話になり、新しい id が registry を上書きする
    /// （手書きの host と同じ動き）。履歴は空で、変えた事実が `Notice` で出る。
    /// 落ちるべき壊し方: `LoadFallback::Fail` のまま — attach が Err になり、chat が開けない。
    #[tokio::test]
    async fn failed_load_falls_back_to_a_new_session_and_overwrites_the_registry() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script {
            load_error: true,
            ..Script::default()
        });
        let host = AcpAgentHost::spawn_with(
            fake_config(AcpEngine::Grok, "vptest-acp-fallback", Some("gone")),
            connector,
        )
        .expect("起動");
        let mut events = host.subscribe();

        let attached = host
            .take_replay_port()
            .expect("履歴の口")
            .source
            .attach()
            .await
            .expect("新規の会話で確定する");
        assert!(attached.events.is_empty());
        assert_eq!(
            agent.methods(),
            ["initialize", "session/load", "session/new"]
        );
        let registry = crate::lane::session_registry::load("vptest-acp-fallback", "lead", "grok");
        assert_eq!(registry.sessions[0].conversation.as_deref(), Some("new-1"));
        let notice = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("Notice")
            .expect("broadcast");
        assert!(
            matches!(notice, ConversationEvent::Notice { .. }),
            "{notice:?}"
        );
    }

    /// engine が終了したら `exited()` が立つ（pool が chat を開き直す時に作り直す合図）。終了した
    /// host は履歴を渡せない。
    /// 落ちるべき壊し方: `exited()` が常に false を返す — 終了した host が slot に残り続け、開き直す
    /// たびに履歴の取得が失敗する。
    #[tokio::test]
    async fn exited_is_raised_when_the_engine_goes_away() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script::default());
        let host = AcpAgentHost::spawn_with(
            fake_config(AcpEngine::Grok, "vptest-acp-exit", None),
            connector,
        )
        .expect("起動");
        let port = host.take_replay_port().expect("履歴の口");
        port.source.attach().await.expect("会話は確定する");
        assert!(!host.exited());

        agent.close();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !host.exited() {
            assert!(tokio::time::Instant::now() < deadline, "exited が立たない");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            port.source.attach().await.is_err(),
            "終了した host は履歴を渡せない"
        );
    }

    /// user の設定切替は engine に届き、返った設定面（VP の wire 型）に現在値が出る。
    #[tokio::test]
    async fn config_handle_switches_an_axis_and_returns_the_wire_view() {
        let _state = crate::test_env::state_dir_async().await;
        let (_agent, connector) = FakeAgent::new(Script {
            config: opencode_like_config(),
            ..Script::default()
        });
        let host = AcpAgentHost::spawn_with(
            fake_config(AcpEngine::OpenCode, "vptest-acp-config", None),
            connector,
        )
        .expect("起動");
        let options = host
            .config_handle()
            .set("mode", "plan")
            .await
            .expect("切替");
        let mode = options.iter().find(|o| o.id == "mode").expect("mode の軸");
        assert_eq!(mode.current_value, "plan");
        assert_eq!(
            host.config_view()
                .expect("設定面")
                .options
                .iter()
                .find(|o| o.id == "mode")
                .map(|o| o.current_value.clone())
                .as_deref(),
            Some("plan")
        );
    }

    // ---- 実機 E2E（CI では走らない。実 CLI + auth + model の使用枠が要る）----
    //
    // engine ごとに 2 本（答えるだけの turn / file を 1 つ作らせる turn）。回し方（engine ごと、直列に）:
    //   cargo test -p vantage-point --lib e2e_real_agy -- --ignored --nocapture --test-threads=1
    // model を指定する（使用枠は model ごとに別）:
    //   VP_AGY_E2E_MODEL=gemini-3.8-flash / VP_OPENCODE_E2E_MODEL=opencode/big-pickle

    /// 実機 E2E で使う model（未設定 = engine の既定）。registry の意図と同じ経路
    /// （spawn 後の当て直し）で当てる。
    fn e2e_intent(var: &str) -> Vec<(String, String)> {
        std::env::var(var)
            .ok()
            .filter(|model| !model.is_empty())
            .map(|model| vec![("model".to_string(), model)])
            .unwrap_or_default()
    }

    /// 新規 → 送信 → engine を止める → 同じ会話 id で立て直す → 履歴が engine から戻る。
    ///
    /// 2 回目の host は VP の `replay_log` を一切読まない（そもそも書いていない）。戻ってくる発話と
    /// 答えは、engine の `session/load` が流したもの。
    ///
    /// `with_tool`: 発話で file を 1 つ作らせる。控えに `ToolCall` と `ToolCallUpdate` が同じ数だけ
    /// 入ることを見る（engine によって load の流れの形が違う: agy / grok は結果まで入った
    /// `tool_call` 1 件、opencode は `tool_call`（pending）+ `tool_call_update` の 2 段）。
    async fn e2e_new_send_respawn_history(
        engine: AcpEngine,
        intent: Vec<(String, String)>,
        with_tool: bool,
    ) {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = format!("vptest-acp-e2e-{}", engine.name());
        let config = |resume: Option<String>| AcpHostConfig {
            engine,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: repo.clone(),
            lane: "lead".into(),
            lane_label: "lead".into(),
            session_key: 1,
            session_id: resume,
            config_intent: intent.clone(),
        };

        // 1. 新規の会話で 1 turn。
        let mut host = AcpAgentHost::spawn(config(None)).expect("spawn");
        let mut rx = host.subscribe();
        let prompt = if with_tool {
            "Create a file named hello.txt containing the text hi in the current directory \
             using your file tool, then reply with exactly: pong-acp"
        } else {
            "Reply with exactly: pong-acp. Nothing else."
        };
        host.submit(prompt).await.expect("submit");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut answer = String::new();
        loop {
            let event = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("180 秒以内に turn が終わる")
                .expect("recv");
            match event {
                ConversationEvent::MessageChunk { text } => answer.push_str(&text),
                ConversationEvent::TurnCompleted { .. } => break,
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                ConversationEvent::EngineExited { message } => panic!("engine exited: {message}"),
                _ => {}
            }
        }
        assert!(answer.contains("pong-acp"), "answer: {answer}");
        let pid = host.pid();
        assert!(pid.is_some(), "engine の pid が読める");
        let axes: Vec<String> = host
            .config_view()
            .map(|view| view.options.into_iter().map(|o| o.id).collect())
            .unwrap_or_default();
        let session_id = crate::lane::session_registry::load(&repo, "lead", engine.name()).sessions
            [0]
        .conversation
        .clone()
        .expect("会話 id が registry に記録される（registry-native）");
        host.stop();
        drop(host);
        // engine が会話を保存し終えるのを待つ（止めた直後の load で欠けないように）。
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // 2. 同じ会話 id で立て直す。履歴は engine から来る。
        let mut host = AcpAgentHost::spawn(config(Some(session_id.clone()))).expect("respawn");
        let attached = host
            .take_replay_port()
            .expect("履歴の口")
            .source
            .attach()
            .await
            .expect("attach");
        let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
        let (mut said, mut answered) = (String::new(), String::new());
        for event in &attached.events {
            let kind = match event {
                ConversationEvent::UserMessage { text } => {
                    said.push_str(text);
                    "UserMessage"
                }
                ConversationEvent::MessageChunk { text } => {
                    answered.push_str(text);
                    "MessageChunk"
                }
                ConversationEvent::ThoughtChunk { .. } => "ThoughtChunk",
                ConversationEvent::ToolCall { .. } => "ToolCall",
                ConversationEvent::ToolCallUpdate { .. } => "ToolCallUpdate",
                _ => "other",
            };
            *kinds.entry(kind).or_default() += 1;
        }
        println!(
            "E2E {}: pid {pid:?} → {:?} / 設定面の軸 {axes:?} / 立て直しの後の控え {kinds:?} / in_flight={} truncated={}",
            engine.name(),
            host.pid(),
            attached.in_flight,
            attached.truncated
        );
        assert!(said.contains("pong-acp"), "発話が履歴に戻る: {said}");
        assert!(
            answered.contains("pong-acp"),
            "答えが履歴に戻る: {answered}"
        );
        assert!(!attached.in_flight);
        if with_tool {
            assert!(
                tmp.path().join("hello.txt").exists(),
                "tool が file を作った"
            );
            let calls = kinds.get("ToolCall").copied().unwrap_or(0);
            let updates = kinds.get("ToolCallUpdate").copied().unwrap_or(0);
            assert!(calls >= 1, "tool の呼び出しが履歴に戻る: {kinds:?}");
            assert_eq!(calls, updates, "呼び出しと結果が対で戻る: {kinds:?}");
        }
        assert_eq!(
            crate::lane::session_registry::load(&repo, "lead", engine.name()).sessions[0]
                .conversation
                .as_deref(),
            Some(session_id.as_str()),
            "load に成功した時は会話 id が変わらない"
        );
        host.stop();
    }

    /// engine を立て直した直後（会話の確定の前）に送った発話が、控えの末尾に入っている。
    ///
    /// `VP_AGY_E2E_MODEL` を指定して回す（設定の意図があると、確定の後に当て直しが走り、
    /// 「控えを取る → その後で発話が送られる」順になる = 発話が画面から消えていた形）。
    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_prompt_right_after_respawn_is_in_history() {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let intent = e2e_intent("VP_AGY_E2E_MODEL");
        let config = |resume: Option<String>| AcpHostConfig {
            engine: AcpEngine::Agy,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-acp-e2e-early".into(),
            lane: "lead".into(),
            lane_label: "lead".into(),
            session_key: 1,
            session_id: resume,
            config_intent: intent.clone(),
        };
        async fn until_turn_end(rx: &mut broadcast::Receiver<ConversationEvent>) -> String {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
            let mut answer = String::new();
            loop {
                match tokio::time::timeout_at(deadline, rx.recv())
                    .await
                    .expect("180 秒以内に turn が終わる")
                    .expect("recv")
                {
                    ConversationEvent::MessageChunk { text } => answer.push_str(&text),
                    ConversationEvent::TurnCompleted { .. } => return answer,
                    ConversationEvent::Error { message } => panic!("engine error: {message}"),
                    ConversationEvent::EngineExited { message } => {
                        panic!("engine exited: {message}")
                    }
                    _ => {}
                }
            }
        }

        // 1. 会話を作る。
        let mut host = AcpAgentHost::spawn(config(None)).expect("spawn");
        let mut rx = host.subscribe();
        host.submit("Reply with exactly: pong-one. Nothing else.")
            .await
            .expect("submit");
        assert!(until_turn_end(&mut rx).await.contains("pong-one"));
        let session_id = crate::lane::session_registry::load("vptest-acp-e2e-early", "lead", "agy")
            .sessions[0]
            .conversation
            .clone()
            .expect("会話 id");
        host.stop();
        drop(host);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // 2. 立て直して、確定を待たずに送る。
        let mut host = AcpAgentHost::spawn(config(Some(session_id))).expect("respawn");
        let mut rx = host.subscribe();
        host.submit("Reply with exactly: pong-two. Nothing else.")
            .await
            .expect("submit");
        let attached = host
            .take_replay_port()
            .expect("履歴の口")
            .source
            .attach()
            .await
            .expect("attach");
        let users: Vec<&str> = attached
            .events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::UserMessage { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        println!(
            "E2E agy（開いた直後に送る）: 控えの発話 {} 件 / 末尾は発話か {} / in_flight={} / 設定の意図 {intent:?}",
            users.len(),
            matches!(
                attached.events.last(),
                Some(ConversationEvent::UserMessage { .. })
            ),
            attached.in_flight
        );
        assert_eq!(users.len(), 2, "前の発話と、今送った発話: {users:?}");
        assert!(users[0].contains("pong-one") && users[1].contains("pong-two"));
        assert!(
            matches!(
                attached.events.last(),
                Some(ConversationEvent::UserMessage { text }) if text.contains("pong-two")
            ),
            "今送った発話が控えの末尾にある"
        );
        assert!(attached.in_flight, "これから turn が走る");
        assert!(until_turn_end(&mut rx).await.contains("pong-two"));
        host.stop();
    }

    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_new_send_respawn_history() {
        e2e_new_send_respawn_history(AcpEngine::Agy, e2e_intent("VP_AGY_E2E_MODEL"), false).await;
    }

    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_tool_turn_history() {
        e2e_new_send_respawn_history(AcpEngine::Agy, e2e_intent("VP_AGY_E2E_MODEL"), true).await;
    }

    #[tokio::test]
    #[ignore = "実機 grok CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_new_send_respawn_history() {
        e2e_new_send_respawn_history(AcpEngine::Grok, Vec::new(), false).await;
    }

    #[tokio::test]
    #[ignore = "実機 grok CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_tool_turn_history() {
        e2e_new_send_respawn_history(AcpEngine::Grok, Vec::new(), true).await;
    }

    /// VP の ChatHost → ACP adapter → 実 Grok で 256×256 の画像を読む（1 turn）。
    /// VP_GROK_E2E_MODEL で model を指定できる。画像の色は prompt に含めない。
    #[tokio::test]
    #[ignore = "実機 grok CLI + auth + model の使用枠が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_image_prompt() {
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = "vptest-grok-image-e2e";
        let adapter = AcpAgentHost::spawn(AcpHostConfig {
            engine: AcpEngine::Grok,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: repo.into(),
            lane: "lead".into(),
            lane_label: "lead".into(),
            session_key: 1,
            session_id: None,
            config_intent: e2e_intent("VP_GROK_E2E_MODEL"),
        })
        .expect("spawn");
        let mut rx = adapter.subscribe();
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            adapter
                .take_replay_port()
                .expect("履歴の口")
                .source
                .attach(),
        )
        .await
        .expect("60 秒以内に会話が確定する")
        .expect("attach");
        let model = adapter
            .config_view()
            .expect("設定面")
            .options
            .into_iter()
            .find(|option| option.id == "model")
            .expect("model")
            .current_value;
        let session = crate::lane::session_registry::load(repo, "lead", "grok").sessions[0]
            .conversation
            .clone()
            .expect("試験用の会話 id");
        println!(
            "Grok image E2E: model={model}, session={session}, cwd={:?}, image=256x256 PNG, turns=1",
            tmp.path()
        );
        let mut host = super::super::ChatHost::Grok(adapter);
        host.submit_with_images(
            "What is the dominant color in this image? Reply with only one English word. Do not use tools.",
            &[red_image_256()],
        )
        .await
        .expect("submit");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut answer = String::new();
        loop {
            let event = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("180 秒以内に turn が終わる")
                .expect("recv");
            match event {
                ConversationEvent::MessageChunk { text } => answer.push_str(&text),
                ConversationEvent::TurnCompleted { .. } => break,
                ConversationEvent::Error { message } => panic!("engine error: {message}"),
                ConversationEvent::EngineExited { message } => panic!("engine exited: {message}"),
                _ => {}
            }
        }
        host.stop();
        assert_eq!(
            answer.trim().trim_end_matches('.').to_ascii_lowercase(),
            "red",
            "画像の色を読めた: {answer}"
        );
    }

    #[tokio::test]
    #[ignore = "実機 opencode CLI + 接続先が必要（ローカル E2E 専用）"]
    async fn e2e_real_opencode_new_send_respawn_history() {
        let intent = e2e_intent("VP_OPENCODE_E2E_MODEL");
        e2e_new_send_respawn_history(AcpEngine::OpenCode, intent, false).await;
    }

    #[tokio::test]
    #[ignore = "実機 opencode CLI + 接続先が必要（ローカル E2E 専用）"]
    async fn e2e_real_opencode_tool_turn_history() {
        let intent = e2e_intent("VP_OPENCODE_E2E_MODEL");
        e2e_new_send_respawn_history(AcpEngine::OpenCode, intent, true).await;
    }
}
