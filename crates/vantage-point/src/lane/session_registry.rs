//! lane ごとの Echoes session registry 永続（doc 38 — 1 Lane = N session / doc 40 — 会話 id SSOT）
//!
//! doc 38 §1 の 3 層分離の「session 層」を担う:
//!
//! ```text
//! slot（Act I の PTY）= lane の設備（1 枚）。本 module の管轄外
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
//!   書き手/読み手の乖離バグを産んだ — doc 40 §1-1）。旧 store（cc/codex_sessions）と移行
//!   bridge（backfill）は doc 40 PR-2 で退役済み — one-shot migration で全 lane の会話 id を
//!   registry へ移設した後、legacy store の record/last/clear は撤去された（`cc_session` /
//!   `codex_session` に残るのは validator / transcript helper / CLI path 解決のみ）
//! - **書き込みの直列化**: 変異（create / focus / remove / set_conversation 系）は process 内
//!   mutex で直列化する（doc 40 §4 — 複数 field JSON の並行 load-modify-save は update を失う）
//! - **root = lane の器に化身する session**（doc 39 — 座と化身）: slot spawn / wire 配送
//!   （channel D・E）/ Act I chip はすべて root に解決される。doc 38 の「slot は session #1 を
//!   既定で化身」は root=1 の特殊ケースに一般化された（#1 の特別性を撤廃）
//! - **会話報告は session 粒度**（doc 40 §4 / doc 46 P5）: hook（`vp wire hook-check`）は
//!   `VP_SESSION_KEY` で「自分がどの session か」を名乗り、[`record_conversation_in`] は
//!   **報告された session** に書く。root 固定だった時代は、同じ lane で 2 本目の claude を
//!   立てると root の会話 id が上書きされ `--resume` が同居人の会話に化けた（doc 46 §3 の
//!   producer blocker）。名乗らない報告（旧 binary / VP 外起動）は従来どおり root 宛

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session_store::sanitize;

/// session の VP 採番ローカル key（1 始まり、lane 内で単調増加・再利用しない）。
pub type SessionKey = u32;

/// session の Act（doc 46 §1.4 — 旧 `lane::console_mode::ConsoleMode` の移設先）。
///
/// **doc 47 §4 の棚卸しで判明**: 旧 `console_mode` は lane の属性でありながら
/// **3 つの仕事**を兼ねていた —
/// ① 表示の排他選択 / ② boot 時の PTY spawn 可否 / ③ wire nudge の配送分岐。
/// 「並列表示の今 Act は意味が薄い」は ① にしか当てはまらず、②③ は現役だった。
///
/// doc 46 §1.5「session ↔ Pane は 1:1」に従い Act は **session の属性**になる。
/// ②③ は **root session（器に化身する session、doc 39）の act** で決まる —
/// slot は lane に 1 枚、mailbox `agent@<lane>` を名乗るのも root だから。
///
/// > client 所有（Pane の kind）にしないのは doc 47 §0 の線を跨ぐため。
/// > 「PTY を立てるか」は**実体**の話で、見え方に決めさせると projection が逆流する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAct {
    /// Act I: PtySlot + engine の TUI（ANSI → xterm）。既定。
    #[default]
    Tui,
    /// Act II: headless engine host → EchoesEvent → ChatView。
    Chat,
}

impl SessionAct {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionAct::Tui => "tui",
            SessionAct::Chat => "chat",
        }
    }

    /// 1 行 / wire 値からパース。未知値は None（壊れた値を黙って Tui 扱いしない —
    /// 呼び手が default を選ぶ）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "tui" => Some(SessionAct::Tui),
            "chat" => Some(SessionAct::Chat),
            _ => None,
        }
    }
}

/// registry 上の 1 session。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// VP 採番のローカル key。
    pub key: SessionKey,
    /// engine 種別（stand 名: "echoes" / "codex" / "grok" / "opencode"。legacy/未知値は shell のみで graceful 吸収）。
    pub stand: String,
    /// この session の Act（doc 47 §4 で lane から移設）。serde default = Tui で
    /// file/wire 後方互換（act 無しの旧 file は従来どおり Act I として読む）。
    #[serde(default)]
    pub act: SessionAct,
    /// engine の会話 id（claude = session uuid / codex = thread id / grok・opencode = ACP sessionId）。
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
    /// doc 39: lane の器（Act I=slot / Act II=headless）に化身し、wire mailbox
    /// `agent@<lane>` を名乗る session の key（常に `sessions` 内に実在する）。
    /// slot spawn / wire 配送（channel D・E）/ Act I chip の読み先はすべてここに解決される。
    /// serde default = 1 で file/wire 後方互換（root field 無し = 従来の「#1 が slot に化身」を
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
                act: SessionAct::Tui,
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
///   Act I（slot）の hook 書き込み先と一致（doc 38 の「slot は session #1 を既定で化身」）
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
        Some(EngineKind::Codex) => super::codex_session::is_valid_thread_id(id),
        // grok = ACP sessionId（UUID v7 形 — 英数+ハイフン、doc 42 §1。registry-native なので
        // engine 別 store module を持たない = 検証だけここに置く）。
        Some(EngineKind::Grok) => {
            !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        // opencode = ACP sessionId（`ses_` prefix + 英数字。実測 `ses_089ead04bffe5oIJcQTHwwTZo8`、
        // doc 43 §1。grok 同様 registry-native なので検証だけここに置く。underscore は prefix のみ
        // で残りは英数字 = single-quote 埋め込みでも injection にならない）。
        Some(EngineKind::OpenCode) => id.strip_prefix("ses_").is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())
        }),
        // engine を持たない stand（shell / 未知 / 撤去済み cursor・agy）は会話 id を持たない。
        None => false,
    }
}

/// 新しく雇う働き手（新 root / 新 lane）の既定レンズ（doc 54 §3.1、mako 2026-07-25
/// 「われわれの ChatView 採用しちゃおうぜ」「新 root や、デフォルトの root は、chat にしよう」）。
///
/// - chat レンズを持てる engine（chat_capable）→ **Chat**（VP 自前の ChatView が既定の面）
/// - shell / 未知 stand → Tui（禁止ではなく**定義** — chat レンズには映す会話が無い）
///
/// ⚠️ これは**生成の既定**であって欠損の解釈ではない（doc 54 §3.1 の 2 つの既定の分離）。
/// registry 不在 / 旧 wire の読み fallback は従来どおり Tui（歴史的事実 — 昔の lane は tui）。
pub fn default_act_for_stand(stand: &str) -> SessionAct {
    match crate::echoes::EngineKind::from_stand(stand) {
        Some(k) if k.chat_capable() => SessionAct::Chat,
        _ => SessionAct::Tui,
    }
}

