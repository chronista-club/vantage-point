//! macOS DistributedNotificationCenter を使ったプロセス間通知
//!
//! VP プロセスの start/stop イベントをメニューバーアプリに即座に通知する。
//! OS カーネルが仲介するため、追加サーバー不要・数ミリ秒で配信。

/// 通知名の定数
pub const NOTIFICATION_PROCESS_CHANGED: &str = "tech.anycreative.vp.process.changed";
pub const NOTIFICATION_CANVAS_OPEN: &str = "tech.anycreative.vp.canvas.open";

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

/// Canvas を開く通知を送信
///
/// Native App が受信して CanvasView パネルを表示する。
/// object に "PORT" を含め、どの SP の Canvas を開くかを特定する。
#[cfg(target_os = "macos")]
pub fn post_canvas_open(port: u16) {
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};

    let center = NSDistributedNotificationCenter::defaultCenter();
    let name = NSString::from_str(NOTIFICATION_CANVAS_OPEN);
    let object = NSString::from_str(&port.to_string());

    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(
            &name,
            Some(&object),
            None,
            true,
        );
    }

    tracing::debug!(
        "Posted DistributedNotification: {} (port={})",
        NOTIFICATION_CANVAS_OPEN,
        port
    );
}

#[cfg(not(target_os = "macos"))]
pub fn post_canvas_open(_port: u16) {}

#[cfg(not(target_os = "macos"))]
pub fn post_process_changed(_port: u16, _event: &str) {
    // macOS 以外では何もしない
}
