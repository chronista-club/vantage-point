//! lane ごとの Echoes session registry 永続（doc 38 — 1 Lane = N session / doc 40 — 会話 id SSOT）
//!
//! doc 38 §1 の 3 層分離の「session 層」を担う:
//!
//! ```text
//! 床（Act I の PTY）= lane の設備（1 枚）。本 module の管轄外
//! session          = 会話の実体。identity は VP 採番のローカル key（1, 2, …）← ここ
//! 会話 id          = session の Option 属性（`SessionEntry.conversation`）← doc 40 でここに統合
//! ```
//!
//! - **disk = 唯一の真実源**（doc 38 §5 原則「供給を 1 系統に」）。in-memory cache は持たない。
//!   registry の読み書きは全て本 module 経由（`LanePool` も RPC もここを読む）
//! - 置き場: `vp_state_dir()/echoes_sessions/<project>__<lane>.json`（1 lane 1 file）
//! - **file 不在 = N=1 の特殊ケース**: 「lane の stand で session #1 のみ・focused=1・root=1」
//!   に解決される。既存 install は registry file を持たないが従来どおり動く（既存動作不変の要）
//! - **会話 id は本 registry が SSOT**（doc 40 §2。旧 engine 別 session_store のラベル鍵は
//!   書き手/読み手の乖離バグを産んだ — doc 40 §1-1）。旧 store は [`load_in`] の read-only
//!   backfill（移行 bridge、doc 40 §3）としてだけ読まれ、PR-2 で退役する
//! - **書き込みの直列化**: 変異（create / focus / remove / set_conversation 系）は process 内
//!   mutex で直列化する（doc 40 §4 — 複数 field JSON の並行 load-modify-save は update を失う）
//! - **root = lane の器に化身する session**（doc 39 — 座と化身）: 床 spawn / wire 配送
//!   （channel D・E）/ Act I chip はすべて root に解決される。doc 38 の「床は session #1 を
//!   既定で化身」は root=1 の特殊ケースに一般化された（#1 の特別性を撤廃）

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session_store::sanitize;

/// session の VP 採番ローカル key（1 始まり、lane 内で単調増加・再利用しない）。
pub type SessionKey = u32;

/// registry 上の 1 session。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// VP 採番のローカル key。
    pub key: SessionKey,
    /// engine 種別（stand 名: "echoes" / "cursor" / "codex" / "agy"）。
    pub stand: String,
    /// engine の会話 id（claude = session uuid / cursor = chatId / codex = thread id）。
    /// **doc 40 §2: ここが SSOT**（旧 engine 別 session_store から統合）。None = Draft
    /// （まだ engine が id を発番していない、doc 38 §1.1）。serde default + skip で
    /// file/wire 後方互換（conversation 無し = 旧 file はそのまま読める）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

/// lane の session 一覧 + focused + root（disk に JSON でそのまま永続される形）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRegistry {
    /// 現在 focus されている session の key（常に `sessions` 内に実在する）。
    pub focused: SessionKey,
    /// doc 39: lane の器（Act I=床 / Act II=headless）に化身し、wire mailbox
    /// `agent@<lane>` を名乗る session の key（常に `sessions` 内に実在する）。
    /// 床 spawn / wire 配送（channel D・E）/ Act I chip の読み先はすべてここに解決される。
    /// serde default = 1 で file/wire 後方互換（root field 無し = 従来の「#1 が床に化身」を
    /// root=1 として読む — doc 38 Phase 1 の focused と同じ手筋）。
    #[serde(default = "default_root")]
    pub root: SessionKey,
    /// 次に採番する key（単調増加。fresh reset まで再利用しない）。
    pub next: SessionKey,
    /// session 一覧（生成順）。空にはならない（最低 1 本）。
    pub sessions: Vec<SessionEntry>,
}

/// serde default: root field を持たない既存 file / wire を「#1 が root」として読む。
fn default_root() -> SessionKey {
    1
}

impl SessionRegistry {
    /// N=1 の特殊ケース（file 不在時の既定形）: lane の stand で session #1 のみ。
    fn single(default_stand: &str) -> Self {
        Self {
            focused: 1,
            root: 1,
            next: 2,
            sessions: vec![SessionEntry {
                key: 1,
                stand: default_stand.to_string(),
                conversation: None,
            }],
        }
    }

    /// 不変条件の検証: 非空・key は 1 以上で重複なし・focused / root 実在・next は最大 key より
    /// 大きい。手編集や部分破損で崩れた file を「壊れた state で動き続ける」より default に
    /// 戻す方が安全。
    fn is_valid(&self) -> bool {
        !self.sessions.is_empty()
            && self.sessions.iter().all(|s| s.key >= 1)
            && self
                .sessions
                .iter()
                .enumerate()
                .all(|(i, s)| !self.sessions[..i].iter().any(|t| t.key == s.key))
            && self.sessions.iter().any(|s| s.key == self.focused)
            && self.sessions.iter().any(|s| s.key == self.root)
            && self.sessions.iter().all(|s| s.key < self.next)
    }
}