/// registry file が存在するか（= この lane が一度でも仕込みを持ったか）。
///
/// doc 54 §8-11: conductor の「初回作成」検出に使う — with_root は毎 boot 呼ばれるため、
/// 「file 不在 = 初回」を生成契機とみなして既定レンズを書く（以降の boot は既存 file を honor）。
pub fn exists_in(base: &Path, project: &str, lane: &str) -> bool {
    registry_file_in(base, project, lane).exists()
}

/// 本番 base での [`exists_in`]。
pub fn exists(project: &str, lane: &str) -> bool {
    exists_in(&crate::config::vp_state_dir(), project, lane)
}

/// registry を読む。file 不在 / 破損 / 不変条件違反は N=1 の既定形に解決（Err にしない —
/// 読めない registry で lane 全体を止めるより、既定形で動き続ける方が復旧可能性が高い）。
pub fn load_in(base: &Path, project: &str, lane: &str, default_stand: &str) -> SessionRegistry {
    // doc 40 PR-2: 会話 id は registry が唯一の SSOT（旧 engine 別 store からの backfill bridge は
    // 撤去済み — one-shot migration で移設済みのため read-only 補完は不要）。
    match std::fs::read_to_string(registry_file_in(base, project, lane)) {
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
///
/// `act` は doc 46 P2「Engine × Act を選んで新コンソール」の Act 側。root は動かさないので、
/// ここで作られる session は器（slot）に化身しない = Act II 用が既定の使い道。
pub fn create_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    act: SessionAct,
    focus: bool,
) -> std::io::Result<SessionKey> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let key = reg.next;
    reg.next += 1;
    reg.sessions.push(SessionEntry {
        key,
        stand: stand.to_string(),
        act,
        conversation: None,
    });
    if focus {
        reg.focused = key;
    }
    save_in(base, project, lane, &reg)?;
    Ok(key)
}

/// 新 session を作り、root と focused を同時にそれへ向ける（doc 39 §4 — Act I の ✨ New =
/// Root 切替「✨ 新 ID から」の shorthand）。1 回の save で書くため、器（slot）と mailbox の
/// 化身がズレる中間 state は disk に存在しない（doc 39 §0「原子的」の registry 側担保）。
/// 旧 root の session は一覧に残る（非破壊 — store も registry entry も触らない）。
pub fn create_root_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    act: SessionAct,
) -> std::io::Result<SessionKey> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let key = reg.next;
    reg.next += 1;
    reg.sessions.push(SessionEntry {
        key,
        stand: stand.to_string(),
        act,
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

/// root を既存 session へ向け替える（doc 39 P3 — Root 切替 picker）。実在しない key は
/// Err（黙って据え置くと「切替えたつもり」の slot が旧 root のまま化身する誤配送になる）。
/// focused も同じ session へ動かす（`create_root_in` と同じ「器に注意が追従する」意味論、
/// 1 save 原子）。旧 root の会話はリストに残る（非破壊）。
pub fn set_root_in(
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
    reg.root = key;
    save_in(base, project, lane, &reg)
}

/// session を 1 本取り除く（doc 38 Phase 3 — tab を閉じる）。
///
/// - 実在しない key は Err（黙って成功にしない）
/// - **root は取り除けない**（doc 39 §6。doc 38 の「最後の 1 本は取り除けない」と
///   「⚠️ #1 close は Act I slot resume を断つ」を包含する一般形 — root は常に実在するので
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
pub enum ReportTrigger {
    /// id 発行時点の eager 報告（SessionStart）。`|| claude` fallback の幻 session であり得る
    /// ため、健在な既存会話は上書きしない（F1/F2 guard — doc 40 §6 の表）。
    Issued,
    /// user が実際に話しかけた authoritative 報告（UserPromptSubmit）。無条件で記録する
    /// （user が commit した会話が常に勝つ）。
    Spoken,
}

/// [`record_conversation_in`] の結果。caller（SP handler）が log と Diff::Update push の
/// 要否判定に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRecordOutcome {
    /// 記録した（disk 変化あり）。
    Recorded,
    /// 既に同 id（no-op）。
    Unchanged,
    /// F1/F2 guard 発動: 既存会話の transcript が健在なため据え置き（Issued のみ）。
    KeptExisting,
    /// 対象 session が claude でない（claude hook の id を他 engine の session に混ぜない）。
    IgnoredNonClaude,
    /// id が形式外（書かず）。
    RejectedInvalid,
    /// 報告された session が registry に**実在しない**（書かず）。
    ///
    /// root に落とさないのが肝（doc 40 §4）。落とすと「自分が誰か分かっているが registry と
    /// ズレている報告者」の書き込みが root の会話 id を壊す — session 粒度化で塞ぎたかった
    /// 事故そのものが、fallback 経由で再現してしまう。報告者は毎ターン再報告するので、
    /// registry が追い付けば次の報告で自然に着地する。
    UnknownSession,
}

/// 会話報告の宛先 session（doc 40 §4 — hook は「自分がどの session か」を名乗る）。
///
/// **「不明」と「root」を型で区別する**のがこの enum の全存在理由。`Option<SessionKey>` を
/// 早々に `unwrap_or(root)` すると、以後「報告者が名乗らなかった」と「報告者が root を
/// 名乗った」が見分けられなくなり、実在しない session の報告を root に落とす事故
/// （[`ConversationRecordOutcome::UnknownSession`] の説明）を検知できない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportTarget {
    /// 報告者が session を名乗らなかった（session env を持たない旧 binary / VP 外で起動された
    /// claude）。**後方互換で root に記録する** — session 粒度化の前は全報告が root 宛だった。
    Unspecified,
    /// 報告者が名乗った session。実在しなければ書かない（root に落とさない）。
    Session(SessionKey),
}

/// hook が上げる 1 件の会話報告 —「**誰が**・**どの会話 id を**・**どの契機で**」。
///
/// 3 つを 1 値にまとめているのは、この 3 つが**常に同じ 1 つの出来事**を指すため
/// （別々の引数だと、target だけ差し替えて conversation を据え置く、のような
/// 「あり得ない組み合わせ」を呼び手が作れてしまう）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationReport<'a> {
    /// 宛先 session（`Unspecified` = 報告者が名乗らなかった → root 宛の後方互換）。
    pub target: ReportTarget,
    /// engine が発番した会話 id。
    pub conversation: &'a str,
    /// 報告の契機（記録 policy の分岐点 — doc 40 §6 の表）。
    pub trigger: ReportTrigger,
}

