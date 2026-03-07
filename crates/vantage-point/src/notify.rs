//! macOS DistributedNotificationCenter を使ったプロセス間通知
//!
//! VP プロセスの start/stop イベントをメニューバーアプリに即座に通知する。
//! OS カーネルが仲介するため、追加サーバー不要・数ミリ秒で配信。

/// 通知名の定数
pub const NOTIFICATION_PROCESS_CHANGED: &str = "club.chronista.vp.process.changed";

/// Process の状態変更を通知する
///
/// メニューバーアプリ（VantagePoint.app）がこの通知を受信して即座に UI を更新する。
/// object に "started:PORT" / "stopped:PORT" 形式でイベント情報を含める。
#[cfg(target_os = "macos")]
pub fn post_process_changed(port: u16, event: &str) {
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};

    let center = NSDistributedNotificationCenter::defaultCenter();
    let name = NSString::from_str(NOTIFICATION_PROCESS_CHANGED);
    let object = NSString::from_str(&format!("{}:{}", event, port));

    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(
            &name,
            Some(&object),
            None,
            true, // バックグラウンドアプリにも即座に配信
        );
    }

    tracing::debug!(
        "Posted DistributedNotification: {} (port={}, event={})",
        NOTIFICATION_PROCESS_CHANGED,
        port,
        event
    );
}

#[cfg(not(target_os = "macos"))]
pub fn post_process_changed(_port: u16, _event: &str) {
    // macOS 以外では何もしない
}