/// session の store label（各 engine session_store / host の記録キー）。
///
/// - **key 1 = 素の lane 名**: 既存 file（`cc_sessions/<project>__<lane>`）との後方互換 +
///   Act I（床）の hook 書き込み先と一致（doc 38 の「床は session #1 を既定で化身」）
/// - key 2 以降 = `<lane>#<n>`（doc 36 実証: `#` は [`sanitize`] で置換されない = file 名安全）
pub fn session_label(lane_label: &str, key: SessionKey) -> String {
    if key <= 1 {
        lane_label.to_string()
    } else {
        format!("{lane_label}#{key}")
    }
}

/// state base dir 配下の registry file path（純関数、テスト用に base 注入）。
fn registry_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("echoes_sessions")
        .join(format!("{}__{}.json", sanitize(project), sanitize(lane)))
}

/// registry 変異の process 内直列化（doc 40 §4）。複数 field JSON の並行 load-modify-save は
/// update を失うため、変異系（create / focus / remove / set_conversation 系）はすべて本 lock を
/// 通る。読み（load / focused / root）は lock 不要 — save の atomic rename が読みの整合を担う。
/// poisoned は継続（lock 保持中の panic で file が壊れるわけではない）。
static MUTATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mutation_guard() -> std::sync::MutexGuard<'static, ()> {
    MUTATION.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`session_label`] の逆関数: store label / host config の label から (lane label, key) を
/// 復元する（doc 40 §4 — Act II host は自分の label しか知らないため、registry 書き込みで
/// key へ逆引きする）。`#` 以降が数値でない label は「素の lane 名 = key 1」として扱う
/// （lane 名自体に `#` を含む edge を破壊しない）。
pub fn parse_session_label(label: &str) -> (&str, SessionKey) {
    if let Some((prefix, suffix)) = label.rsplit_once('#')
        && !prefix.is_empty()
        && let Ok(key) = suffix.parse::<SessionKey>()
        && key >= 1
    {
        return (prefix, key);
    }
    (label, 1)
}

/// 会話 id の engine 別検証（doc 40 §4 — write 側 dispatch）。`--resume '<id>'` への
/// injection 防壁を旧 store の書き込み検証から引き継ぐ（spawn 側の再検証と二段 = 深層防御）。
fn is_valid_conversation(stand: &str, id: &str) -> bool {
    use crate::echoes::EngineKind;
    match EngineKind::from_stand(stand) {
        Some(EngineKind::Claude) => super::cc_session::is_valid_session_id(id),
        Some(EngineKind::Cursor) => super::cursor_session::is_valid_chat_id(id),
        Some(EngineKind::Codex) => super::codex_session::is_valid_thread_id(id),
        // grok = ACP sessionId（UUID v7 形 — 英数+ハイフン、doc 42 §1。registry-native なので
        // engine 別 store module を持たない = 検証だけここに置く）。
        Some(EngineKind::Grok) => {
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        Some(EngineKind::Agy) | None => false,
    }
}

/// registry を読む。file 不在 / 破損 / 不変条件違反は N=1 の既定形に解決（Err にしない —
/// 読めない registry で lane 全体を止めるより、既定形で動き続ける方が復旧可能性が高い）。
pub fn load_in(base: &Path, project: &str, lane: &str, default_stand: &str) -> SessionRegistry {
    let mut reg = match std::fs::read_to_string(registry_file_in(base, project, lane)) {
        Ok(raw) => match serde_json::from_str::<SessionRegistry>(&raw) {
            Ok(reg) if reg.is_valid() => reg,
            _ => {
                tracing::warn!(
                    "session registry が不正のため既定形に解決（project={project}, lane={lane}）"
                );
                SessionRegistry::single(default_stand)
            }
        },
        Err(_) => SessionRegistry::single(default_stand),
    };
    backfill_legacy_conversations(base, project, lane, &mut reg);
    reg
}

/// doc 40 §3 の移行 bridge: conversation 未統合の entry を旧 engine 別 store（cc/cursor/
/// codex_sessions）から read-only 補完する。registry への固定は次の save が自然に行う
/// （本関数は file を書かない）。**PR-2（legacy store 退役）で本関数ごと撤去する。**
fn backfill_legacy_conversations(
    base: &Path,
    project: &str,
    lane: &str,
    reg: &mut SessionRegistry,
) {
    use crate::echoes::EngineKind;
    for entry in reg.sessions.iter_mut().filter(|e| e.conversation.is_none()) {
        let label = session_label(lane, entry.key);
        entry.conversation = match EngineKind::from_stand(&entry.stand) {
            Some(EngineKind::Claude) => super::cc_session::last_in(base, project, &label),
            Some(EngineKind::Cursor) => super::cursor_session::last_in(base, project, &label),
            Some(EngineKind::Codex) => super::codex_session::last_in(base, project, &label),
            // grok は registry-native（旧 store が存在しない — bridge の対象外）。
            Some(EngineKind::Grok) | Some(EngineKind::Agy) | None => None,
        };
    }
}

/// registry を書く（上書き）。不変条件違反は書かずに Err（壊れた state を disk に固定しない）。
///
/// tmp file + rename の atomic 置換で書く — 他プロセスの read-only reader（hook の no-op
/// 事前判定 / daemon の channel D）が truncate 途中の部分 file を拾って既定形 fallback に
/// 落ちる窓を塞ぐ（doc 40 で write 頻度が上がるため顕在化しやすくなる穴）。
pub fn save_in(
    base: &Path,
    project: &str,
    lane: &str,
    reg: &SessionRegistry,
) -> std::io::Result<()> {
    if !reg.is_valid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session registry が不変条件違反（project={project}, lane={lane}）"),
        ));
    }
    let path = registry_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(reg).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