/// slot（claude hook）の会話報告を**報告された session** に適用する — doc 40 §6 policy の
/// **唯一の実装点**。旧「UserPromptSubmit のみ記録」（#795 の鈍器）の置換。
///
/// policy（F1/F2 guard / engine 判定 / 形式検証）は doc 40 §6 の表そのままで、**書き先だけが
/// root 固定から報告 session になった**（doc 46 P5 の「1 lane に複数 console slot」を production で
/// 立てるための前提 — 同じ lane の 2 本目の claude が root の会話 id を上書きしなくなる）。
///
/// 直書き（Act II host の record-from-init）は [`set_conversation_in`]。こちらは policy を持たない
/// authoritative な書き込みで、報告経路とは別物。
///
/// `transcript_exists` は注入する（テストが実 `~/.claude` に依存しないため。本番 wrapper
/// [`record_conversation`] が `cc_session::transcript_exists` を渡す）。
pub fn record_conversation_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    report: ConversationReport<'_>,
    transcript_exists: impl Fn(&str) -> bool,
) -> std::io::Result<ConversationRecordOutcome> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let session_id = report.conversation;
    let key = match report.target {
        // 名乗らなかった報告は root 宛（session 粒度化前の唯一の宛先 = 後方互換）。
        ReportTarget::Unspecified => reg.root,
        ReportTarget::Session(k) => k,
    };
    let Some(entry) = reg.sessions.iter_mut().find(|s| s.key == key) else {
        // Unspecified で来た場合は is_valid が root 実在を保証するため到達しない。
        // Session(k) の不在はここ = **root に落とさず** UnknownSession で返す。
        return Ok(ConversationRecordOutcome::UnknownSession);
    };
    if !matches!(
        crate::echoes::EngineKind::from_stand(&entry.stand),
        Some(crate::echoes::EngineKind::Claude)
    ) {
        return Ok(ConversationRecordOutcome::IgnoredNonClaude);
    }
    if !is_valid_conversation(&entry.stand, session_id) {
        return Ok(ConversationRecordOutcome::RejectedInvalid);
    }
    match &entry.conversation {
        Some(cur) if cur == session_id => Ok(ConversationRecordOutcome::Unchanged),
        Some(cur) if report.trigger == ReportTrigger::Issued && transcript_exists(cur) => {
            // resume 失敗 `|| claude` fallback の幻 session が、健在な旧会話への復帰路を
            // 上書きするのを防ぐ（F1 clobber / F2 幻ポインタの再演防止）。次の Spoken で
            // user が幻側に commit したら上書きされる（self-heal）。
            Ok(ConversationRecordOutcome::KeptExisting)
        }
        _ => {
            entry.conversation = Some(session_id.to_string());
            save_in(base, project, lane, &reg)?;
            Ok(ConversationRecordOutcome::Recorded)
        }
    }
}

/// session の会話 id を書く（doc 40 §4 — Act II host の record-from-init / cursor create-chat
/// 採番の書き込み口。SP プロセス内から呼ぶ）。
///
/// - 実在しない key は Err（黙って捨てると「記録したつもり」の幻 resume になる）
/// - 形式外 id は**書かずに** Ok(false)（旧 session_store の共通原則を引き継ぐ）
/// - 変化なしは save しない。戻り値 = 「disk が変わったか」（caller の Diff::Update 判定用）
/// - `None`（clear）は entry.conversation を None に落とすだけ（doc 40 PR-2 で backfill bridge が
///   撤去されたため、旧 engine store の併せ消し = 蘇生防止は不要になった）
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
/// slot spawn（`stand_spawner`）/ channel D enrich のような「registry 全体は要らない」経路用。
pub fn root_in(base: &Path, project: &str, lane: &str) -> SessionKey {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return 1;
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg.root,
        _ => 1,
    }
}

/// **root session の act** だけを軽量に読む（file 不在 / 破損は Tui = 従来の既定）。
///
/// doc 47 §4: 旧 `console_mode::last()` の後継。lane の器（slot / mailbox）に化身するのは
/// root なので、「PTY を立てるか」「nudge をどちらの method で送るか」は root の act で決まる。
pub fn root_act_in(base: &Path, project: &str, lane: &str) -> SessionAct {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return SessionAct::Tui;
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg
            .sessions
            .iter()
            .find(|s| s.key == reg.root)
            .map(|s| s.act)
            .unwrap_or_default(),
        _ => SessionAct::Tui,
    }
}

/// root session の act を書き替える（doc 47 §4 — 旧 `console_mode::record()` の後継）。
///
/// 戻り値は「実際に変わったか」。変化なしで save を走らせない（disk write を減らすためでなく、
/// 「切替えた」ログが変化と 1:1 になるようにするため）。
pub fn set_root_act_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    act: SessionAct,
) -> std::io::Result<bool> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let root = reg.root;
    let Some(entry) = reg.sessions.iter_mut().find(|s| s.key == root) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("root session が存在しません（project={project}, lane={lane}, root={root}）"),
        ));
    };
    if entry.act == act {
        return Ok(false);
    }
    entry.act = act;
    save_in(base, project, lane, &reg)?;
    Ok(true)
}

/// 指定 session の act を書き替える（doc 50 §4.6 A6 — [`set_root_act_in`] の session 一般化）。
///
/// root を渡せば `set_root_act_in` と同義（root は session の 1 つ）。session = Pane の
/// act badge（World B）が root 以外の pane も切り替えられるようになったため、任意 session の
/// act を永続する入口が要る。戻り値は「実際に変わったか」（`set_root_act_in` と同じく
/// 「切替えた」ログを変化と 1:1 にする）。session 不在は Err。
pub fn set_session_act_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    session: SessionKey,
    act: SessionAct,
) -> std::io::Result<bool> {
    let _guard = mutation_guard();
    let mut reg = load_in(base, project, lane, default_stand);
    let Some(entry) = reg.sessions.iter_mut().find(|s| s.key == session) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session が存在しません（project={project}, lane={lane}, session={session}）"),
        ));
    };
    if entry.act == act {
        return Ok(false);
    }
    entry.act = act;
    save_in(base, project, lane, &reg)?;
    Ok(true)
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

