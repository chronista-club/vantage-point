//! AcpAgentHost — ACP engine（grok / opencode / agy）を lane 単位で**常駐**駆動する host（gui、doc 42・43・88）
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
//! 途絶の自己修復: engine が終了すると `submit` が Err を返し、`ensure_and_submit_chat` が
//! drop → 再 ensure → retry する。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_chat_acp::{
    AcpClientHost, HostOptions, LaunchContext, LoadFallback, PermissionDecision, PromptImage,
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
}

impl AcpEngine {
    /// registry / ログ / エラー文言の名乗り（[`super::EngineKind::agent_name`] と一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::OpenCode => "opencode",
            Self::Agy => "agy",
        }
    }

    /// CLI 実行パスの解決（launchd 起動 daemon の細い PATH 対策 — `session_store::resolve_cli`）。
    fn cli_path(self) -> String {
        match self {
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
/// 許可の option（`allow_once` → `allow_always`）を選ぶ。許可の option が無ければ先頭の option
/// （手書きの host と同じ。共有 crate の既定は「無ければ取り消す」）。
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

/// lane 単位の常駐 ACP host（gui、doc 42・43・88）。grok / opencode / agy の 3 engine を
/// [`AcpEngine`] パラメータ（cli_path / spawn_args / name）で共有する。
pub struct AcpAgentHost {
    engine: AcpEngine,
    host: Arc<AcpClientHost>,
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
            .map_err(|e| anyhow::anyhow!("{} の起動に失敗: {e}", engine.name()))?;
        tracing::info!(
            "AcpAgentHost spawn（常駐 {} ACP、repo={}, lane={}, resume={}）",
            engine.name(),
            config.repo,
            config.lane,
            config.session_id.as_deref().unwrap_or("new"),
        );
        Ok(Self::wrap(engine, host))
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
                .map_err(|e| anyhow::anyhow!("{} の起動に失敗: {e}", engine.name()))?;
        Ok(Self::wrap(engine, host))
    }

    fn wrap(engine: AcpEngine, host: AcpClientHost) -> Self {
        let host = Arc::new(host);
        let (replay, requests) = ReplayRequester::channel();
        Self {
            engine,
            replay,
            port: Mutex::new(Some(ReplayPort {
                source: Arc::new(AcpReplaySource(Arc::clone(&host))),
                requests,
            })),
            host,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.host.subscribe()
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
        self.host.config_view().map(Into::into)
    }

    /// user の設定切替の口（pool の lock を離してから応答を待つため、host の外へ clone して持ち出す）。
    pub fn config_handle(&self) -> AcpConfigHandle {
        AcpConfigHandle {
            engine: self.engine,
            host: Arc::clone(&self.host),
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

    /// 実行中 turn を中断する（`session/cancel` notification — プロセスは殺さない）。
    /// prompt response が stopReason=cancelled で返り、通常の完了経路で streaming が畳まれる。
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        self.host
            .interrupt()
            .map_err(|e| anyhow::anyhow!("{} を中断できません: {e}", self.engine.name()))
    }

    /// process を立て直して load し直す（agent-chat 設計書 01「呼び手の規則」）。
    ///
    /// 他の書き手（console）が会話を進めた後、次の発話の前に呼ぶための口。生きている engine は
    /// 他の書き手の分を知らない（agy 1.5.11 では保存からも消える、実測）。turn の最中は Err。
    /// 成功したら履歴を配り直す。
    ///
    /// ⚠️ **呼び手はまだ無い**（2026-11-20）。いつ呼ぶかは別の task（creo `mem_1ChA3fZy2Kq7tb3qaLSmpe`、
    /// 直し方が未決）で決める。
    pub async fn restart(&self) -> Result<(), String> {
        self.host.restart().await.map_err(|e| e.to_string())?;
        self.replay.request();
        Ok(())
    }

    /// 明示 teardown（[`super::engine::ChatEngineSlot`] Drop から呼ぶ）。
    pub fn stop(&mut self) {
        self.host.stop();
        tracing::info!("AcpAgentHost stop（{}）", self.engine.name());
    }
}

/// pump に渡す履歴の出どころ（共有 host の `attach()`）。
struct AcpReplaySource(Arc<AcpClientHost>);

impl ReplaySource for AcpReplaySource {
    fn attach(&self) -> AttachFuture {
        let host = Arc::clone(&self.0);
        Box::pin(async move {
            let (history, rx) = host.attach().await.map_err(|e| e.to_string())?;
            Ok(ReplayAttach {
                events: history.events,
                in_flight: history.in_flight,
                truncated: history.truncated,
                rx,
            })
        })
    }

    fn session_init(&self) -> Option<ConversationEvent> {
        self.0.session_init()
    }
}

/// user の設定切替の口（[`AcpAgentHost::config_handle`]）。host を共有する薄い clone。
///
/// pool の lock を握ったまま engine の応答を待たないために、host から切り離して持ち出す。
#[derive(Clone)]
pub struct AcpConfigHandle {
    engine: AcpEngine,
    host: Arc<AcpClientHost>,
}

impl AcpConfigHandle {
    /// `session/set_config_option` を送り、engine の応答まで待つ（上限は共有 host の
    /// `config_timeout`、既定 15 秒）。
    ///
    /// 成功で configOptions（候補を含む全体）が更新され、それを返す（model を替えると他の軸の
    /// 候補が変わるため、呼び手が意図を整えるのに使う）。失敗は engine のエラー文を返し、
    /// roster の `acp_config.error` にも残る。永続（registry）は呼び手の仕事（engine 別の形）。
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
        assert_eq!(
            auto_allow(&permission_request(serde_json::json!([
                {"optionId": "rej", "name": "No", "kind": "reject_once"},
                {"optionId": "yes", "name": "Yes", "kind": "allow_once"},
            ]))),
            PermissionDecision::Select("yes".into())
        );
        assert_eq!(
            auto_allow(&permission_request(serde_json::json!([
                {"optionId": "first", "name": "No", "kind": "reject_once"},
            ]))),
            PermissionDecision::Select("first".into())
        );
        assert_eq!(
            auto_allow(&permission_request(serde_json::json!([]))),
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

    /// opencode 2.3.0 が出す設定面の形（実測 2026-11-20: model / effort / mode の 3 軸）。
    fn opencode_like_config() -> Vec<serde_json::Value> {
        serde_json::from_value(serde_json::json!([
            {"id": "model", "name": "Model", "type": "select", "currentValue": "opencode/big-pickle",
             "options": [{"value": "opencode/big-pickle", "name": "Big Pickle"}]},
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
                ("model", "opencode/big-pickle"),
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
    // 回し方（1 本ずつ）:
    //   cargo test -p vantage-point --lib e2e_real_agy -- --ignored --nocapture
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
    async fn e2e_new_send_respawn_history(engine: AcpEngine, intent: Vec<(String, String)>) {
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
        host.submit("Reply with exactly: pong-acp. Nothing else.")
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
        assert_eq!(
            crate::lane::session_registry::load(&repo, "lead", engine.name()).sessions[0]
                .conversation
                .as_deref(),
            Some(session_id.as_str()),
            "load に成功した時は会話 id が変わらない"
        );
        host.stop();
    }

    #[tokio::test]
    #[ignore = "実機 agy CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_agy_new_send_respawn_history() {
        e2e_new_send_respawn_history(AcpEngine::Agy, e2e_intent("VP_AGY_E2E_MODEL")).await;
    }

    #[tokio::test]
    #[ignore = "実機 grok CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_new_send_respawn_history() {
        e2e_new_send_respawn_history(AcpEngine::Grok, Vec::new()).await;
    }

    #[tokio::test]
    #[ignore = "実機 opencode CLI + 接続先が必要（ローカル E2E 専用）"]
    async fn e2e_real_opencode_new_send_respawn_history() {
        e2e_new_send_respawn_history(AcpEngine::OpenCode, e2e_intent("VP_OPENCODE_E2E_MODEL"))
            .await;
    }
}