/// session を 1 本追加して key を返す（`focus=true` なら focused も移す）。
pub fn create_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    focus: bool,
) -> std::io::Result<SessionKey> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let key = reg.next;
    reg.next += 1;
    reg.sessions.push(SessionEntry {
        key,
        stand: stand.to_string(),
        conversation: None,
    });
    if focus {
        reg.focused = key;
    }
    save_in(base, project, lane, &reg)?;
    Ok(key)
}

/// 新 session を作り、root と focused を同時にそれへ向ける（doc 39 §4 — Act I の ✨ New =
/// Root 切替「✨ 新 ID から」の shorthand）。1 回の save で書くため、器（床）と mailbox の
/// 化身がズレる中間 state は disk に存在しない（doc 39 §0「原子的」の registry 側担保）。
/// 旧 root の session は一覧に残る（非破壊 — store も registry entry も触らない）。
pub fn create_root_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
) -> std::io::Result<SessionKey> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let key = reg.next;
    reg.next += 1;
    reg.sessions.push(SessionEntry {
        key,
        stand: stand.to_string(),
        conversation: None,
    });
    reg.focused = key;
    reg.root = key;
    save_in(base, project, lane, &reg)?;
    Ok(key)
}

/// focused を切り替える。実在しない key は Err（黙って据え置くと「切替えたつもり」の誤配送になる）。
pub fn focus_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<()> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    if !reg.sessions.iter().any(|s| s.key == key) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session が存在しません（project={project}, lane={lane}, session={key}）"),
        ));
    }
    reg.focused = key;
    save_in(base, project, lane, &reg)
}

/// session を 1 本取り除く（doc 38 Phase 3 — tab を閉じる）。
///
/// - 実在しない key は Err（黙って成功にしない）
/// - **root は取り除けない**（doc 39 §6。doc 38 の「最後の 1 本は取り除けない」と
///   「⚠️ #1 close は Act I 床 resume を断つ」を包含する一般形 — root は常に実在するので
///   最後の 1 本 = root。root を移してから取り除く。lane を素に戻したいなら
///   fresh restart = registry clear が正道）
/// - focused を取り除いた場合は残りの先頭へ focus を移す（決定的な fallback）
/// - 取り除いた key は再利用されない（`next` は据え置き = 採番の単調性維持）
///
/// 戻り値 = 取り除き後の focused key（caller が engine drop / 表示更新に使う）。
pub fn remove_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<SessionKey> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    if !reg.sessions.iter().any(|s| s.key == key) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session が存在しません（project={project}, lane={lane}, session={key}）"),
        ));
    }
    if key == reg.root {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "root session は取り除けません（project={project}, lane={lane}, session={key}。root を移すか fresh restart で素に戻せます）"
            ),
        ));
    }
    reg.sessions.retain(|s| s.key != key);
    if reg.focused == key {
        // 決定的 fallback: 残りの先頭（生成順で最も古い session）。
        reg.focused = reg.sessions[0].key;
    }
    save_in(base, project, lane, &reg)?;
    Ok(reg.focused)
}