/// **既定形（N=1）に書き戻す**（Reset = lane を素に戻す動詞の registry 側）。
///
/// [`clear_in`] との違いは **file を残すこと**。lane が生き続ける Reset ではこちらを使う:
///
/// - `clear_in` は file ごと消すので、その後の [`load_in`] は `SessionRegistry::single()` の
///   **act=Tui 固定**に倒れる（「壊れていたら保守的に Tui」の判断）。一方 `with_root` は
///   「file 不在 = 初回」と見て**既定レンズ**（chat_capable なら Chat）を書く。この 2 つの
///   既定が食い違うので、**file 不在の lane は観測者によって型が変わる**
/// - 「消してから書き戻す」も同じ穴を踏む: [`set_root_act_in`] は「値が同じなら save しない」
///   最適化を持ち、その前提（disk が既に正しい）は clear 直後には成り立たない。**Tui へ戻す
///   ケースだけ save がスキップされ file が不在のまま残る**（team-b 指摘 2026-07-26）
///
/// だから Reset は **1 回の save で既定形を確定させる**（不在の窓を作らない）。
/// lane 自体を消す GC（`clear_lane_state_in`）は file を残す理由が無いので `clear_in` のまま。
pub fn reset_to_single_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    act: SessionAct,
) -> std::io::Result<()> {
    let _guard = mutation_guard();
    let mut reg = SessionRegistry::single(default_stand);
    // `single()` は act=Tui 固定なので、呼び手の意図（Reset 直前の act）を必ず上書きする。
    if let Some(root) = reg.sessions.first_mut() {
        root.act = act;
    }
    save_in(base, project, lane, &reg)
}

// ---- 本番 base（vp_state_dir）での wrapper ----

/// 本番 base での [`reset_to_single_in`]。
pub fn reset_to_single(
    project: &str,
    lane: &str,
    default_stand: &str,
    act: SessionAct,
) -> std::io::Result<()> {
    reset_to_single_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        act,
    )
}

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
    act: SessionAct,
    focus: bool,
) -> std::io::Result<SessionKey> {
    create_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        stand,
        act,
        focus,
    )
}

/// 本番 base での create_root。
pub fn create_root(
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    act: SessionAct,
) -> std::io::Result<SessionKey> {
    create_root_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        stand,
        act,
    )
}

/// 本番 base での root_act（旧 `console_mode::last` の後継）。
pub fn root_act(project: &str, lane: &str) -> SessionAct {
    root_act_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での set_root_act（旧 `console_mode::record` の後継）。
pub fn set_root_act(
    project: &str,
    lane: &str,
    default_stand: &str,
    act: SessionAct,
) -> std::io::Result<bool> {
    set_root_act_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        act,
    )
}

/// 本番 base での set_session_act（doc 50 §4.6 A6）。
pub fn set_session_act(
    project: &str,
    lane: &str,
    default_stand: &str,
    session: SessionKey,
    act: SessionAct,
) -> std::io::Result<bool> {
    set_session_act_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        session,
        act,
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

/// 本番 base での set_root。
pub fn set_root(
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<()> {
    set_root_in(
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

/// 本番 base での record_conversation（SP の hook 報告 handler から呼ぶ）。
/// F1/F2 guard の transcript 判定は claude の実 transcript（`cc_session::transcript_exists`）。
pub fn record_conversation(
    project: &str,
    lane: &str,
    default_stand: &str,
    report: ConversationReport<'_>,
) -> std::io::Result<ConversationRecordOutcome> {
    record_conversation_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        report,
        super::cc_session::transcript_exists,
    )
}

/// 旧 `console_modes/<project>__<lane>` を **root session の act** へ畳む one-shot migration
/// （doc 47 §4 — Act の lane → session 移設）。戻り値は畳んだ lane 数。
///
/// Daemon boot で 1 回だけ呼ぶ。畳み終えたら legacy file / dir ごと消えるので 2 回目以降は
/// no-op（`clear_in` と同じ「終端まで進めて痕跡を残さない」規律）。
///
/// - `Tui` の記録は**書かずに捨てる** — Tui は既定なので、registry file を持たない lane に
///   わざわざ file を作る意味がない（migration が state を増やさない）
/// - file 名は sanitize 済みだが、`registry_file_in` が同じ sanitize を通すので往復は安定
///   （sanitize は冪等 — `/`・`\`・`.` を `-` に潰すだけ）
pub fn migrate_console_modes_in(base: &Path) -> usize {
    let dir = base.join("console_modes");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut migrated = 0usize;
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // `<project>__<lane>`。lane 名に `__` は入り得ないので最初の `__` で割る。
        let Some((project, lane)) = name.split_once("__") else {
            continue;
        };
        let act = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| SessionAct::parse(&raw));
        if act == Some(SessionAct::Chat) {
            // default_stand は lane の stand 記録から引く（無ければ echoes = 従来既定）。
            let default_stand = super::stand_store::last_in(base, project, lane)
                .unwrap_or_else(|| "echoes".to_string());
            match set_root_act_in(base, project, lane, &default_stand, SessionAct::Chat) {
                Ok(_) => migrated += 1,
                Err(err) => {
                    tracing::warn!(
                        "console_mode migration 失敗（残置）: project={project} lane={lane} err={err}"
                    );
                    continue;
                }
            }
        }
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!("console_mode migration: legacy file の削除失敗: {err}");
        }
    }
    // 空になった dir を畳む（残っていても無害だが、次回 boot の read_dir を空振りさせない）。
    let _ = std::fs::remove_dir(&dir);
    if migrated > 0 {
        tracing::info!("console_mode → session act migration: {migrated} lane を畳んだ");
    }
    migrated
}

