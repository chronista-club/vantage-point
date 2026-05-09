//! Stage 1 (ADR-0001): Rust 側 alacritty_terminal::Term<T> attach 機構
//!
//! 既存 PtySlot::output_tx (broadcast::Sender<Vec<u8>>) を subscribe して、
//! VT bytes を TerminalState::feed_bytes() に流す task を 1 Lane あたり 1 つ起動。
//!
//! Rust コアが「真実の cell 状態」 を保持することで、 以下の foundation になる:
//! - Web/iPad/Vision Pro frontend (= xterm.js 以外の client) への配信
//! - xterm.js 側 ghost char (mem_1CaVpvsBKR3ckieRXo1nwr) の整合性 oracle (Stage 3)
//!
//! 関連:
//! - ADR: docs/adr/0001-web-terminal-dual-track.md
//! - 既存 broadcast::Sender: crates/vantage-point/src/daemon/pty_slot.rs:100
//! - 既存 TerminalState: crates/vantage-point/src/terminal/state.rs:174

use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::terminal::TerminalState;

/// 1 Lane あたり 1 つ。 PtySlot subscribe → TerminalState 駆動 task の lifecycle handle。
///
/// `state` は TUI mode と同 pattern (= `Arc<std::sync::Mutex<TerminalState>>`)、
/// 外部から `state.lock().snapshot()` で grid snapshot を取得できる (Stage 3 oracle 想定)。
pub struct TermAttach {
    /// snapshot 取得 entry point (Stage 3 oracle で使う想定)
    pub state: Arc<Mutex<TerminalState>>,
    /// resize 通知 channel (PTY resize と並走で grid resize を反映)
    resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    /// blocking task handle (Drop で abort)
    handle: JoinHandle<()>,
}

impl TermAttach {
    /// PTY broadcast subscriber を消費する blocking task を起動。
    ///
    /// `rx` には PtySlot::spawn の戻り値の `initial_rx` (= broadcast::channel 作成と同時に
    /// 取得した Receiver、 reader_task が start する**前**に subscribe 済) を渡せば
    /// race フリーで PTY 最初の output から取り逃しなし。
    pub fn spawn(rx: broadcast::Receiver<Vec<u8>>, cols: u16, rows: u16) -> Self {
        let state = Arc::new(Mutex::new(TerminalState::new(cols as usize, rows as usize)));
        let (resize_tx, resize_rx) = mpsc::unbounded_channel();
        let state_for_task = state.clone();
        let handle = tokio::task::spawn_blocking(move || {
            run_loop(rx, resize_rx, state_for_task);
        });
        Self {
            state,
            resize_tx,
            handle,
        }
    }

    /// PTY resize と並走で Term<T> grid を resize する。
    /// LanePool::resize_lane が PtySlot::resize と同期して呼ぶ想定。
    /// task 死亡後の send は silent ignore (= 失敗 = task abort 後、 害なし)。
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resize_tx.send((cols, rows));
    }
}

impl Drop for TermAttach {
    fn drop(&mut self) {
        // PtySlot drop で broadcast::Sender が closed → blocking_recv が Closed error で
        // 自然終了するが、 念のため明示 abort で確実に reclaim する。
        self.handle.abort();
    }
}

/// blocking pool で動く本体 loop。
/// resize 通知を non-blocking で消化してから broadcast を blocking_recv で待つ。
fn run_loop(
    mut rx: broadcast::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::UnboundedReceiver<(u16, u16)>,
    state: Arc<Mutex<TerminalState>>,
) {
    loop {
        // resize は non-blocking で先に消化 (broadcast 待ちの間に蓄積した分も含む)。
        // 同 tick で複数 resize が来た場合は最後のものが winner。
        while let Ok((cols, rows)) = resize_rx.try_recv() {
            if let Ok(mut s) = state.lock() {
                s.resize(cols as usize, rows as usize);
            }
        }
        match rx.blocking_recv() {
            Ok(bytes) => {
                if let Ok(mut s) = state.lock() {
                    s.feed_bytes(&bytes);
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // broadcast channel buffer (= 256 chunk) がオーバーフロー。
                // Term<T> は再 play 不可なので drift する → Stage 3 oracle で resync 検出を担う。
                tracing::warn!(
                    target: "vp::term_attach",
                    "lagged: {} chunks dropped — Term<T> may drift. Stage 3 oracle で resync 想定",
                    n
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::info!(target: "vp::term_attach", "broadcast closed, exit loop");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    /// Hello を流して TerminalState の cell に反映されることを確認。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_term_attach_feed_basic() {
        let (tx, rx) = broadcast::channel(16);
        let attach = TermAttach::spawn(rx, 80, 24);
        tx.send(b"Hello".to_vec()).unwrap();
        // spawn_blocking task が処理する隙を与える
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snap = attach.state.lock().unwrap().snapshot();
        assert_eq!(snap.cells[0][0].ch, 'H');
        assert_eq!(snap.cells[0][1].ch, 'e');
        assert_eq!(snap.cells[0][2].ch, 'l');
        assert_eq!(snap.cells[0][3].ch, 'l');
        assert_eq!(snap.cells[0][4].ch, 'o');
    }

    /// resize() で grid 寸法が変わることを確認。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_term_attach_resize() {
        let (tx, rx) = broadcast::channel(16);
        let attach = TermAttach::spawn(rx, 80, 24);
        // resize_rx は broadcast 待ちで blocking 中のため、 trigger となる broadcast を 1 つ送ると
        // 直前の try_recv で resize が消化される。
        attach.resize(120, 40);
        tx.send(b"X".to_vec()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snap = attach.state.lock().unwrap().snapshot();
        assert_eq!(snap.cols, 120);
        assert_eq!(snap.lines, 40);
    }

    /// Drop で task が abort されることを ─ 厳密検証は probabilistic だが、
    /// 少なくとも compile / Drop 走行を確認する smoke test。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_term_attach_drop() {
        let (_tx, rx) = broadcast::channel(16);
        let attach = TermAttach::spawn(rx, 80, 24);
        let _state_clone = attach.state.clone();
        drop(attach);
        // abort signal の伝播待ち
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // task が abort 済 / または Closed で自然終了している。
        // 副作用 (panic 等) がないことが確認できれば smoke 成立。
    }
}