/// 会話報告の契機（doc 40 §6）。CC hook の event 名でなく意味で持つ（engine 常駐統合
/// （doc 39 §7）で claude 以外の報告者が増えても再利用できる語彙）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationReport {
    /// id 発行時点の eager 報告（SessionStart）。`|| claude` fallback の幻 session であり得る
    /// ため、健在な既存会話は上書きしない（F1/F2 guard — doc 40 §6 の表）。
    Issued,
    /// user が実際に話しかけた authoritative 報告（UserPromptSubmit）。無条件で記録する
    /// （user が commit した会話が常に勝つ）。
    Spoken,
}

/// [`record_root_conversation_in`] の結果。caller（SP handler）が log と Diff::Update push の
/// 要否判定に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootRecordOutcome {
    /// 記録した（disk 変化あり）。
    Recorded,
    /// 既に同 id（no-op）。
    Unchanged,
    /// F1/F2 guard 発動: 既存会話の transcript が健在なため据え置き（Issued のみ）。
    KeptExisting,
    /// root session が claude でない（claude hook の id を他 engine の session に混ぜない）。
    IgnoredNonClaude,
    /// id が形式外（書かず）。
    RejectedInvalid,
}

/// Act I 床（claude hook）の会話報告を root session に適用する — doc 40 §6 policy の
/// **唯一の実装点**。旧「UserPromptSubmit のみ記録」（#795 の鈍器）の置換。
///
/// `transcript_exists` は注入する（テストが実 `~/.claude` に依存しないため。本番 wrapper
/// [`record_root_conversation`] が `cc_session::transcript_exists` を渡す）。
pub fn record_root_conversation_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    session_id: &str,
    report: ConversationReport,
    transcript_exists: impl Fn(&str) -> bool,
) -> std::io::Result<RootRecordOutcome> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let root = reg.root;
    let Some(entry) = reg.sessions.iter_mut().find(|s| s.key == root) else {
        // is_valid が root 実在を保証するため到達しない（防御的に no-op）
        return Ok(RootRecordOutcome::Unchanged);
    };
    if !matches!(
        crate::echoes::EngineKind::from_stand(&entry.stand),
        Some(crate::echoes::EngineKind::Claude)
    ) {
        return Ok(RootRecordOutcome::IgnoredNonClaude);
    }
    if !is_valid_conversation(&entry.stand, session_id) {
        return Ok(RootRecordOutcome::RejectedInvalid);
    }
    match &entry.conversation {
        Some(cur) if cur == session_id => Ok(RootRecordOutcome::Unchanged),
        Some(cur) if report == ConversationReport::Issued && transcript_exists(cur) => {
            // resume 失敗 `|| claude` fallback の幻 session が、健在な旧会話への復帰路を
            // 上書きするのを防ぐ（F1 clobber / F2 幻ポインタの再演防止）。次の Spoken で
            // user が幻側に commit したら上書きされる（self-heal）。
            Ok(RootRecordOutcome::KeptExisting)
        }
        _ => {
            entry.conversation = Some(session_id.to_string());
            save_in(base, project, lane, &reg)?;
            Ok(RootRecordOutcome::Recorded)
        }
    }
}

/// session の会話 id を書く（doc 40 §4 — Act II host の record-from-init / cursor create-chat
/// 採番の書き込み口。SP プロセス内から呼ぶ）。
///
/// - 実在しない key は Err（黙って捨てると「記録したつもり」の幻 resume になる）
/// - 形式外 id は**書かずに** Ok(false)（旧 session_store の共通原則を引き継ぐ）
/// - 変化なしは save しない。戻り値 = 「disk が変わったか」（caller の Diff::Update 判定用）
/// - `None`（clear）は旧 engine store も併せて消す — 残すと次 load の backfill が閉じた会話を
///   蘇らせる（doc 40 §9。bridge 撤去 = PR-2 で本処理ごと消える）
pub fn set_conversation_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
    conversation: Option<&str>,
) -> std::io::Result<bool> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let Some(entry) = reg.sessions.iter_mut().find(|s| s.key == key) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session が存在しません（project={project}, lane={lane}, session={key}）"),
        ));
    };
    if let Some(id) = conversation
        && !is_valid_conversation(&entry.stand, id)
    {
        tracing::warn!(
            "会話 id が形式外のため書かず（project={project}, lane={lane}, session={key}, stand={}）",
            entry.stand
        );
        return Ok(false);
    }
    if conversation.is_none() {
        // bridge 整合: legacy store を残すと backfill が復活させる（上記 doc）。
        use crate::echoes::EngineKind;
        let label = session_label(lane, key);
        let _ = match EngineKind::from_stand(&entry.stand) {
            Some(EngineKind::Claude) => super::cc_session::clear_in(base, project, &label),
            Some(EngineKind::Cursor) => super::cursor_session::clear_in(base, project, &label),
            Some(EngineKind::Codex) => super::codex_session::clear_in(base, project, &label),
            // grok は registry-native（legacy store が無い = 蘇生源も無い）。
            Some(EngineKind::Grok) | Some(EngineKind::Agy) | None => Ok(()),
        };
    }
    let new = conversation.map(str::to_string);
    if entry.conversation == new {
        return Ok(false);
    }
    entry.conversation = new;
    save_in(base, project, lane, &reg)?;
    Ok(true)
}

