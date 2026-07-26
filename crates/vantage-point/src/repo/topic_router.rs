//! TopicRouter: Hub → Topic 振り分けルーター
//!
//! Hub からの RepoMessage を Topic パスに変換し、
//! パターンマッチする subscriber に配信する。
//! Retained 対象（state/command）のメッセージは最新値を保持し、
//! 新規 subscribe 時に初期配信する。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{RwLock, mpsc};

use crate::protocol::RepoMessage;
use crate::repo::retained::RetainedStore;
use crate::repo::topic::{TopicPath, TopicPattern};

/// demand-driven production (S2 / doc 27 §4.1 Cap2) の hook。
///
/// ある concrete topic に subscriber が現れた/消えた瞬間 (0↔1 遷移) に `cb` が呼ばれ、
/// producer を lazy に start/stop させる。 これにより 「購読者が居る間だけ pump を回す」
/// = 無駄 stream を消す production が可能になる (terminal pump の本命用途)。
struct DemandHook {
    /// 監視対象パターン (例: `repo/terminal/data/+/out`)。
    /// subscriber の **concrete** な topic がこれにマッチしたとき demand 計上する。
    pattern: TopicPattern,
    /// `(topic, active)` で呼ばれる。 active=true: 0→1 (start)、 false: 1→0 (stop)。
    /// sync で呼ばれるので cb 側は重い処理を `tokio::spawn` に逃がすこと
    /// (subscribe/unsubscribe の hot path を塞がない)。
    cb: DemandCallback,
}

type DemandCallback = Arc<dyn Fn(String, bool) + Send + Sync>;

/// Topic ベースのメッセージルーター
pub struct TopicRouter {
    /// Retained メッセージストア（state/command の最新値を保持）
    retained: Arc<RwLock<RetainedStore>>,
    /// アクティブな subscriber 一覧
    subscribers: Arc<RwLock<Vec<TopicSubscription>>>,
    /// subscriber ID の採番カウンター
    next_id: AtomicU64,
    /// demand hook 一覧 (S2)。 `register_demand` で登録。 std Mutex (await を跨がない)。
    demands: Mutex<Vec<DemandHook>>,
    /// topic ごとの現在 subscriber 数（hook の有無に依らず常時計上、 0 で entry 除去）。
    /// 0↔1 遷移で hook.cb を呼ぶ判定 + `demand_active` の level 読み（doc 53 R2）に使う。
    demand_counts: Mutex<HashMap<String, usize>>,
}

/// 個別の subscriber エントリ
struct TopicSubscription {
    /// 一意な識別子
    id: u64,
    /// マッチング対象のパターン
    pattern: TopicPattern,
    /// subscribe 時の生パターン文字列 (demand 計上のキー。 unsubscribe 時に逆引きする)。
    pattern_str: String,
    /// メッセージ配信チャネル
    tx: mpsc::Sender<(String, RepoMessage)>,
}

/// repo path_key → canvas TopicRouter の共有 map（daemon の canvas 集約 + repo 起動の両方が触る）。
///
/// get-or-create の両側性: subscribe が先なら placeholder を作り、repo 起動が
/// **後から同じ entry を養子縁組する**（`RepoRuntimes::start` → `start_repo`）。
/// これが無いと boot 窓（daemon 再起動直後、repo spawn 完了前の subscribe）で
/// placeholder に固定された購読者へ実 router の broadcast が永遠に届かない。
pub(crate) type CanvasRouters =
    std::sync::Arc<RwLock<std::collections::HashMap<String, Arc<TopicRouter>>>>;

