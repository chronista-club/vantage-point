//! TopicRouter: Hub → Topic 振り分けルーター
//!
//! Hub からの ProcessMessage を Topic パスに変換し、
//! パターンマッチする subscriber に配信する。
//! Retained 対象（state/command）のメッセージは最新値を保持し、
//! 新規 subscribe 時に初期配信する。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{RwLock, mpsc};

use crate::process::retained::RetainedStore;
use crate::process::topic::{TopicPath, TopicPattern};
use crate::protocol::ProcessMessage;

/// demand-driven production (S2 / doc 27 §4.1 Cap2) の hook。
///
/// ある concrete topic に subscriber が現れた/消えた瞬間 (0↔1 遷移) に `cb` が呼ばれ、
/// producer を lazy に start/stop させる。 これにより 「購読者が居る間だけ pump を回す」
/// = 無駄 stream を消す production が可能になる (terminal pump の本命用途)。
struct DemandHook {
    /// 監視対象パターン (例: `process/terminal/data/+/out`)。
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
    /// demand 対象 topic ごとの subscriber 数。 0↔1 遷移で hook.cb を呼ぶ判定に使う。
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
    tx: mpsc::Sender<(String, ProcessMessage)>,
}

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
    /// 1. ProcessMessage → topic 文字列に変換
    /// 2. Retained 対象なら最新値を保存
    /// 3. パターンマッチする全 subscriber に配信
    pub async fn route(&self, msg: ProcessMessage) {
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
    /// per-lane PP topic の lane 部に使う（conductor/performer 語彙）。
    fn lane_seg(lane: &Option<String>) -> &str {
        lane.as_deref().unwrap_or("conductor")
    }

    /// lane address（`vp/performer/foo` 等、 `/` を含む）を topic segment 安全な 1 token に
    /// 変換する（`/` → `~`）。 doc 27 §4.1 の per-lane terminal topic 用。 message には full
    /// address を載せ、 topic key だけ encode する（subscriber も同じ変換で subscribe する）。
    fn terminal_lane_key(lane: &str) -> String {
        lane.replace('/', "~")
    }

    /// ProcessMessage → Topic 文字列のマッピング
    ///
    /// 命名規則: `{scope}/{capability}/{category}/{detail}`
    /// - scope: "process"
    /// - capability: paisley-park, heavens-door, terminal, debug, star-platinum
    /// - category: command, event, state, data, log, trace
    fn message_to_topic(msg: &ProcessMessage) -> String {
        match msg {
            // === Paisley Park（Canvas 表示能力）===
            // lane segment を verb の後に挿入: `.../command/{verb}/{lane}/{pane_id}`。
            // category(seg2)=command は不変なので is_retained は維持され、retained store は
            // lane 別に分離される（conductor/main と performer-foo/main が別 topic）。
            // lane=None は conductor（lead）に正規化。
            ProcessMessage::Show { pane_id, lane, .. } => {
                format!(
                    "process/paisley-park/command/show/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            ProcessMessage::Clear { pane_id, lane, .. } => {
                format!(
                    "process/paisley-park/command/clear/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            ProcessMessage::Split { pane_id, lane, .. } => {
                format!(
                    "process/paisley-park/command/split/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            ProcessMessage::Close { pane_id, lane, .. } => {
                format!(
                    "process/paisley-park/command/close/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            ProcessMessage::TogglePane { pane_id, lane, .. } => {
                format!(
                    "process/paisley-park/command/toggle/{}/{}",
                    Self::lane_seg(lane),
                    pane_id
                )
            }
            ProcessMessage::ScreenshotRequest { .. } => {
                "process/paisley-park/command/screenshot".to_string()
            }

            // === Heaven's Door（AI Agent 能力）===
            ProcessMessage::ChatChunk { .. } => "process/heavens-door/event/text-chunk".to_string(),
            ProcessMessage::ChatMessage { .. } => {
                "process/heavens-door/event/chat-message".to_string()
            }
            ProcessMessage::ChatComponent { .. } => {
                "process/heavens-door/event/component".to_string()
            }
            ProcessMessage::ComponentDismissed { .. } => {
                "process/heavens-door/event/component-dismissed".to_string()
            }
            ProcessMessage::AgUi { .. } => "process/heavens-door/event/ag-ui".to_string(),
            ProcessMessage::SessionList { .. } => {
                "process/heavens-door/state/session-list".to_string()
            }
            ProcessMessage::SessionSwitched { .. } => {
                "process/heavens-door/state/session".to_string()
            }
            ProcessMessage::SessionCreated { .. } => {
                "process/heavens-door/event/session-created".to_string()
            }
            ProcessMessage::SessionClosed { .. } => {
                "process/heavens-door/event/session-closed".to_string()
            }
            ProcessMessage::SessionHistory { .. } => {
                "process/heavens-door/event/session-history".to_string()
            }

            // === Terminal（PTY 出力）===
            ProcessMessage::TerminalOutput { .. } => "process/terminal/data/output".to_string(),
            ProcessMessage::TerminalReady => "process/terminal/state/ready".to_string(),
            ProcessMessage::TerminalExited => "process/terminal/state/exited".to_string(),
            // doc 27 §4.1: Lane PTY 出力は per-lane topic。 category(seg2)=data → 非 retained
            // (ephemeral stream)。 lane address ('/' 含む) は seg3 で 1 token 化する。
            ProcessMessage::LaneTerminalOutput { lane, .. } => {
                format!(
                    "process/terminal/data/{}/out",
                    Self::terminal_lane_key(lane)
                )
            }

            // === Debug（デバッグ情報）===
            ProcessMessage::DebugInfo { .. } => "process/debug/log".to_string(),
            ProcessMessage::DebugModeChanged { .. } => "process/debug/state/mode".to_string(),
            ProcessMessage::TraceLog { .. } => "process/debug/trace".to_string(),

            // === Star Platinum（Process 管理）===
            ProcessMessage::Ping => "process/star-platinum/event/ping".to_string(),
            // switch_lane は一時コマンド（active Lane 切替）であり state ではない。
            // category=event にして **非 retained** にする（command にすると retained store に
            // 残り、canvas channel 再接続のたび「最後の switch」が replay され、ユーザーが別 lane
            // を選んでいても強制ジャンプする副作用が出る）。canvas channel は paisley-park/# を
            // 購読するので event でも live 配信は届く。
            ProcessMessage::SwitchLane { .. } => {
                "process/paisley-park/event/switch-lane".to_string()
            }
            // wiremsg: Lane 一覧 snapshot。category=state → retained。
            ProcessMessage::LanesSnapshot { .. } => "process/star-platinum/state/lanes".to_string(),
        }
    }

    /// パターンに一致するメッセージを受信する subscriber を登録
    ///
    /// 登録時に retained ストアから初期値を配信する。
    /// 返り値の Receiver でメッセージを受信し、u64 で unsubscribe に使う。
    pub async fn subscribe(
        &self,
        pattern: &str,
    ) -> (u64, mpsc::Receiver<(String, ProcessMessage)>) {
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

    /// subscriber の増減を demand hook に反映する。
    ///
    /// 同一 topic の subscriber 数を辿り、 0→1 / 1→0 の遷移時だけ該当 hook の cb を呼ぶ。
    /// hook 未登録 (= 通常の canvas/lanes 経路) なら即 return = ゼロコスト。
    /// lock は cb 呼び出し前に手放す (cb が router を再帰 lock しても deadlock しない)。
    fn fire_demand(&self, topic: &str, added: bool) {
        let to_call: Vec<DemandCallback> = {
            let demands = self.demands.lock().unwrap();
            if demands.is_empty() {
                return;
            }
            let path = TopicPath::parse(topic);
            let matched: Vec<DemandCallback> = demands
                .iter()
                .filter(|h| path.matches(&h.pattern))
                .map(|h| h.cb.clone())
                .collect();
            if matched.is_empty() {
                return;
            }

            let mut counts = self.demand_counts.lock().unwrap();
            let entry = counts.entry(topic.to_string()).or_insert(0);
            let transition = if added {
                *entry += 1;
                *entry == 1 // 0→1
            } else {
                if *entry > 0 {
                    *entry -= 1;
                }
                *entry == 0 // 1→0
            };
            if *entry == 0 {
                counts.remove(topic);
            }
            if transition { matched } else { Vec::new() }
        };

        for cb in to_call {
            cb(topic.to_string(), added);
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
    use crate::protocol::{Content, ProcessMessage};

    /// テスト用の Show メッセージを生成
    fn make_show(pane_id: &str, text: &str) -> ProcessMessage {
        ProcessMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
            lane: None,
        }
    }

    /// lane を指定した Show（per-lane topic 分離テスト用）
    fn make_show_lane(pane_id: &str, text: &str, lane: &str) -> ProcessMessage {
        ProcessMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
            lane: Some(lane.to_string()),
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
        assert_eq!(topic, "process/paisley-park/command/show/conductor/main");
    }

    #[test]
    fn test_message_to_topic_show_performer_lane() {
        // performer lane は lane segment にその名が入り、conductor と別 topic になる
        let msg = make_show_lane("main", "# Hi", "feat-api");
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/paisley-park/command/show/feat-api/main");
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
        let msg = ProcessMessage::Clear {
            pane_id: "side".to_string(),
            lane: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/paisley-park/command/clear/conductor/side");
    }

    #[test]
    fn test_message_to_topic_chat_chunk() {
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/heavens-door/event/text-chunk");
    }

    #[test]
    fn test_message_to_topic_session_list() {
        let msg = ProcessMessage::SessionList {
            sessions: vec![],
            active_id: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/heavens-door/state/session-list");
    }

    #[test]
    fn test_message_to_topic_terminal_ready() {
        let topic = TopicRouter::message_to_topic(&ProcessMessage::TerminalReady);
        assert_eq!(topic, "process/terminal/state/ready");
    }

    #[test]
    fn test_message_to_topic_lane_terminal_output() {
        // doc 27 §4.1: per-lane terminal 出力。 lane address の '/' は seg3 で '~' に encode、
        // category(seg2)=data なので 非 retained（ephemeral stream）。
        let msg = ProcessMessage::LaneTerminalOutput {
            lane: "vp/performer/foo".to_string(),
            data: "aGVsbG8=".to_string(),
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/terminal/data/vp~performer~foo/out");
        assert!(!TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_lane_terminal_topics_are_per_lane() {
        // 別 lane は別 topic（subscriber 数 = lane 別 demand の前提、 S2 で効く）。
        let a = TopicRouter::message_to_topic(&ProcessMessage::LaneTerminalOutput {
            lane: "vp/conductor".to_string(),
            data: String::new(),
        });
        let b = TopicRouter::message_to_topic(&ProcessMessage::LaneTerminalOutput {
            lane: "vp/performer/foo".to_string(),
            data: String::new(),
        });
        assert_ne!(a, b);
    }

    #[test]
    fn test_message_to_topic_ping() {
        let topic = TopicRouter::message_to_topic(&ProcessMessage::Ping);
        assert_eq!(topic, "process/star-platinum/event/ping");
    }

    #[test]
    fn test_switch_lane_is_event_not_retained() {
        // switch_lane は一時コマンド → event category → 非 retained（再接続 replay されない）
        let msg = ProcessMessage::SwitchLane {
            lane: "feat-api".to_string(),
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/paisley-park/event/switch-lane");
        assert!(!TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_message_to_topic_lanes_snapshot() {
        // wiremsg: Lane snapshot は state カテゴリ → retained 対象。
        let msg = ProcessMessage::LanesSnapshot { lanes: vec![] };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/star-platinum/state/lanes");
        assert!(TopicPath::parse(&topic).is_retained());
    }

    #[test]
    fn test_message_to_topic_debug_info() {
        let msg = ProcessMessage::DebugInfo {
            level: crate::protocol::DebugMode::Simple,
            category: "test".to_string(),
            message: "hello".to_string(),
            data: None,
            tags: vec![],
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/debug/log");
    }

    // =========================================================================
    // route → retained に保存
    // =========================================================================

    #[tokio::test]
    async fn test_route_stores_retained_for_state() {
        let router = TopicRouter::new();

        // state カテゴリは retained
        router.route(ProcessMessage::TerminalReady).await;

        let retained = router.retained.read().await;
        let msg = retained.get("process/terminal/state/ready");
        assert!(msg.is_some());
        assert!(matches!(msg.unwrap(), ProcessMessage::TerminalReady));
    }

    #[tokio::test]
    async fn test_route_stores_retained_for_command() {
        let router = TopicRouter::new();

        // command カテゴリも retained
        let show = make_show("main", "# Hello");
        router.route(show).await;

        let retained = router.retained.read().await;
        let msg = retained.get("process/paisley-park/command/show/conductor/main");
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_route_does_not_store_event() {
        let router = TopicRouter::new();

        // event カテゴリは retained 対象外
        let msg = ProcessMessage::ChatChunk {
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
        router.route(ProcessMessage::TerminalReady).await;
        router.route(make_show("main", "# Test")).await;

        // state を subscribe → retained から初期配信される
        let (_id, mut rx) = router.subscribe("process/terminal/state/#").await;

        let (topic, msg) = rx.try_recv().expect("初期配信があるはず");
        assert_eq!(topic, "process/terminal/state/ready");
        assert!(matches!(msg, ProcessMessage::TerminalReady));

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
        let (_id, mut rx) = router.subscribe("process/heavens-door/event/#").await;

        // route でメッセージ配信
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        let (topic, received) = rx.try_recv().expect("メッセージを受信できるはず");
        assert_eq!(topic, "process/heavens-door/event/text-chunk");
        assert!(matches!(received, ProcessMessage::ChatChunk { .. }));
    }

    #[tokio::test]
    async fn test_subscribe_does_not_receive_unmatched() {
        let router = TopicRouter::new();

        // terminal だけ subscribe
        let (_id, mut rx) = router.subscribe("process/terminal/#").await;

        // 別 capability のメッセージを route
        let msg = ProcessMessage::ChatChunk {
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

        router.route(ProcessMessage::Ping).await;
        router.route(ProcessMessage::TerminalReady).await;

        let (topic1, _) = rx.try_recv().expect("Ping を受信");
        assert_eq!(topic1, "process/star-platinum/event/ping");

        let (topic2, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(topic2, "process/terminal/state/ready");
    }

    #[tokio::test]
    async fn test_single_wildcard_subscribe() {
        let router = TopicRouter::new();

        // 全 capability の state を subscribe
        let (_id, mut rx) = router.subscribe("process/+/state/#").await;

        router.route(ProcessMessage::TerminalReady).await;
        router
            .route(ProcessMessage::SessionList {
                sessions: vec![],
                active_id: None,
            })
            .await;
        // event はマッチしない
        router
            .route(ProcessMessage::ChatChunk {
                content: "x".to_string(),
                done: true,
            })
            .await;

        let (t1, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(t1, "process/terminal/state/ready");

        let (t2, _) = rx.try_recv().expect("SessionList を受信");
        assert_eq!(t2, "process/heavens-door/state/session-list");

        // 3つ目はないはず
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // unsubscribe
    // =========================================================================

    #[tokio::test]
    async fn test_unsubscribe_stops_delivery() {
        let router = TopicRouter::new();

        let (id, mut rx) = router.subscribe("process/terminal/#").await;

        // unsubscribe 前は受信できる
        router.route(ProcessMessage::TerminalReady).await;
        assert!(rx.try_recv().is_ok());

        // unsubscribe
        router.unsubscribe(id).await;

        // unsubscribe 後は配信されない
        router.route(ProcessMessage::TerminalExited).await;
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // 複数 subscriber
    // =========================================================================

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let router = TopicRouter::new();

        let (_id1, mut rx1) = router.subscribe("process/terminal/#").await;
        let (_id2, mut rx2) = router.subscribe("process/+/state/#").await;

        // TerminalReady は両方にマッチ
        router.route(ProcessMessage::TerminalReady).await;

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    // =========================================================================
    // demand-driven production (S2 / doc 27 §4.1 Cap2)
    // =========================================================================

    #[tokio::test]
    async fn test_demand_fires_on_first_and_last_subscriber() {
        use std::sync::atomic::AtomicUsize;
        let router = TopicRouter::new();
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        {
            let s = starts.clone();
            let t = stops.clone();
            router.register_demand("process/terminal/data/+/out", move |_topic, active| {
                if active {
                    s.fetch_add(1, Ordering::Relaxed);
                } else {
                    t.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // 同一 lane topic に 2 subscriber: start は 0→1 の 1 回だけ。
        let (id1, _rx1) = router
            .subscribe("process/terminal/data/vp~conductor/out")
            .await;
        let (id2, _rx2) = router
            .subscribe("process/terminal/data/vp~conductor/out")
            .await;
        assert_eq!(starts.load(Ordering::Relaxed), 1, "start は 0→1 の 1 回");
        assert_eq!(stops.load(Ordering::Relaxed), 0);

        // 1 つ抜けてもまだ残るので stop しない。
        router.unsubscribe(id1).await;
        assert_eq!(stops.load(Ordering::Relaxed), 0, "残 1 なので stop しない");

        // 最後の 1 つが抜けて 1→0 で stop。
        router.unsubscribe(id2).await;
        assert_eq!(stops.load(Ordering::Relaxed), 1, "stop は 1→0 の 1 回");
    }

    #[tokio::test]
    async fn test_demand_per_lane_independent() {
        use std::sync::Mutex as StdMutex;
        let router = TopicRouter::new();
        let started: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        {
            let log = started.clone();
            router.register_demand("process/terminal/data/+/out", move |topic, active| {
                if active {
                    log.lock().unwrap().push(topic);
                }
            });
        }

        let (_a, _ra) = router
            .subscribe("process/terminal/data/vp~conductor/out")
            .await;
        let (_b, _rb) = router
            .subscribe("process/terminal/data/vp~performer~foo/out")
            .await;

        let log = started.lock().unwrap();
        assert_eq!(log.len(), 2, "lane ごとに独立して start");
        assert!(log.contains(&"process/terminal/data/vp~conductor/out".to_string()));
        assert!(log.contains(&"process/terminal/data/vp~performer~foo/out".to_string()));
    }

    #[tokio::test]
    async fn test_demand_ignores_non_matching_pattern() {
        use std::sync::atomic::AtomicUsize;
        let router = TopicRouter::new();
        let fired = Arc::new(AtomicUsize::new(0));
        {
            let f = fired.clone();
            router.register_demand("process/terminal/data/+/out", move |_t, _a| {
                f.fetch_add(1, Ordering::Relaxed);
            });
        }
        // paisley-park の subscribe は terminal demand を発火しない。
        let (_id, _rx) = router.subscribe("process/paisley-park/#").await;
        assert_eq!(fired.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_subscribe_without_demand_hook_is_noop() {
        // hook 未登録なら fire_demand は即 return = 既存 canvas/lanes 経路はゼロ影響。
        let router = TopicRouter::new();
        let (id, _rx) = router.subscribe("process/paisley-park/#").await;
        router.unsubscribe(id).await;
        // panic せず通れば OK (demand_counts は触られない)。
    }

    // =========================================================================
    // Default trait
    // =========================================================================

    #[test]
    fn test_default() {
        let _router = TopicRouter::default();
    }
}