/// 本番 base での console_mode migration（Daemon boot から 1 回）。
pub fn migrate_console_modes() -> usize {
    migrate_console_modes_in(&crate::config::vp_state_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// file 不在 = N=1 の特殊ケース（lane の stand で session #1・focused=1・root=1）。
    /// 既存 install が registry file 無しで従来どおり動くことの根拠。
    #[test]
    fn load_without_file_resolves_to_single_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(
            reg,
            SessionRegistry {
                focused: 1,
                root: 1,
                next: 2,
                sessions: vec![SessionEntry {
                    key: 1,
                    stand: "echoes".to_string(),
                    act: SessionAct::Tui,
                    conversation: None,
                }],
            }
        );
        assert_eq!(focused_in(tmp.path(), "vp", "root"), 1);
        assert_eq!(root_in(tmp.path(), "vp", "root"), 1);
    }

    /// doc 39 P1 の後方互換の核: root field を持たない既存 file は root=1 として読める
    /// （serde default）。既存 install の registry を壊さず root を導入できる根拠。
    #[test]
    fn registry_without_root_field_reads_as_root_1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vp__root.json"),
            r#"{"focused":2,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":2,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.root, 1, "root 無し file は root=1（従来の #1 化身）");
        assert_eq!(reg.focused, 2, "focused は file の値を維持");
        assert_eq!(root_in(tmp.path(), "vp", "root"), 1);
    }

    /// create → 採番 2・focus 追随 → roundtrip 永続。focus=false は focused を据え置く。
    #[test]
    fn create_assigns_monotonic_keys_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let k2 = create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "codex",
            SessionAct::Chat,
            true,
        )
        .expect("create");
        assert_eq!(k2, 2);
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.focused, 2, "focus=true は新 session に focus が移る");
        assert_eq!(reg.sessions.len(), 2);
        assert_eq!(reg.sessions[0].stand, "echoes", "session #1 は lane stand");
        assert_eq!(reg.sessions[1].stand, "codex");

        let k3 = create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Chat,
            false,
        )
        .expect("create");
        assert_eq!(k3, 3);
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.focused, 2, "focus=false は focused を動かさない");
        assert_eq!(reg.next, 4);
    }

    /// focus は実在 key のみ受理。不在 key は Err（黙って据え置かない）。
    #[test]
    fn focus_rejects_unknown_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "codex",
            SessionAct::Chat,
            false,
        )
        .expect("create");
        focus_in(tmp.path(), "vp", "root", "echoes", 2).expect("実在 key への focus");
        assert_eq!(focused_in(tmp.path(), "vp", "root"), 2);
        assert!(
            focus_in(tmp.path(), "vp", "root", "echoes", 99).is_err(),
            "不在 key への focus は Err"
        );
    }

    /// 破損 file / 不変条件違反は既定形に解決（壊れた state で動き続けない）。
    #[test]
    fn corrupt_or_invalid_file_falls_back_to_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("vp__root.json");

        // 非 JSON
        std::fs::write(&file, "not json").unwrap();
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.focused, 1);

        // focused が不在 key（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":9,"next":3,"sessions":[{"key":1,"stand":"echoes"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.focused, 1, "focused 不在の file は既定形に解決");
        assert_eq!(focused_in(tmp.path(), "vp", "root"), 1);

        // key 重複（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":1,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":1,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.sessions.len(), 1, "key 重複の file は既定形に解決");

        // root が不在 key（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":1,"root":9,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":2,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.root, 1, "root 不在の file は既定形に解決");
        assert_eq!(root_in(tmp.path(), "vp", "root"), 1);
    }

    /// remove: 実在検証 / root は拒否（doc 39 §6 — 最後の 1 本の拒否を包含）/
    /// focused fallback は残りの先頭 / key 再利用なし。
    #[test]
    fn remove_validates_and_moves_focus_deterministically() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // #2(codex, focused) / #3(cursor) を追加 → #1/#2/#3
        create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "codex",
            SessionAct::Chat,
            true,
        )
        .expect("create #2");
        create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "cursor",
            SessionAct::Chat,
            false,
        )
        .expect("create #3");

        // 不在 key は Err
        assert!(remove_in(tmp.path(), "vp", "root", "echoes", 9).is_err());

        // root(#1) は N>1 でも取り除けない（doc 38 の「⚠️ #1 close は Act I slot resume を断つ」
        // footgun を構造で塞ぐ — doc 39 §2）
        assert!(
            remove_in(tmp.path(), "vp", "root", "echoes", 1).is_err(),
            "root session の remove は Err"
        );

        // focused(#2) を remove → focus は残りの先頭(#1) へ
        let focused = remove_in(tmp.path(), "vp", "root", "echoes", 2).expect("remove #2");
        assert_eq!(focused, 1, "focused の remove は残り先頭へ fallback");
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(
            reg.sessions.iter().map(|s| s.key).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(reg.next, 4, "採番は据え置き（key 再利用なし）");

        // 非 focused(#3) を remove → focus は不変
        let focused = remove_in(tmp.path(), "vp", "root", "echoes", 3).expect("remove #3");
        assert_eq!(focused, 1);

        // 最後の 1 本 = root なので取り除けない（fresh restart が正道）
        assert!(
            remove_in(tmp.path(), "vp", "root", "echoes", 1).is_err(),
            "最後の session の remove は Err"
        );
    }

    /// create_root: 新 session に root + focused が同時に移り、旧 root は一覧に残る（doc 39 §4
    /// Act I New = 非破壊）。旧 root（#1）は非 root になったので remove 可能になる。
    #[test]
    fn create_root_moves_root_and_focus_atomically() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let k2 = create_root_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Tui,
        )
        .expect("create_root #2");
        assert_eq!(k2, 2);
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.root, 2, "root は新 session へ");
        assert_eq!(reg.focused, 2, "focused も新 session へ（追加して focus）");
        assert_eq!(reg.sessions.len(), 2, "旧 root(#1) は一覧に残る（非破壊）");
        assert_eq!(root_in(tmp.path(), "vp", "root"), 2);

        // 旧 root(#1) は非 root になったので閉じられる（doc 39 §2 — #1 の特別性撤廃）
        let focused = remove_in(tmp.path(), "vp", "root", "echoes", 1).expect("remove #1");
        assert_eq!(focused, 2);
        // 新 root(#2) は取り除けない
        assert!(remove_in(tmp.path(), "vp", "root", "echoes", 2).is_err());
    }

    /// set_root: 既存 session へ root + focused が同時に移る（doc 39 P3 Root 切替 = 非破壊）。
    /// 不在 key は Err。切替後、旧 root は非 root になり remove 可能。
    #[test]
    fn set_root_switches_to_existing_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // #2 を作って root にする（#1 は残存）→ 既存の #1 へ切り戻す
        create_root_in(tmp.path(), "vp", "root", "echoes", "codex", SessionAct::Tui)
            .expect("create_root #2");
        set_root_in(tmp.path(), "vp", "root", "echoes", 1).expect("set_root #1");
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.root, 1, "root は既存 #1 へ");
        assert_eq!(reg.focused, 1, "focused も追従（create_root と同じ意味論）");
        assert_eq!(reg.sessions.len(), 2, "非破壊 — 両 session が一覧に残る");
        // 旧 root(#2) は非 root になったので閉じられる
        remove_in(tmp.path(), "vp", "root", "echoes", 2).expect("remove #2");
        // 不在 key への切替は Err（黙って据え置かない）
        assert!(set_root_in(tmp.path(), "vp", "root", "echoes", 99).is_err());
    }

    /// clear = fresh reset。file が消えて既定形に戻り、採番も 1 からやり直し。冪等。
    #[test]
    fn clear_resets_to_default_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "codex",
            SessionAct::Chat,
            true,
        )
        .expect("create");
        clear_in(tmp.path(), "vp", "root").expect("clear");
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.next, 2, "fresh 後は採番もやり直し");
        // 未記録の clear は no-op（session_store と同じ原則）
        clear_in(tmp.path(), "vp", "root").expect("二重 clear は Ok");
    }

    /// session label: key 1 = 素の lane 名（既存 file 互換）、key 2+ = `<lane>#<n>`。
    /// `#` は sanitize で置換されない = session_store の file 名にそのまま安全に使える
    /// （doc 36 実証の固定化）。
    #[test]
    fn session_label_is_bare_for_key1_and_hash_suffixed_after() {
        assert_eq!(session_label("root", 1), "root");
        assert_eq!(session_label("root", 2), "root#2");
        assert_eq!(session_label("feat-x", 10), "feat-x#10");
        assert_eq!(sanitize("root#2"), "root#2", "# は sanitize 安全");
    }

    /// set_conversation: roundtrip 永続 / 変化なしは no-op / 不在 key は Err / 形式外は書かず。
    #[test]
    fn set_conversation_roundtrips_validates_and_rejects_unknown_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(
            tmp.path(),
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Chat,
            true,
        )
        .expect("create #2");

        assert!(
            set_conversation_in(tmp.path(), "vp", "root", "echoes", 2, Some("id-alpha"))
                .expect("set"),
            "初回 set は disk 変化あり"
        );
        let reg = load_in(tmp.path(), "vp", "root", "echoes");
        assert_eq!(reg.sessions[1].conversation.as_deref(), Some("id-alpha"));

        assert!(
            !set_conversation_in(tmp.path(), "vp", "root", "echoes", 2, Some("id-alpha"))
                .expect("same"),
            "同値 set は no-op（Diff::Update 不要の合図）"
        );
        assert!(
            !set_conversation_in(tmp.path(), "vp", "root", "echoes", 2, Some("bad id'; rm"))
                .expect("invalid"),
            "形式外 id は書かずに Ok(false)"
        );
        assert_eq!(
            load_in(tmp.path(), "vp", "root", "echoes").sessions[1]
                .conversation
                .as_deref(),
            Some("id-alpha"),
            "形式外 set 後も既存値が守られる"
        );
        assert!(
            set_conversation_in(tmp.path(), "vp", "root", "echoes", 99, Some("id-x")).is_err(),
            "不在 key は Err"
        );
    }

    /// is_valid_conversation の engine 別形式（doc 43 §6: opencode = `ses_` prefix + 英数字）。
    #[test]
    fn is_valid_conversation_per_engine_form() {
        // opencode: 実測形式（doc 43 §1）は valid、prefix 欠落 / 空 rest / injection 形は reject。
        assert!(is_valid_conversation(
            "opencode",
            "ses_089ead04bffe5oIJcQTHwwTZo8"
        ));
        assert!(
            !is_valid_conversation("opencode", "089ead04"),
            "ses_ prefix 必須"
        );
        assert!(
            !is_valid_conversation("opencode", "ses_"),
            "rest が空は不可"
        );
        assert!(
            !is_valid_conversation("opencode", "ses_bad'; rm"),
            "quote 破りは reject（single-quote 埋め込み防壁）"
        );
        // grok は英数 + ハイフン（opencode の underscore 付き id は grok では不可 = 別ルール）。
        assert!(is_valid_conversation("grok", "0199a2ff-eeee-7abc"));
        assert!(!is_valid_conversation("grok", "ses_089ead04"));
        // engine を持たない stand（shell / 撤去済み）は会話 id を持たない。
        assert!(!is_valid_conversation("shell", "ses_089ead04"));
    }

    /// set_conversation(None) = clear は entry.conversation を None に落とす（doc 40 PR-2 —
    /// backfill bridge 撤去後は「次 load での蘇生」が構造的に起こらないため、registry の
    /// conversation を None にするだけで閉じた会話が復活しない）。
    #[test]
    fn clear_conversation_resets_entry_to_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // registry に会話 id を記録した状態を作る
        assert!(
            set_conversation_in(tmp.path(), "vp", "root", "echoes", 1, Some("cc-old"))
                .expect("set")
        );
        assert_eq!(
            load_in(tmp.path(), "vp", "root", "echoes").sessions[0]
                .conversation
                .as_deref(),
            Some("cc-old")
        );

        // clear → conversation が None に落ち、再 load でも蘇らない
        assert!(set_conversation_in(tmp.path(), "vp", "root", "echoes", 1, None).expect("clear"));
        assert_eq!(
            load_in(tmp.path(), "vp", "root", "echoes").sessions[0].conversation,
            None,
            "clear 後は conversation が None（backfill 蘇生源は存在しない）"
        );
    }

    /// doc 40 §6 policy の全 arm（`record_conversation` の Unspecified = 従来の root 宛報告）。
    #[test]
    fn record_root_policies_follow_doc40_table() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rec = |sid: &str, trigger: ReportTrigger, transcript: bool| {
            record_conversation_in(
                tmp.path(),
                "vp",
                "root",
                "echoes",
                ConversationReport {
                    target: ReportTarget::Unspecified,
                    conversation: sid,
                    trigger,
                },
                |_| transcript,
            )
            .expect("record_conversation")
        };
        let conv = || {
            let reg = load_in(tmp.path(), "vp", "root", "echoes");
            let root = reg.root;
            reg.sessions
                .iter()
                .find(|s| s.key == root)
                .and_then(|s| s.conversation.clone())
        };

        // fresh（None）: Issued で即記録 = boot で chip が点く（eager の核）
        assert_eq!(
            rec("id-a", ReportTrigger::Issued, false),
            ConversationRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-a"));

        // 同 id: no-op
        assert_eq!(
            rec("id-a", ReportTrigger::Issued, true),
            ConversationRecordOutcome::Unchanged
        );

        // 別 id + Issued + 旧 transcript 健在 → 据え置き（F1/F2 guard: `|| claude` fallback の幻）
        assert_eq!(
            rec("id-phantom", ReportTrigger::Issued, true),
            ConversationRecordOutcome::KeptExisting
        );
        assert_eq!(conv().as_deref(), Some("id-a"), "健在な旧会話が守られる");

        // 別 id + Spoken → 無条件で記録（user が commit した会話が勝つ）
        assert_eq!(
            rec("id-b", ReportTrigger::Spoken, true),
            ConversationRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-b"));

        // 別 id + Issued + 旧 transcript 消滅 → 記録（幻 pointer 保持より改善）
        assert_eq!(
            rec("id-c", ReportTrigger::Issued, false),
            ConversationRecordOutcome::Recorded
        );
        assert_eq!(conv().as_deref(), Some("id-c"));

        // 形式外 id は書かず
        assert_eq!(
            rec("bad id'; rm", ReportTrigger::Spoken, false),
            ConversationRecordOutcome::RejectedInvalid
        );
        assert_eq!(conv().as_deref(), Some("id-c"));

        // root が非 claude → 無視（claude hook の id を他 engine の session に混ぜない）
        create_root_in(tmp.path(), "vp", "root", "echoes", "codex", SessionAct::Tui)
            .expect("root=codex");
        assert_eq!(
            rec("id-d", ReportTrigger::Spoken, false),
            ConversationRecordOutcome::IgnoredNonClaude
        );
    }

    // ---- doc 40 §4 / doc 46 P5: 会話報告の session 粒度化 ----

    /// **本 PR の核心**: 非 root session の hook 報告は、root の会話 id を上書きしない。
    ///
    /// doc 46 §3 が producer の blocker として挙げた事故そのもの — 同じ lane に 2 本目の
    /// claude が立つと、その SessionStart が root の会話 id を潰し、root の `--resume` が
    /// 同居人の会話に化ける。report 先を session 粒度にすることで**構造的に起こせなく**なる。
    #[test]
    fn non_root_report_does_not_clobber_root_conversation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        // root(#1) は会話 id を持っており（発話済み）、同居人 #2 が新たに立った状況。
        set_conversation_in(base, "vp", "root", "echoes", 1, Some("root-conv")).expect("root conv");
        let k2 = create_in(
            base,
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Tui,
            false,
        )
        .expect("create #2");
        assert_eq!(k2, 2);

        // #2 の claude が自分の会話 id を報告（transcript 健在 = 旧実装なら F1 guard が
        // 効いて root が守られたように見えるが、Spoken では素通りして root を潰していた）。
        let outcome = record_conversation_in(
            base,
            "vp",
            "root",
            "echoes",
            ConversationReport {
                target: ReportTarget::Session(2),
                conversation: "roommate-conv",
                trigger: ReportTrigger::Spoken,
            },
            |_| true,
        )
        .expect("record");
        assert_eq!(outcome, ConversationRecordOutcome::Recorded);

        let reg = load_in(base, "vp", "root", "echoes");
        assert_eq!(reg.root, 1, "root は動かない（報告は root を移さない）");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("root-conv"),
            "root の会話 id は同居人の報告で上書きされない（本 PR の核心）"
        );
        assert_eq!(
            reg.sessions[1].conversation.as_deref(),
            Some("roommate-conv"),
            "報告した本人の session に着地する"
        );
    }

    /// 実在しない session の報告は**書かない**（root に落とさない）。
    ///
    /// 「不明だから root」にすると、session を名乗れる報告者の取り違えが root の会話を壊す —
    /// session 粒度化で消したかった事故が fallback 経由で蘇る。
    #[test]
    fn report_for_unknown_session_is_not_folded_into_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        set_conversation_in(base, "vp", "root", "echoes", 1, Some("root-conv")).expect("root conv");

        let outcome = record_conversation_in(
            base,
            "vp",
            "root",
            "echoes",
            ConversationReport {
                target: ReportTarget::Session(9),
                conversation: "ghost-conv",
                trigger: ReportTrigger::Spoken,
            },
            |_| false,
        )
        .expect("record");
        assert_eq!(outcome, ConversationRecordOutcome::UnknownSession);

        let reg = load_in(base, "vp", "root", "echoes");
        assert_eq!(reg.sessions.len(), 1, "session は増えない");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("root-conv"),
            "実在しない session の報告は root に化けない"
        );
    }

    /// 後方互換: session を名乗らない報告（session env の無い旧 binary / VP 外で起動された
    /// claude）は従来どおり root に記録される。root が #2 に移っていても root 追従。
    #[test]
    fn unspecified_report_still_records_into_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        // root を #2 へ移す（Act I ✨ New 相当）。旧 root(#1) は残る。
        let k2 = create_root_in(base, "vp", "root", "echoes", "echoes", SessionAct::Tui)
            .expect("create_root #2");
        assert_eq!(k2, 2);

        let outcome = record_conversation_in(
            base,
            "vp",
            "root",
            "echoes",
            ConversationReport {
                target: ReportTarget::Unspecified,
                conversation: "legacy-conv",
                trigger: ReportTrigger::Spoken,
            },
            |_| false,
        )
        .expect("record");
        assert_eq!(outcome, ConversationRecordOutcome::Recorded);

        let reg = load_in(base, "vp", "root", "echoes");
        assert_eq!(reg.sessions[0].conversation, None, "旧 root(#1) は無傷");
        assert_eq!(
            reg.sessions[1].conversation.as_deref(),
            Some("legacy-conv"),
            "名乗らない報告は現 root（#2）へ = session 粒度化前と同じ着地"
        );
    }

    /// `ReportTarget::Session(root)` と `Unspecified` は同じ session に着地する
    /// （= 名乗った root と名乗らなかった報告の**結果**は一致する。区別しているのは
    /// 「実在しない session を root に落とさない」ためであって、root 宛の意味を変えるためではない）。
    #[test]
    fn explicit_root_report_matches_unspecified() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let root = load_in(base, "vp", "root", "echoes").root;
        let outcome = record_conversation_in(
            base,
            "vp",
            "root",
            "echoes",
            ConversationReport {
                target: ReportTarget::Session(root),
                conversation: "explicit-root",
                trigger: ReportTrigger::Issued,
            },
            |_| false,
        )
        .expect("record");
        assert_eq!(outcome, ConversationRecordOutcome::Recorded);
        assert_eq!(
            load_in(base, "vp", "root", "echoes").sessions[0]
                .conversation
                .as_deref(),
            Some("explicit-root")
        );
    }

    /// F1/F2 guard は session 粒度でも同じ policy で効く（doc 40 §6 の表は書き先が変わっても不変）。
    #[test]
    fn f1_f2_guard_applies_per_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        create_in(
            base,
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Tui,
            false,
        )
        .expect("create #2");
        set_conversation_in(base, "vp", "root", "echoes", 2, Some("live-conv")).expect("#2 conv");

        // Issued + 旧 transcript 健在 → 据え置き（`|| claude` fallback の幻から守る）
        let outcome = record_conversation_in(
            base,
            "vp",
            "root",
            "echoes",
            ConversationReport {
                target: ReportTarget::Session(2),
                conversation: "phantom-conv",
                trigger: ReportTrigger::Issued,
            },
            |_| true,
        )
        .expect("record");
        assert_eq!(outcome, ConversationRecordOutcome::KeptExisting);
        assert_eq!(
            load_in(base, "vp", "root", "echoes").sessions[1]
                .conversation
                .as_deref(),
            Some("live-conv"),
            "非 root session でも健在な会話が守られる"
        );
    }

    /// parse_session_label は session_label の逆関数（+ 非数値 suffix は key 1 に倒す）。
    #[test]
    fn parse_session_label_inverts_session_label() {
        assert_eq!(parse_session_label("root"), ("root", 1));
        assert_eq!(parse_session_label("root#2"), ("root", 2));
        assert_eq!(parse_session_label("feat-x#10"), ("feat-x", 10));
        assert_eq!(
            parse_session_label("a#b"),
            ("a#b", 1),
            "非数値 suffix は素の名前"
        );
        assert_eq!(parse_session_label("root#1"), ("root", 1));
        for (label, key) in [("root", 1u32), ("root", 2), ("feat-x", 10)] {
            let l = session_label(label, key);
            assert_eq!(parse_session_label(&l), (label, key), "roundtrip: {l}");
        }
    }

    /// registry file 名も sanitize が効く（session_store と同じ規則）。
    #[test]
    fn registry_file_sanitizes_project_and_lane() {
        let p = registry_file_in(Path::new("/base"), "creo.memories", "root");
        assert_eq!(
            p,
            Path::new("/base/echoes_sessions/creo-memories__root.json")
        );
        let p = registry_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/echoes_sessions/a-b__---evil.json"));
    }

    // ---- doc 47 §4: Act の lane → session 移設 ----

    /// root の act の往復。**root を向け替えたら act も新 root のものになる**ことまで固定する
    /// （旧 `console_mode` は lane 単位だったので、この区別が存在しなかった）。
    #[test]
    fn root_act_follows_the_root_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        // 未記録は Tui（file 不在 = 従来の既定）
        assert_eq!(root_act_in(base, "vp", "root"), SessionAct::Tui);

        assert!(
            set_root_act_in(base, "vp", "root", "echoes", SessionAct::Chat).expect("set"),
            "変化したので true"
        );
        assert_eq!(root_act_in(base, "vp", "root"), SessionAct::Chat);
        assert!(
            !set_root_act_in(base, "vp", "root", "echoes", SessionAct::Chat).expect("set 2"),
            "同値は no-op（false）"
        );

        // #2 を Tui で作って root を移すと、root の act も #2 のものになる。
        let k2 = create_in(
            base,
            "vp",
            "root",
            "echoes",
            "echoes",
            SessionAct::Tui,
            false,
        )
        .expect("create #2");
        set_root_in(base, "vp", "root", "echoes", k2).expect("set_root");
        assert_eq!(
            root_act_in(base, "vp", "root"),
            SessionAct::Tui,
            "act は lane ではなく root session に付く"
        );
    }

    /// act 無しの旧 file は Tui として読める（serde default の後方互換）。
    #[test]
    fn legacy_registry_without_act_reads_as_tui() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let path = registry_file_in(base, "vp", "root");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"focused":1,"root":1,"next":2,"sessions":[{"key":1,"stand":"echoes"}]}"#,
        )
        .unwrap();
        assert_eq!(root_act_in(base, "vp", "root"), SessionAct::Tui);
        let reg = load_in(base, "vp", "root", "echoes");
        assert_eq!(reg.sessions[0].act, SessionAct::Tui);
    }

    /// doc 54 §3.1: 生成の既定レンズ。chat_capable な engine は Chat、shell / 未知は Tui
    /// （定義 — chat レンズには映す会話が無い）。「生成の既定」と「欠損の解釈（Tui）」は
    /// 別の問い — 後者は上の root_act fallback テスト群が固定している。
    #[test]
    fn default_act_for_stand_is_chat_for_engines_tui_for_shell() {
        for engine in ["echoes", "hd", "codex", "grok", "opencode"] {
            assert_eq!(
                default_act_for_stand(engine),
                SessionAct::Chat,
                "engine {engine} の既定レンズは Chat（われわれの ChatView）"
            );
        }
        for non_engine in ["shell", "tmux", "cursor", "agy", "unknown-stand", ""] {
            assert_eq!(
                default_act_for_stand(non_engine),
                SessionAct::Tui,
                "{non_engine:?} は chat レンズを持てない = Tui（定義）"
            );
        }
    }

    /// one-shot migration: 旧 `console_modes/` の chat 記録が root の act に畳まれ、
    /// legacy file / dir ごと消える。Tui は既定なので registry を作らずに捨てる。
    #[test]
    fn migrate_console_modes_folds_chat_into_root_act() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let dir = base.join("console_modes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__root"), "chat").unwrap();
        std::fs::write(dir.join("vp__feat"), "tui").unwrap();
        std::fs::write(dir.join("vp__broken"), "gui").unwrap(); // 未知値は捨てる

        assert_eq!(migrate_console_modes_in(base), 1, "畳んだのは chat の 1 件");

        assert_eq!(root_act_in(base, "vp", "root"), SessionAct::Chat);
        assert_eq!(root_act_in(base, "vp", "feat"), SessionAct::Tui);
        assert!(
            !registry_file_in(base, "vp", "feat").exists(),
            "Tui は既定なので registry file を作らない（migration が state を増やさない）"
        );
        assert!(!dir.exists(), "legacy dir ごと消える");
        // 2 回目は no-op（冪等）
        assert_eq!(migrate_console_modes_in(base), 0);
    }

    /// migration は legacy **file 名**（sanitize 済み）から project/lane を復元するため、
    /// 素の project 名に `.` や `/` を含む lane でも同じ registry file に着地する
    /// （`registry_file_in` が同じ sanitize を通す = 冪等だから成立する）。
    /// ここがズレると「移行したのに反映されない」= 無音の Act 巻き戻りになる。
    #[test]
    fn migration_lands_on_the_same_file_as_the_unsanitized_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let dir = base.join("console_modes");
        std::fs::create_dir_all(&dir).unwrap();
        // 実 project 名は "creo.memories" — legacy file 名は sanitize 済みの "creo-memories__root"
        std::fs::write(dir.join("creo-memories__root"), "chat").unwrap();

        assert_eq!(migrate_console_modes_in(base), 1);

        // 素の project 名で引いても Chat が読める（= 同じ file に着地した）
        assert_eq!(root_act_in(base, "creo.memories", "root"), SessionAct::Chat);
    }
}