/// focused key だけを軽量に読む（file 不在 / 破損は 1 = N=1 特殊ケース）。
/// `LaneInfo::refresh_engine_session_id` のような enrich 経路用（default stand 不要）。
pub fn focused_in(base: &Path, project: &str, lane: &str) -> SessionKey {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return 1;
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg.focused,
        _ => 1,
    }
}

/// root key だけを軽量に読む（file 不在 / 破損は 1 = N=1 特殊ケース）。
/// 床 spawn（`stand_spawner`）/ channel D enrich のような「registry 全体は要らない」経路用。
pub fn root_in(base: &Path, project: &str, lane: &str) -> SessionKey {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return 1;
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg.root,
        _ => 1,
    }
}

/// registry を捨てる（fresh reset）。file 不在は no-op。
///
/// 「fresh = N=1 の既定形へ戻す」の state 側表現（doc 38 落とし穴②「fresh が副 session を
/// 知らない」の再演防止 — 個別 field の初期化でなく file ごと捨てて既定形に収束させる）。
/// 採番 counter も 1 からやり直しになる（fresh 後の会話 id は全 store で消えている前提）。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(registry_file_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

// ---- 本番 base（vp_state_dir）での wrapper ----

/// 本番 base での load。
pub fn load(project: &str, lane: &str, default_stand: &str) -> SessionRegistry {
    load_in(&crate::config::vp_state_dir(), project, lane, default_stand)
}

/// 本番 base での create。
pub fn create(
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    focus: bool,
) -> std::io::Result<SessionKey> {
    create_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        stand,
        focus,
    )
}

/// 本番 base での create_root。
pub fn create_root(
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
) -> std::io::Result<SessionKey> {
    create_root_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        stand,
    )
}

/// 本番 base での focus。
pub fn focus(
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<()> {
    focus_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        key,
    )
}

/// 本番 base での remove。
pub fn remove(
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<SessionKey> {
    remove_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        key,
    )
}

