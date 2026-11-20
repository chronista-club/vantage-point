//! 履歴を host から受け取って配り直すための口（ACP engine 用、agent-chat 設計書 01「履歴」）。
//!
//! ACP engine は会話の履歴の正を engine が持つ。host（共有 crate `agent-chat-acp` の
//! `AcpClientHost`）は `session/load` が流した過去の会話と、その後の会話中の event を控えに持ち、
//! `attach()` で「控え」と「その続きの受け口」を**同じ lock の中で**渡す。控えに入った event は
//! 受け口に出ず、控えに無い event は必ず受け口に出る。
//!
//! VP は pump が topic への唯一の書き手なので、配り直しも pump がやる
//! （`repo::conversation_pump`）。横から控えを読んで topic に流すと、控えを取ってから
//! `ReplayStart` が届くまでの間に pump が流した event が消えるか重なる。
//!
//! 本 module は pump と host の間の型だけを持つ（pump が `agent-chat-acp` の型を直接知らずに済み、
//! test は偽の host を差せる）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use super::event::ConversationEvent;

/// 控えと、その続きの受け口。
pub struct ReplayAttach {
    /// 控え（過去の会話 + ここまでの会話中の event）。`ReplayStart` 〜 `ReplayEnd` で包んで配る。
    pub events: Vec<ConversationEvent>,
    /// 今 turn の最中か（`ReplayEnd` にそのまま載せる）。
    pub in_flight: bool,
    /// 上限を超えて、古い方を落としたか・1 件を切ったか。
    pub truncated: bool,
    /// 控えの続き。pump はこれに差し替えて読み続ける。
    pub rx: broadcast::Receiver<ConversationEvent>,
}

pub type AttachFuture = Pin<Box<dyn Future<Output = Result<ReplayAttach, String>> + Send>>;

/// 履歴を渡せる host。
pub trait ReplaySource: Send + Sync + 'static {
    /// 控えと続きの受け口を一緒に取る。会話の確定まで待つ。失敗は engine の文。
    ///
    /// 契約: 返した `events` に入った event は `rx` に出ず、入っていない event は必ず `rx` に出る。
    fn attach(&self) -> AttachFuture;
    /// 最後に出した `SessionInit`（`ReplayStart` の直後に配り直す）。
    fn session_init(&self) -> Option<ConversationEvent>;
}

/// pump に渡す口: 履歴の出どころと、「配り直して」の依頼の受け口。
pub struct ReplayPort {
    pub source: Arc<dyn ReplaySource>,
    pub requests: mpsc::UnboundedReceiver<()>,
}

/// 「配り直して」を pump に頼む側。host が持つ。
#[derive(Clone)]
pub struct ReplayRequester(mpsc::UnboundedSender<()>);

impl ReplayRequester {
    /// 依頼の口の対を作る。
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), rx)
    }

    /// 配り直しを頼む。pump がもう居なければ false。
    pub fn request(&self) -> bool {
        self.0.send(()).is_ok()
    }
}