impl TopicRouter {
    /// 新しいルーターを作成
    pub fn new() -> Self {
        Self {
            retained: Arc::new(RwLock::new(RetainedStore::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            next_id: AtomicU64::new(0),
            demands: Mutex::new(Vec::new()),
            demand_counts: Mutex::new(HashMap::new()),
        }
    }

    /// Hub からメッセージを受け取り、topic に振り分ける
    ///
    /// 1. RepoMessage → topic 文字列に変換
    /// 2. Retained 対象なら最新値を保存
    /// 3. パターンマッチする全 subscriber に配信
    pub async fn route(&self, msg: RepoMessage) {
        let topic = Self::message_to_topic(&msg);

        // Retained 対象（state/command カテゴリ）なら保存
        if TopicPath::parse(&topic).is_retained() {
            self.retained.write().await.set(&topic, msg.clone());
        }

        // マッチする subscriber に配信（送信失敗は無視）
        let subs = self.subscribers.read().await;
        for sub in subs.iter() {
            if TopicPath::parse(&topic).matches(&sub.pattern) {
                let _ = sub.tx.try_send((topic.clone(), msg.clone()));
            }
        }
    }

    /// lane segment の正規化: `None` = conductor（lead）。
    /// per-lane board topic の lane 部に使う（root/performer 語彙）。
    fn lane_seg(lane: &Option<String>) -> &str {
        lane.as_deref()
            .unwrap_or(crate::repo::lanes_state::ROOT_LANE_NAME)
    }

    /// lane address（`vp/performer/foo` 等、 `/` を含む）を topic segment 安全な 1 token に
    /// 変換する（`/` → `~`）。 doc 27 §4.1 の per-lane terminal topic 用。 message には full
    /// address を載せ、 topic key だけ encode する（subscriber も同じ変換で subscribe する）。
    fn terminal_lane_key(lane: &str) -> String {
        lane.replace('/', "~")
    }

    /// RepoMessage → Topic 文字列のマッピング
    ///
    /// 命名規則: `{scope}/{capability}/{category}/{detail}`
    /// - scope: "process"
    /// - capability: board, heavens-door, terminal, debug, star-platinum
    /// - category: command, event, state, data, log, trace
    fn message_to_topic(msg: &RepoMessage) -> String {
        match msg {
            // === Board（Canvas 表示能力）===
            // lane segment を verb の後に挿入: `.../command/{verb}/{lane}/{pane_id}`。
            // category(seg2)=command は不変なので is_retained は維持され、retained store は
            // lane 別に分離される（root/main と performer-foo/main が別 topic）。
            // lane=None は conductor（lead）に正規化。
            RepoMessage::Show { pane_id, lane, .. } => {
                format!(
                    "repo/board/command/show/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            RepoMessage::Clear { pane_id, lane, .. } => {
                format!(
                    "repo/board/command/clear/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            RepoMessage::Split { pane_id, lane, .. } => {
                format!(
                    "repo/board/command/split/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            RepoMessage::Close { pane_id, lane, .. } => {
                format!(
                    "repo/board/command/close/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            RepoMessage::TogglePane { pane_id, lane, .. } => {
                format!(
                    "repo/board/command/toggle/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            // board モデル (2026-07-15): scope 別 board の snapshot。 category=state(seg2) で
            // retained され、 再接続/board 切替時の初期配信を兼ねる。 topic は
            // `.../state/board/{scope}/{lane}` で scope×lane ごとに retained を分離する
            // (lane board は lane で分離、 proj board は lane=conductor に正規化)。
            RepoMessage::BoardUpdated { scope, lane, .. } => {
                format!("repo/board/state/board/{}/{}", scope, Self::lane_seg(lane))
            }
            // doc 48 Phase 2: editor bridge command。canvas channel (`board/#`) に乗せて
            // vp-app へ届ける。category=event = 非 retained (stale command の再購読 replay を
            // 構造的に防ぐ — retained にすると再接続のたびに古い editor_set が再実行される)。
            RepoMessage::EditorCommand { request_id, .. } => {
                format!("repo/board/event/editor/{}", request_id)
            }
            // === Heaven's Door（AI Agent 能力）===
            RepoMessage::ChatChunk { .. } => "repo/heavens-door/event/text-chunk".to_string(),
            RepoMessage::ChatMessage { .. } => "repo/heavens-door/event/chat-message".to_string(),
            RepoMessage::ChatComponent { .. } => "repo/heavens-door/event/component".to_string(),
            RepoMessage::ComponentDismissed { .. } => {
                "repo/heavens-door/event/component-dismissed".to_string()
            }
            RepoMessage::AgUi { .. } => "repo/heavens-door/event/ag-ui".to_string(),
            RepoMessage::SessionList { .. } => "repo/heavens-door/state/session-list".to_string(),
            RepoMessage::SessionSwitched { .. } => "repo/heavens-door/state/session".to_string(),
            RepoMessage::SessionCreated { .. } => {
                "repo/heavens-door/event/session-created".to_string()
            }
            RepoMessage::SessionClosed { .. } => {
                "repo/heavens-door/event/session-closed".to_string()
            }
            RepoMessage::SessionHistory { .. } => {
                "repo/heavens-door/event/session-history".to_string()
            }

            // === Terminal（PTY 出力）===
            RepoMessage::TerminalOutput { .. } => "repo/terminal/data/output".to_string(),
            RepoMessage::TerminalReady => "repo/terminal/state/ready".to_string(),
            RepoMessage::TerminalExited => "repo/terminal/state/exited".to_string(),
            // doc 27 §4.1: Lane PTY 出力は per-lane topic。 category(seg2)=data → 非 retained
            // (ephemeral stream)。 lane address ('/' 含む) は seg3 で 1 token 化する。
            RepoMessage::LaneTerminalOutput { lane, .. } => {
                format!("repo/terminal/data/{}/out", Self::terminal_lane_key(lane))
            }
            // === Echoes gui（構造化会話 GUI）===
            // doc 32: per-lane の構造化イベント stream。category(seg2)=data → 非 retained。
            // lane address ('/' 含む) は seg3 で 1 token 化（terminal と同じ規則）。
            RepoMessage::EchoesEvent { lane, .. } => {
                format!("repo/echoes/data/{}/event", Self::terminal_lane_key(lane))
            }

            // === repo（Process 管理）===
            RepoMessage::Ping => "repo/star-platinum/event/ping".to_string(),
            // switch_lane は一時コマンド（active Lane 切替）であり state ではない。
            // category=event にして **非 retained** にする（command にすると retained store に
            // 残り、canvas channel 再接続のたび「最後の switch」が replay され、ユーザーが別 lane
            // を選んでいても強制ジャンプする副作用が出る）。canvas channel は board/# を
            // 購読するので event でも live 配信は届く。
            RepoMessage::SwitchLane { .. } => "repo/board/event/switch-lane".to_string(),
            // wiremsg: Lane 一覧 snapshot。category=state → retained。
            RepoMessage::LanesSnapshot { .. } => "repo/star-platinum/state/lanes".to_string(),
        }
    }

    /// パターンに一致するメッセージを受信する subscriber を登録
    ///
    /// 登録時に retained ストアから初期値を配信する。
    /// 返り値の Receiver でメッセージを受信し、u64 で unsubscribe に使う。
    pub async fn subscribe(&self, pattern: &str) -> (u64, mpsc::Receiver<(String, RepoMessage)>) {
        let pattern_str = pattern.to_string();
        let pattern = TopicPattern::parse(pattern);
        let (tx, rx) = mpsc::channel(256);

        // Retained メッセージの初期配信
        {
            let retained = self.retained.read().await;
            for (topic, msg) in retained.get_matching(&pattern) {
                let _ = tx.try_send((topic.to_string(), msg.clone()));
            }
        }

        // subscriber 登録（アトミックに ID を採番）
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut subs = self.subscribers.write().await;
            subs.push(TopicSubscription {
                id,
                pattern,
                pattern_str: pattern_str.clone(),
                tx,
            });
        }

        // demand 計上: subscriber lock を解放してから評価する (cb は spawn して即返る)。
        self.fire_demand(&pattern_str, true);

        (id, rx)
    }

    /// subscriber を削除
    pub async fn unsubscribe(&self, id: u64) {
        // 削除した subscription の pattern_str を取り出し、 demand を巻き戻す。
        let removed = {
            let mut subs = self.subscribers.write().await;
            subs.iter()
                .position(|s| s.id == id)
                .map(|pos| subs.remove(pos).pattern_str)
        };
        if let Some(pattern_str) = removed {
            self.fire_demand(&pattern_str, false);
        }
    }

    /// demand hook を登録する (S2 / doc 27 §4.1 Cap2)。
    ///
    /// `pattern` にマッチする **concrete** topic に subscriber が現れた瞬間 (0→1) に
    /// `cb(topic, true)`、 最後の subscriber が消えた瞬間 (1→0) に `cb(topic, false)` が呼ばれる。
    /// producer (例: terminal pump) を lazy に start/stop させる用途。
    /// `cb` は sync で呼ばれるため、 重い処理 (reverse-route 等) は内部で `tokio::spawn` すること。
    pub fn register_demand<F>(&self, pattern: &str, cb: F)
    where
        F: Fn(String, bool) + Send + Sync + 'static,
    {
        self.demands.lock().unwrap().push(DemandHook {
            pattern: TopicPattern::parse(pattern),
            cb: Arc::new(cb),
        });
    }

    /// 指定 concrete topic に現在 subscriber が居るか（demand の level 読み、doc 53 R2）。
    ///
    /// hook の発火が「edge（0↔1 の瞬間）」を伝えるのに対し、これは「今どうか」を答える。
    /// pump reconcile の intent 側入力（`reconcile_lane_pumps`）が読む。key は購読時の
    /// concrete topic 文字列（lane key は `~` encode 済の形、`fire_demand` の計上と同一）。
    pub fn demand_active(&self, topic: &str) -> bool {
        self.demand_counts
            .lock()
            .unwrap()
            .get(topic)
            .is_some_and(|c| *c > 0)
    }

    /// 指定 concrete topic を購読している **subscriber id の最大値**（購読の世代）。
    ///
    /// `demand_active` が「今 誰か居るか」を答えるのに対し、これは「**誰が居るか変わったか**」を
    /// 比較できる値を返す。id は単調増加なので、購読者が入れ替われば必ず増える。
    /// 購読ゼロなら `None`。
    ///
    /// 用途: pump の replay 判断（doc 53 §6.5.0）。pump を張り直すべきか（= slot pid が
    /// 差し替わったか）と、replay を流すべきか（= **client が画面を持っているか**）は
    /// 別の問いで、後者は購読者の世代でしか答えられない。GUI が再起動すると slot は同じまま
    /// 購読者だけが入れ替わるため、pid 照合では「変化なし」と誤答する
    /// （[[one-predicate-three-properties]] の同型 — 答えが一致する間は静的に判別できない）。
    pub async fn subscriber_epoch(&self, topic: &str) -> Option<u64> {
        let path = TopicPath::parse(topic);
        self.subscribers
            .read()
            .await
            .iter()
            .filter(|s| path.matches(&s.pattern))
            .map(|s| s.id)
            .max()
    }

    /// subscriber の増減を demand hook に反映する。
    ///
    /// 計上（demand_counts）は **hook の有無に依らず常に**行う — `demand_active` の level 読みが
    /// hook の登録順序（router 養子縁組の前に subscribe が立つ boot 窓）に依存しないため
    /// （doc 53 R2。旧実装は hook 未登録なら計上ごと skip していた）。0 で entry を除去するので
    /// map は「現在購読が生きている topic」に有界。subscribe/unsubscribe は接続単位の頻度なので
    /// 常時計上のコストは無視できる（hot path の route() は counts に触らない）。
    ///
    /// **hook は購読の増減があれば毎回呼ぶ**（doc 53 §2.3 — edge → level の hook 側）。
    ///
    /// ⚠️ 旧実装は 0→1 / 1→0 の遷移時だけ呼んでいた。これは **寿命の違う 2 者の間で変化を
    /// signal にする**形で、次の順序逆転で edge が落ちる（2026-07-26 実測）:
    ///
    /// ```text
    /// 01:35:22  added=true   count=1  ← GUI が購読、pump 起動
    /// 01:36:03  added=true   count=2  ← GUI 再起動の新購読（旧がまだ居る = edge 立たず）
    /// 01:37:00  added=false  count=1  ← 旧購読の掃除が **57 秒遅れて** 到着
    /// ```
    ///
    /// GUI プロセスが死んでも daemon は QUIC の idle timeout（~60s）まで気づかないため、
    /// **新購読が先・旧購読の掃除が後**になり count が一度も 0 を通らない。結果 hook が
    /// 飛ばず、pump が張られず、**console が永久に沈黙**する（doc 53 §6.5.0 の追跡で判明）。
    /// timeout を縮めても遅延が小さくなるだけで、crash では bye も送れない —
    /// **順序に依存する限り原理的に塞げない**。
    ///
    /// 呼び手（`register_demand` の cb）は reconcile で**冪等**なので、何度呼ばれても
    /// 「今の level（`demand_active`）を読んであるべき姿に合わせる」に収束する。R2 が pump 側で
    /// 採った形を hook 側にも適用したもの（**契機は判断を持たない**）。
    /// 頻度は購読の増減 = 人間の操作（GUI 起動 / lane 切替）なので実質無視できる。
    ///
    /// lock は cb 呼び出し前に手放す (cb が router を再帰 lock しても deadlock しない)。
    fn fire_demand(&self, topic: &str, added: bool) {
        {
            let mut counts = self.demand_counts.lock().unwrap();
            let entry = counts.entry(topic.to_string()).or_insert(0);
            if added {
                *entry += 1;
            } else if *entry > 0 {
                *entry -= 1;
            }
            if *entry == 0 {
                counts.remove(topic);
            }
        }
        let to_call: Vec<DemandCallback> = {
            let demands = self.demands.lock().unwrap();
            let path = TopicPath::parse(topic);
            demands
                .iter()
                .filter(|h| path.matches(&h.pattern))
                .map(|h| h.cb.clone())
                .collect()
        };
        for cb in to_call {
            cb(topic.to_string(), added);
        }
    }

    /// 現在 active (subscriber count > 0) な全 demand topic に対し、 demand hook を
    /// `active=true` で **再発火** する (count は変えない)。
    ///
    /// 用途 (S2 polish): repo control channel が daemon に (再)接続した瞬間の catch-up。
    /// surface が先に subscribe して demand_start を撃った時点で repo control channel が
    /// 不在だと reverse-route が捨てられる (start が repo に届かない)。 repo 接続後に本 method を
    /// 呼ぶと、 既に立っている demand を撃ち直して pump を起こせる。 cb (terminal_demand_start)
    /// は repo 側で idempotent (既存 pump を差し替え) なので二重呼びでも 1 本に収束する。
    pub fn refire_active_demands(&self) {
        let active: Vec<String> = {
            let counts = self.demand_counts.lock().unwrap();
            counts
                .iter()
                .filter(|(_, c)| **c > 0)
                .map(|(t, _)| t.clone())
                .collect()
        };
        for topic in active {
            let to_call: Vec<DemandCallback> = {
                let demands = self.demands.lock().unwrap();
                let path = TopicPath::parse(&topic);
                demands
                    .iter()
                    .filter(|h| path.matches(&h.pattern))
                    .map(|h| h.cb.clone())
                    .collect()
            };
            for cb in to_call {
                cb(topic.clone(), true);
            }
        }
    }

    /// Retained store への直接アクセス
    pub fn retained(&self) -> Arc<RwLock<RetainedStore>> {
        self.retained.clone()
    }
}

impl Default for TopicRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Content, RepoMessage};

    /// doc 48 Phase 2: EditorCommand は canvas channel (`board/#`) 配下かつ
    /// **非 retained** (category=event)。retained にすると再購読のたびに stale な
    /// editor_set が replay される — その回帰を固定するガード。
    #[test]
    fn editor_command_topic_is_under_board_and_not_retained() {
        let topic = TopicRouter::message_to_topic(&RepoMessage::EditorCommand {
            request_id: "r1".to_string(),
            op: "values".to_string(),
            field_id: None,
            value: None,
        });
        assert!(
            topic.starts_with("repo/board/event/editor/"),
            "topic={topic}"
        );
        assert!(
            !crate::repo::topic::TopicPath::parse(&topic).is_retained(),
            "EditorCommand が retained になっている: {topic}"
        );
    }

    /// テスト用の Show メッセージを生成
    fn make_show(pane_id: &str, text: &str) -> RepoMessage {
        RepoMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
            lane: None,
            scope: None,
        }
    }

    /// lane を指定した Show（per-lane topic 分離テスト用）
    fn make_show_lane(pane_id: &str, text: &str, lane: &str) -> RepoMessage {
        RepoMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
            lane: Some(lane.to_string()),
            scope: None,
        }
    }

    // =========================================================================
    // message_to_topic マッピング
    // =========================================================================

    #[test]
    fn test_message_to_topic_show() {
        // lane=None は conductor に正規化され lane segment に入る
        let msg = make_show("main", "# Hello");
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/board/command/show/root/main");
    }

    #[test]
    fn test_message_to_topic_show_performer_lane() {
        // performer lane は lane segment にその名が入り、conductor と別 topic になる
        let msg = make_show_lane("main", "# Hi", "feat-api");
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/board/command/show/feat-api/main");
    }

    #[test]
    fn test_per_lane_topic_separation() {
        // 同 pane_id でも lane が違えば別 topic（retained 後勝ち上書きが起きない）
        let conductor = TopicRouter::message_to_topic(&make_show("main", "a"));
        let performer = TopicRouter::message_to_topic(&make_show_lane("main", "b", "feat-api"));
        assert_ne!(conductor, performer);
        // category(seg2)=command は不変 → 両方 retained 対象
        assert!(TopicPath::parse(&conductor).is_retained());
        assert!(TopicPath::parse(&performer).is_retained());
    }

    #[test]
    fn test_message_to_topic_clear() {
        let msg = RepoMessage::Clear {
            pane_id: "side".to_string(),
            lane: None,
            scope: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/board/command/clear/root/side");
    }

    #[test]
    fn test_message_to_topic_chat_chunk() {
        let msg = RepoMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/heavens-door/event/text-chunk");
    }

    #[test]
    fn test_message_to_topic_session_list() {
        let msg = RepoMessage::SessionList {
            sessions: vec![],
            active_id: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/heavens-door/state/session-list");
    }

    #[test]
    fn test_message_to_topic_terminal_ready() {
        let topic = TopicRouter::message_to_topic(&RepoMessage::TerminalReady);
        assert_eq!(topic, "repo/terminal/state/ready");
    }

    #[test]
    fn test_message_to_topic_lane_terminal_output() {
        // doc 27 §4.1: per-lane terminal 出力。 lane address の '/' は seg3 で '~' に encode、
        // category(seg2)=data なので 非 retained（ephemeral stream）。
        let msg = RepoMessage::LaneTerminalOutput {
            lane: "vp/performer/foo".to_string(),
            session: 1,
            data: "aGVsbG8=".to_string(),
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/terminal/data/vp~performer~foo/out");
        assert!(!TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_lane_terminal_topics_are_per_lane() {
        // 別 lane は別 topic（subscriber 数 = lane 別 demand の前提、 S2 で効く）。
        let a = TopicRouter::message_to_topic(&RepoMessage::LaneTerminalOutput {
            lane: "vp/root".to_string(),
            session: 1,
            data: String::new(),
        });
        let b = TopicRouter::message_to_topic(&RepoMessage::LaneTerminalOutput {
            lane: "vp/performer/foo".to_string(),
            session: 1,
            data: String::new(),
        });
        assert_ne!(a, b);
    }

    #[test]
    fn test_lane_terminal_topic_ignores_session() {
        // doc 50 §4.6 A6（Design B / doc 38 落とし穴①）: session は topic key に埋めず message
        // field で運ぶ。同 lane の別 session は **同一 topic** に流れ、 World A の xterm が
        // session で振り分ける（demand は lane 単位のまま = register_lane_demand 不変の前提）。
        let s1 = TopicRouter::message_to_topic(&RepoMessage::LaneTerminalOutput {
            lane: "vp/root".to_string(),
            session: 1,
            data: String::new(),
        });
        let s2 = TopicRouter::message_to_topic(&RepoMessage::LaneTerminalOutput {
            lane: "vp/root".to_string(),
            session: 7,
            data: String::new(),
        });
        assert_eq!(s1, s2, "session は topic を分けない（落とし穴①）");
    }

    #[test]
    fn test_message_to_topic_echoes_event() {
        // doc 32: Echoes gui の per-lane 構造化イベント。lane の '/' は '~' に encode、
        // category(seg2)=data なので 非 retained（ephemeral stream、terminal と同じ規則）。
        let msg = RepoMessage::EchoesEvent {
            lane: "vp/performer/foo".to_string(),
            session: 2,
            event: crate::echoes::EchoesEvent::MessageChunk {
                text: "hi".to_string(),
            },
        };
        let topic = TopicRouter::message_to_topic(&msg);
        // doc 38 落とし穴①: session は topic key に混入しない（per-lane topic のまま）。
        assert_eq!(topic, "repo/echoes/data/vp~performer~foo/event");
        assert!(!TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_message_to_topic_ping() {
        let topic = TopicRouter::message_to_topic(&RepoMessage::Ping);
        assert_eq!(topic, "repo/star-platinum/event/ping");
    }

    #[test]
    fn test_switch_lane_is_event_not_retained() {
        // switch_lane は一時コマンド → event category → 非 retained（再接続 replay されない）
        let msg = RepoMessage::SwitchLane {
            lane: "feat-api".to_string(),
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/board/event/switch-lane");
        assert!(!TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_message_to_topic_lanes_snapshot() {
        // wiremsg: Lane snapshot は state カテゴリ → retained 対象。
        let msg = RepoMessage::LanesSnapshot {
            lanes: vec![],
            origin: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "repo/star-platinum/state/lanes");
        assert!(TopicPath::parse(&topic).is_retained());
    }

    // =========================================================================
    // route → retained に保存
    // =========================================================================

    #[tokio::test]
    async fn test_route_stores_retained_for_state() {
        let router = TopicRouter::new();

        // state カテゴリは retained
        router.route(RepoMessage::TerminalReady).await;

        let retained = router.retained.read().await;
        let msg = retained.get("repo/terminal/state/ready");
        assert!(msg.is_some());
        assert!(matches!(msg.unwrap(), RepoMessage::TerminalReady));
    }

    #[tokio::test]
    async fn test_route_stores_retained_for_command() {
        let router = TopicRouter::new();

        // command カテゴリも retained
        let show = make_show("main", "# Hello");
        router.route(show).await;

        let retained = router.retained.read().await;
        let msg = retained.get("repo/board/command/show/root/main");
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_route_does_not_store_event() {
        let router = TopicRouter::new();

        // event カテゴリは retained 対象外
        let msg = RepoMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        let retained = router.retained.read().await;
        assert!(retained.is_empty());
    }

    // =========================================================================
    // subscribe → retained の初期配信
    // =========================================================================

    #[tokio::test]
    async fn test_subscribe_receives_retained_initial() {
        let router = TopicRouter::new();

        // 先に retained に保存
        router.route(RepoMessage::TerminalReady).await;
        router.route(make_show("main", "# Test")).await;

        // state を subscribe → retained から初期配信される
        let (_id, mut rx) = router.subscribe("repo/terminal/state/#").await;

        let (topic, msg) = rx.try_recv().expect("初期配信があるはず");
        assert_eq!(topic, "repo/terminal/state/ready");
        assert!(matches!(msg, RepoMessage::TerminalReady));

        // command は別 topic なので配信されない
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // subscribe → route で新規メッセージ受信
    // =========================================================================

    #[tokio::test]
    async fn test_subscribe_receives_new_messages() {
        let router = TopicRouter::new();

        // 先に subscribe
        let (_id, mut rx) = router.subscribe("repo/heavens-door/event/#").await;

        // route でメッセージ配信
        let msg = RepoMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        let (topic, received) = rx.try_recv().expect("メッセージを受信できるはず");
        assert_eq!(topic, "repo/heavens-door/event/text-chunk");
        assert!(matches!(received, RepoMessage::ChatChunk { .. }));
    }

    #[tokio::test]
    async fn test_subscribe_does_not_receive_unmatched() {
        let router = TopicRouter::new();

        // terminal だけ subscribe
        let (_id, mut rx) = router.subscribe("repo/terminal/#").await;

        // 別 capability のメッセージを route
        let msg = RepoMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        // 受信しないはず
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // ワイルドカード subscribe
    // =========================================================================

    #[tokio::test]
    async fn test_wildcard_subscribe_all() {
        let router = TopicRouter::new();

        // 全メッセージを subscribe
        let (_id, mut rx) = router.subscribe("#").await;

        router.route(RepoMessage::Ping).await;
        router.route(RepoMessage::TerminalReady).await;

        let (topic1, _) = rx.try_recv().expect("Ping を受信");
        assert_eq!(topic1, "repo/star-platinum/event/ping");

        let (topic2, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(topic2, "repo/terminal/state/ready");
    }

    #[tokio::test]
    async fn test_single_wildcard_subscribe() {
        let router = TopicRouter::new();

        // 全 capability の state を subscribe
        let (_id, mut rx) = router.subscribe("repo/+/state/#").await;

        router.route(RepoMessage::TerminalReady).await;
        router
            .route(RepoMessage::SessionList {
                sessions: vec![],
                active_id: None,
            })
            .await;
        // event はマッチしない
        router
            .route(RepoMessage::ChatChunk {
                content: "x".to_string(),
                done: true,
            })
            .await;

        let (t1, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(t1, "repo/terminal/state/ready");

        let (t2, _) = rx.try_recv().expect("SessionList を受信");
        assert_eq!(t2, "repo/heavens-door/state/session-list");

        // 3つ目はないはず
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // unsubscribe
    // =========================================================================

    #[tokio::test]
    async fn test_unsubscribe_stops_delivery() {
        let router = TopicRouter::new();

        let (id, mut rx) = router.subscribe("repo/terminal/#").await;

        // unsubscribe 前は受信できる
        router.route(RepoMessage::TerminalReady).await;
        assert!(rx.try_recv().is_ok());

        // unsubscribe
        router.unsubscribe(id).await;

        // unsubscribe 後は配信されない
        router.route(RepoMessage::TerminalExited).await;
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // 複数 subscriber
    // =========================================================================

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let router = TopicRouter::new();

        let (_id1, mut rx1) = router.subscribe("repo/terminal/#").await;
        let (_id2, mut rx2) = router.subscribe("repo/+/state/#").await;

        // TerminalReady は両方にマッチ
        router.route(RepoMessage::TerminalReady).await;

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    // =========================================================================
    // demand-driven production (S2 / doc 27 §4.1 Cap2)
    // =========================================================================

    /// **消費者が到達する結論は level で決まる**（doc 53 §2.3 — edge → level）。
    ///
    /// 旧テストは「hook の**呼ばれた回数**」（start 1 回 / stop 1 回）を固定していた。それは
    /// 「hook は 0↔1 の遷移でだけ飛ぶ」という edge 仕様の写しで、**寿命の違う 2 者の間で
    /// 順序が逆転すると落ちる**形だった（実測: GUI 再起動で `1 → 2 → 1` となり count が
    /// 0 を通らず、hook が飛ばず console が永久沈黙）。
    ///
    /// 今は購読の増減のたびに hook が飛び、**消費者が `demand_active` を読んで決める**
    /// （`register_lane_demand` の実装と同じ形をここで再現する）。固定すべき性質は
    /// 「何回呼ばれたか」ではなく「**最後にどちらへ収束したか**」。
    #[tokio::test]
    async fn test_demand_consumer_converges_on_current_level() {
        use std::sync::Mutex as StdMutex;
        let router = Arc::new(TopicRouter::new());
        // 消費者が到達した結論（true = start すべき / false = stop すべき）。
        let verdict: Arc<StdMutex<Vec<bool>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            let v = verdict.clone();
            let weak = Arc::downgrade(&router);
            router.register_demand("repo/terminal/data/+/out", move |topic, _added| {
                let Some(r) = weak.upgrade() else { return };
                v.lock().unwrap().push(r.demand_active(&topic));
            });
        }
        let last = || *verdict.lock().unwrap().last().expect("hook が呼ばれる");

        // 同一 lane topic に 2 subscriber。増えるたびに hook は飛ぶが、結論は常に「start」。
        let (id1, _rx1) = router.subscribe("repo/terminal/data/vp~root/out").await;
        assert!(last(), "購読者が居る = start");
        let (id2, _rx2) = router.subscribe("repo/terminal/data/vp~root/out").await;
        assert!(last(), "2 本目でも結論は start（回数ではなく level）");

        // 1 つ抜けてもまだ残る = start のまま。
        router.unsubscribe(id1).await;
        assert!(last(), "残 1 なので stop に倒れない");

        // 最後の 1 つが抜けて初めて stop。
        router.unsubscribe(id2).await;
        assert!(!last(), "誰も居なくなったら stop");
    }

    /// **順序が逆転しても正しく収束する**（本修正の受け入れ条件）。
    ///
    /// GUI が死んでも daemon は QUIC idle timeout（~60s）まで気づかないため、実機では
    /// **新購読が先・旧購読の掃除が後**という順序になる（2026-07-26 実測）。旧 edge 仕様では
    /// count が `1 → 2 → 1` と動いて 0 を通らず、hook が 1 度も飛ばずに pump が沈黙した。
    ///
    /// 壊し方: `fire_demand` に `if !transition { return; }` を戻すと、この test は
    /// 「hook が呼ばれる」の時点で落ちる。
    #[tokio::test]
    async fn test_demand_survives_stale_subscriber_outliving_reconnect() {
        use std::sync::Mutex as StdMutex;
        let router = Arc::new(TopicRouter::new());
        let verdict: Arc<StdMutex<Vec<bool>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            let v = verdict.clone();
            let weak = Arc::downgrade(&router);
            router.register_demand("repo/terminal/data/+/out", move |topic, _added| {
                let Some(r) = weak.upgrade() else { return };
                v.lock().unwrap().push(r.demand_active(&topic));
            });
        }
        let topic = "repo/terminal/data/vp~root/out";

        // ① 旧 GUI が購読（count 0→1）。ここは旧実装でも hook が飛ぶ。
        let (stale, _rx_stale) = router.subscribe(topic).await;
        let after_first = verdict.lock().unwrap().len();

        // ② 旧 GUI が死ぬ。だが daemon は QUIC idle timeout まで気づかない
        //    （stale は router に残ったまま = ここでは unsubscribe しない）。
        // ③ 新 GUI が購読 → count 1→2 で **0 を通らない**。
        let (_fresh, _rx_fresh) = router.subscribe(topic).await;

        // **ここが本命**: 新しい購読者に pump を張り直す契機は、この hook しかない。
        // 旧 edge 実装は「遷移していない」として飛ばさず、console が永久に沈黙した。
        let (calls, verdict_now) = {
            let v = verdict.lock().unwrap();
            (v.len(), *v.last().expect("hook が呼ばれる"))
        };
        assert!(
            calls > after_first,
            "stale が残ったままの再購読でも hook が飛ぶ（旧 edge 実装はここで飛ばなかった）"
        );
        assert!(
            verdict_now,
            "結論は『購読者が居る』= start（level 直読なので count=2 でも正しい）"
        );

        // ④ 遅れて旧購読の掃除が届く（count 2→1）。まだ新 GUI が見ているので stop に倒れない。
        let before_cleanup = verdict.lock().unwrap().len();
        router.unsubscribe(stale).await;
        let (calls, verdict_now) = {
            let v = verdict.lock().unwrap();
            (v.len(), *v.last().expect("hook が呼ばれる"))
        };
        assert!(calls > before_cleanup, "掃除でも hook は飛ぶ");
        assert!(
            verdict_now,
            "生きた購読が残っていれば stop に倒れない（方向ではなく level で決める）"
        );
    }

    #[tokio::test]
    async fn test_demand_per_lane_independent() {
        use std::sync::Mutex as StdMutex;
        let router = TopicRouter::new();
        let started: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            let log = started.clone();
            router.register_demand("repo/terminal/data/+/out", move |topic, active| {
                if active {
                    log.lock().unwrap().push(topic);
                }
            });
        }

        let (_a, _ra) = router.subscribe("repo/terminal/data/vp~root/out").await;
        let (_b, _rb) = router
            .subscribe("repo/terminal/data/vp~performer~foo/out")
            .await;

        let log = started.lock().unwrap();
        assert_eq!(log.len(), 2, "lane ごとに独立して start");
        assert!(log.contains(&"repo/terminal/data/vp~root/out".to_string()));
        assert!(log.contains(&"repo/terminal/data/vp~performer~foo/out".to_string()));
    }

    #[tokio::test]
    async fn test_demand_ignores_non_matching_pattern() {
        use std::sync::atomic::AtomicUsize;
        let router = TopicRouter::new();
        let fired = Arc::new(AtomicUsize::new(0));
        {
            let f = fired.clone();
            router.register_demand("repo/terminal/data/+/out", move |_t, _a| {
                f.fetch_add(1, Ordering::Relaxed);
            });
        }
        // board の subscribe は terminal demand を発火しない。
        let (_id, _rx) = router.subscribe("repo/board/#").await;
        assert_eq!(fired.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_subscribe_without_demand_hook_is_noop() {
        // hook 未登録でも subscribe/unsubscribe は安全に通る (cb は誰も呼ばれない)。
        // doc 53 R2 で計上 (demand_counts) は hook 非依存の常時計上になった — unsubscribe で
        // 0 に戻り entry も除去されるので、 map は伸びない。
        let router = TopicRouter::new();
        let (id, _rx) = router.subscribe("repo/board/#").await;
        router.unsubscribe(id).await;
        assert!(!router.demand_active("repo/board/#"));
    }

    /// doc 53 R2: `demand_active` は「今 subscriber が居るか」の level 読み。
    /// hook の登録有無・登録順序に依存しない（router 養子縁組の boot 窓で、hook 登録前に
    /// 立った購読も demand として見える — reconcile の intent 側入力の要件）。
    #[tokio::test]
    async fn test_demand_active_reads_current_subscribers_without_hooks() {
        let router = TopicRouter::new();
        let topic = "repo/terminal/data/vp~root/out";
        assert!(!router.demand_active(topic), "購読前は inactive");

        // hook 未登録のまま subscribe → level は立つ（edge の cb は誰も呼ばれない）。
        let (id1, _rx1) = router.subscribe(topic).await;
        assert!(router.demand_active(topic), "hook 無しでも計上される");

        // 2 本目でも active のまま、1 本抜けても active、全部抜けたら inactive。
        let (id2, _rx2) = router.subscribe(topic).await;
        router.unsubscribe(id1).await;
        assert!(router.demand_active(topic), "残 1 本なら active");
        router.unsubscribe(id2).await;
        assert!(!router.demand_active(topic), "0 本で inactive に戻る");

        // 別 topic の購読はこの topic の demand に影響しない（concrete topic 単位の計上）。
        let (_id3, _rx3) = router.subscribe("repo/terminal/data/other~root/out").await;
        assert!(!router.demand_active(topic));
    }

    #[tokio::test]
    async fn test_refire_active_demands_recalls_active_starts() {
        // S2 polish: repo 再接続時 catch-up。 active (count>0) な demand だけ start を撃ち直す。
        use std::sync::atomic::AtomicUsize;
        let router = TopicRouter::new();
        let starts = Arc::new(AtomicUsize::new(0));
        {
            let s = starts.clone();
            router.register_demand("repo/terminal/data/+/out", move |_t, active| {
                if active {
                    s.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        let (id, _rx) = router.subscribe("repo/terminal/data/vp~root/out").await;
        assert_eq!(starts.load(Ordering::Relaxed), 1, "初回 0→1 start");

        // repo 再接続相当: active な demand を撃ち直す → 再発火 (count は不変)。
        router.refire_active_demands();
        assert_eq!(starts.load(Ordering::Relaxed), 2, "catch-up で再発火");

        // subscriber が居なくなれば active 無し → refire は no-op。
        router.unsubscribe(id).await;
        router.refire_active_demands();
        assert_eq!(starts.load(Ordering::Relaxed), 2, "active 無しなら no-op");
    }

    // =========================================================================
    // Default trait
    // =========================================================================

    #[test]
    fn test_default() {
        let _router = TopicRouter::default();
    }
}