/// 本番 base での focused。
pub fn focused(project: &str, lane: &str) -> SessionKey {
    focused_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での root。
pub fn root(project: &str, lane: &str) -> SessionKey {
    root_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での clear。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での set_conversation（Act II host の record-from-init から呼ぶ）。
pub fn set_conversation(
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
    conversation: Option<&str>,
) -> std::io::Result<bool> {
    set_conversation_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        key,
        conversation,
    )
}

/// 本番 base での record_root_conversation（SP の hook 報告 handler から呼ぶ）。
/// F1/F2 guard の transcript 判定は claude の実 transcript（`cc_session::transcript_exists`）。
pub fn record_root_conversation(
    project: &str,
    lane: &str,
    default_stand: &str,
    session_id: &str,
    report: ConversationReport,
) -> std::io::Result<RootRecordOutcome> {
    record_root_conversation_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        session_id,
        report,
        super::cc_session::transcript_exists,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// file 不在 = N=1 の特殊ケース（lane の stand で session #1・focused=1・root=1）。
    /// 既存 install が registry file 無しで従来どおり動くことの根拠。
    #[test]
    fn load_without_file_resolves_to_single_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(
            reg,
            SessionRegistry {
                focused: 1,
                root: 1,
                next: 2,
                sessions: vec![SessionEntry {
                    key: 1,
                    stand: "echoes".to_string(),
                    conversation: None,
                }],
            }
        );
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 1);
        assert_eq!(root_in(tmp.path(), "vp", "conductor"), 1);
    }

    /// doc 39 P1 の後方互換の核: root field を持たない既存 file は root=1 として読める
    /// （serde default）。既存 install の registry を壊さず root を導入できる根拠。
    #[test]
    fn registry_without_root_field_reads_as_root_1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vp__conductor.json"),
            r#"{"focused":2,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":2,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.root, 1, "root 無し file は root=1（従来の #1 化身）");
        assert_eq!(reg.focused, 2, "focused は file の値を維持");
        assert_eq!(root_in(tmp.path(), "vp", "conductor"), 1);
    }

    /// create → 採番 2・focus 追随 → roundtrip 永続。focus=false は focused を据え置く。
    #[test]
    fn create_assigns_monotonic_keys_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let k2 = create_in(tmp.path(), "vp", "conductor", "echoes", "codex", true).expect("create");
        assert_eq!(k2, 2);
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 2, "focus=true は新 session に focus が移る");
        assert_eq!(reg.sessions.len(), 2);
        assert_eq!(reg.sessions[0].stand, "echoes", "session #1 は lane stand");
        assert_eq!(reg.sessions[1].stand, "codex");

        let k3 =
            create_in(tmp.path(), "vp", "conductor", "echoes", "echoes", false).expect("create");
        assert_eq!(k3, 3);
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 2, "focus=false は focused を動かさない");
        assert_eq!(reg.next, 4);
    }

    /// focus は実在 key のみ受理。不在 key は Err（黙って据え置かない）。
    #[test]
    fn focus_rejects_unknown_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(tmp.path(), "vp", "conductor", "echoes", "codex", false).expect("create");
        focus_in(tmp.path(), "vp", "conductor", "echoes", 2).expect("実在 key への focus");
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 2);
        assert!(
            focus_in(tmp.path(), "vp", "conductor", "echoes", 99).is_err(),
            "不在 key への focus は Err"
        );
    }

    /// 破損 file / 不変条件違反は既定形に解決（壊れた state で動き続けない）。
    #[test]
    fn corrupt_or_invalid_file_falls_back_to_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("vp__conductor.json");

        // 非 JSON
        std::fs::write(&file, "not json").unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.focused, 1);

        // focused が不在 key（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":9,"next":3,"sessions":[{"key":1,"stand":"echoes"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 1, "focused 不在の file は既定形に解決");
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 1);

        // key 重複（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":1,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":1,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1, "key 重複の file は既定形に解決");

        // root が不在 key（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":1,"root":9,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":2,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.root, 1, "root 不在の file は既定形に解決");
        assert_eq!(root_in(tmp.path(), "vp", "conductor"), 1);
    }

    /// remove: 実在検証 / root は拒否（doc 39 §6 — 最後の 1 本の拒否を包含）/
    /// focused fallback は残りの先頭 / key 再利用なし。
    #[test]
    fn remove_validates_and_moves_focus_deterministically() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // #2(codex, focused) / #3(cursor) を追加 → #1/#2/#3
        create_in(tmp.path(), "vp", "conductor", "echoes", "codex", true).expect("create #2");
        create_in(tmp.path(), "vp", "conductor", "echoes", "cursor", false).expect("create #3");

        // 不在 key は Err
        assert!(remove_in(tmp.path(), "vp", "conductor", "echoes", 9).is_err());

        // root(#1) は N>1 でも取り除けない（doc 38 の「⚠️ #1 close は Act I 床 resume を断つ」
        // footgun を構造で塞ぐ — doc 39 §2）
        assert!(
            remove_in(tmp.path(), "vp", "conductor", "echoes", 1).is_err(),
            "root session の remove は Err"
        );

        // focused(#2) を remove → focus は残りの先頭(#1) へ
        let focused = remove_in(tmp.path(), "vp", "conductor", "echoes", 2).expect("remove #2");
        assert_eq!(focused, 1, "focused の remove は残り先頭へ fallback");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(
            reg.sessions.iter().map(|s| s.key).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(reg.next, 4, "採番は据え置き（key 再利用なし）");

        // 非 focused(#3) を remove → focus は不変
        let focused = remove_in(tmp.path(), "vp", "conductor", "echoes", 3).expect("remove #3");
        assert_eq!(focused, 1);

        // 最後の 1 本 = root なので取り除けない（fresh restart が正道）
        assert!(
            remove_in(tmp.path(), "vp", "conductor", "echoes", 1).is_err(),
            "最後の session の remove は Err"
        );
    }

    /// create_root: 新 session に root + focused が同時に移り、旧 root は一覧に残る（doc 39 §4
    /// Act I New = 非破壊）。旧 root（#1）は非 root になったので remove 可能になる。
    #[test]
    fn create_root_moves_root_and_focus_atomically() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let k2 = create_root_in(tmp.path(), "vp", "conductor", "echoes", "echoes")
            .expect("create_root #2");
        assert_eq!(k2, 2);
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.root, 2, "root は新 session へ");
        assert_eq!(reg.focused, 2, "focused も新 session へ（追加して focus）");
        assert_eq!(reg.sessions.len(), 2, "旧 root(#1) は一覧に残る（非破壊）");
        assert_eq!(root_in(tmp.path(), "vp", "conductor"), 2);

        // 旧 root(#1) は非 root になったので閉じられる（doc 39 §2 — #1 の特別性撤廃）
        let focused = remove_in(tmp.path(), "vp", "conductor", "echoes", 1).expect("remove #1");
        assert_eq!(focused, 2);
        // 新 root(#2) は取り除けない
        assert!(remove_in(tmp.path(), "vp", "conductor", "echoes", 2).is_err());
    }

    /// clear = fresh reset。file が消えて既定形に戻り、採番も 1 からやり直し。冪等。
    #[test]
    fn clear_resets_to_default_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(tmp.path(), "vp", "conductor", "echoes", "codex", true).expect("create");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.next, 2, "fresh 後は採番もやり直し");
        // 未記録の clear は no-op（session_store と同じ原則）
        clear_in(tmp.path(), "vp", "conductor").expect("二重 clear は Ok");
    }

    /// session label: key 1 = 素の lane 名（既存 file 互換）、key 2+ = `<lane>#<n>`。
    /// `#` は sanitize で置換されない = session_store の file 名にそのまま安全に使える
    /// （doc 36 実証の固定化）。
    #[test]
    fn session_label_is_bare_for_key1_and_hash_suffixed_after() {
        assert_eq!(session_label("conductor", 1), "conductor");
        assert_eq!(session_label("conductor", 2), "conductor#2");
        assert_eq!(session_label("feat-x", 10), "feat-x#10");
        assert_eq!(sanitize("conductor#2"), "conductor#2", "# は sanitize 安全");
    }

    /// doc 40 §3 bridge: conversation 未統合 entry は旧 engine store から stand 別に backfill
    /// される（entry ごとに正しい label で読む = doc 40 §1-1 のラベル乖離バグが構造的に不可能な
    /// 読み方）。registry に既にある conversation は store より優先（SSOT は registry）。
    #[test]
    fn load_backfills_legacy_conversations_per_entry_stand() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // registry: #1 echoes（conversation なし）/ #2 codex（conversation なし）/
        // #3 echoes（registry native の conversation あり）
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vp__conductor.json"),
            r#"{"focused":1,"root":1,"next":4,"sessions":[
                {"key":1,"stand":"echoes"},
                {"key":2,"stand":"codex"},
                {"key":3,"stand":"echoes","conversation":"native-id"}]}"#,
        )
        .unwrap();
        // legacy store: #1 は素の lane 名、#2 は `conductor#2`、#3 は stale な store 値
        crate::lane::cc_session::record_in(tmp.path(), "vp", "conductor", "cc-legacy-1")
            .expect("record cc #1");
        crate::lane::codex_session::record_in(
            tmp.path(),
            "vp",
            "conductor#2",
            "01998888-2222-7333-8444-555566667777",
        )
        .expect("record codex #2");
        crate::lane::cc_session::record_in(tmp.path(), "vp", "conductor#3", "stale-store-id")
            .expect("record cc #3");

        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("cc-legacy-1"),
            "#1 は cc store（素の lane 名）から backfill"
        );
        assert_eq!(
            reg.sessions[1].conversation.as_deref(),
            Some("01998888-2222-7333-8444-555566667777"),
            "#2 は codex store（conductor#2）から backfill"
        );
        assert_eq!(
            reg.sessions[2].conversation.as_deref(),
            Some("native-id"),
            "registry native の conversation が store より優先（SSOT）"
        );
        // file 不在（single fallback）でも #1 の backfill は効く（既存 install の移行の核）
        let reg = load_in(tmp.path(), "vp", "other-lane", "echoes");
        assert_eq!(reg.sessions[0].conversation, None, "store も無ければ None");
    }

    /// set_conversation: roundtrip 永続 / 変化なしは no-op / 不在 key は Err / 形式外は書かず。
    #[test]
    fn set_conversation_roundtrips_validates_and_rejects_unknown_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(tmp.path(), "vp", "conductor", "echoes", "echoes", true).expect("create #2");

        assert!(
            set_conversation_in(tmp.path(), "vp", "conductor", "echoes", 2, Some("id-alpha"))
                .expect("set"),
            "初回 set は disk 変化あり"
        );
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions[1].conversation.as_deref(), Some("id-alpha"));

        assert!(
            !set_conversation_in(tmp.path(), "vp", "conductor", "echoes", 2, Some("id-alpha"))
                .expect("same"),
            "同値 set は no-op（Diff::Update 不要の合図）"
        );
        assert!(
            !set_conversation_in(
                tmp.path(),
                "vp",
                "conductor",
                "echoes",
                2,
                Some("bad id'; rm")
            )
            .expect("invalid"),
            "形式外 id は書かずに Ok(false)"
        );
        assert_eq!(
            load_in(tmp.path(), "vp", "conductor", "echoes").sessions[1]
                .conversation
                .as_deref(),
            Some("id-alpha"),
            "形式外 set 後も既存値が守られる"
        );
        assert!(
            set_conversation_in(tmp.path(), "vp", "conductor", "echoes", 99, Some("id-x")).is_err(),
            "不在 key は Err"
        );
    }

    /// set_conversation(None) = clear は legacy store も消す（bridge 蘇生防止、doc 40 §9）。
    #[test]
    fn clear_conversation_also_clears_legacy_store_no_resurrection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // legacy store に会話 id → backfill で registry に見えている状態を作る
        crate::lane::cc_session::record_in(tmp.path(), "vp", "conductor", "cc-old").expect("rec");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions[0].conversation.as_deref(), Some("cc-old"));

        // save して registry に固定してから clear
        save_in(tmp.path(), "vp", "conductor", &reg).expect("save");
        assert!(
            set_conversation_in(tmp.path(), "vp", "conductor", "echoes", 1, None).expect("clear")
        );
        assert_eq!(
            load_in(tmp.path(), "vp", "conductor", "echoes").sessions[0].conversation,
            None,
            "clear 後に backfill が旧 store から蘇生させない（store も消えている）"
        );
        assert_eq!(
            crate::lane::cc_session::last_in(tmp.path(), "vp", "conductor"),
            None,
            "legacy store 側も消えている"
        );
    }

    /// doc 40 §6 policy の全 arm（record_root_conversation）。
    #[test]
    fn record_root_policies_follow_doc40_table() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rec = |sid: &str, report: ConversationReport, transcript: bool| {
            record_root_conversation_in(
                tmp.path(),
                "vp",
                "conductor",
                "echoes",
                sid,
                report,
                |_| transcript,
            )
            .expect("record_root")
        };
        let conv = || {
            let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
            let root = reg.root;
            reg.sessions
                .iter()
                .find(|s| s.key == root)
                .and_then(|s| s.conversation.clone())
        };

        // fresh（None）: Issued で即記録 = boot で chip が点く（eager の核）
        assert_eq!(
            rec("id-a", ConversationReport::Issued, false),
            RootRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-a"));

        // 同 id: no-op
        assert_eq!(
            rec("id-a", ConversationReport::Issued, true),
            RootRecordOutcome::Unchanged
        );

        // 別 id + Issued + 旧 transcript 健在 → 据え置き（F1/F2 guard: `|| claude` fallback の幻）
        assert_eq!(
            rec("id-phantom", ConversationReport::Issued, true),
            RootRecordOutcome::KeptExisting
        );
        assert_eq!(conv().as_deref(), Some("id-a"), "健在な旧会話が守られる");

        // 別 id + Spoken → 無条件で記録（user が commit した会話が勝つ）
        assert_eq!(
            rec("id-b", ConversationReport::Spoken, true),
            RootRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-b"));

        // 別 id + Issued + 旧 transcript 消滅 → 記録（幻 pointer 保持より改善）
        assert_eq!(
            rec("id-c", ConversationReport::Issued, false),
            RootRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-c"));

        // 形式外 id は書かず
        assert_eq!(
            rec("bad id'; rm", ConversationReport::Spoken, false),
            RootRecordOutcome::RejectedInvalid
        );
        assert_eq!(conv().as_deref(), Some("id-c"));

        // root が非 claude → 無視（claude hook の id を他 engine の session に混ぜない）
        create_root_in(tmp.path(), "vp", "conductor", "echoes", "codex").expect("root=codex");
        assert_eq!(
            rec("id-d", ConversationReport::Spoken, false),
            RootRecordOutcome::IgnoredNonClaude
        );
    }

    /// parse_session_label は session_label の逆関数（+ 非数値 suffix は key 1 に倒す）。
    #[test]
    fn parse_session_label_inverts_session_label() {
        assert_eq!(parse_session_label("conductor"), ("conductor", 1));
        assert_eq!(parse_session_label("conductor#2"), ("conductor", 2));
        assert_eq!(parse_session_label("feat-x#10"), ("feat-x", 10));
        assert_eq!(
            parse_session_label("a#b"),
            ("a#b", 1),
            "非数値 suffix は素の名前"
        );
        assert_eq!(parse_session_label("conductor#1"), ("conductor", 1));
        for (label, key) in [("conductor", 1u32), ("conductor", 2), ("feat-x", 10)] {
            let l = session_label(label, key);
            assert_eq!(parse_session_label(&l), (label, key), "roundtrip: {l}");
        }
    }

    /// registry file 名も sanitize が効く（session_store と同じ規則）。
    #[test]
    fn registry_file_sanitizes_project_and_lane() {
        let p = registry_file_in(Path::new("/base"), "creo.memories", "conductor");
        assert_eq!(
            p,
            Path::new("/base/echoes_sessions/creo-memories__conductor.json")
        );
        let p = registry_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/echoes_sessions/a-b__---evil.json"));
    }
}
