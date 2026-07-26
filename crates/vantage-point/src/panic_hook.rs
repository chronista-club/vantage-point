//! panic の可視化（doc 44 P1 の露払い）。
//!
//! # なぜ必要か
//!
//! VP は `tokio::spawn` を 100 箇所超で使うが、`JoinHandle` の大半は捨てられており
//! `is_panic()` を観測している箇所は無い。 Rust の default は unwind なので、
//! **task 内 panic はその task だけを黙って殺し、プロセスは何事もなく動き続ける**。
//! 結果として「pump が止まる」「lane の event 配信だけが消える」といった
//! *機能だけが欠ける* 障害になり、log にも痕跡が残らない。
//!
//! doc 44 は「repo プロセス = repo 単位の障害隔壁」を前提に fold-in のリスクを見積もって
//! いたが、上記のとおり **task panic に対して repo 境界は何も守っていない**（守るのは
//! deadlock と OOM/abort のみ）。 隔壁を外す前に、まず「何が落ちているか」を見えるようにする。
//!
//! # やり方
//!
//! default hook を *置き換える* のではなく **前段に log を挿して default へ委譲する**。
//! こうすると daemon（launchd 常駐で stderr が捨てられる）でも tracing 経由で
//! `daemon.kdl.log` に残り、foreground 実行時の stderr 出力は従来どおり保たれる。

use std::any::Any;
use std::sync::OnceLock;

static INSTALLED: OnceLock<()> = OnceLock::new();

/// panic payload から人が読めるメッセージを取り出す（calculation）。
///
/// `panic!("...")` の payload は文字列リテラルなら `&'static str`、 format! を伴えば
/// `String` になる。 それ以外（`panic_any` で任意型を投げた場合）は型が判らないので
/// プレースホルダを返す。
fn describe_payload(payload: &(dyn Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<非文字列 payload>"
    }
}

/// 全 panic を tracing に記録する hook を install する（action）。
///
/// 多重呼び出しは無害（初回のみ有効）。 `init_tracing` が経路によって複数回呼ばれても
/// hook が入れ子に積み上がらないよう [`OnceLock`] で守る。
pub fn install() {
    if INSTALLED.set(()).is_err() {
        return;
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<不明>".to_string());
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<無名>").to_string();
        let message = describe_payload(info.payload());
        // daemon は RUST_BACKTRACE を持たないので force_capture。 panic は稀な事象であり、
        // 頻発するなら尚更それを知りたいので、 capture コストは受け入れる。
        let backtrace = std::backtrace::Backtrace::force_capture();

        tracing::error!(
            location = %location,
            thread = %thread_name,
            "panic 発生: {message}\nbacktrace:\n{backtrace}"
        );

        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_payload_reads_static_str() {
        let payload: Box<dyn Any + Send> = Box::new("静的文字列の panic");
        assert_eq!(describe_payload(payload.as_ref()), "静的文字列の panic");
    }

    #[test]
    fn describe_payload_reads_string() {
        let payload: Box<dyn Any + Send> = Box::new(String::from("format! 由来の panic"));
        assert_eq!(describe_payload(payload.as_ref()), "format! 由来の panic");
    }

    #[test]
    fn describe_payload_falls_back_for_unknown_type() {
        // panic_any(42) 相当。 型が判らなくても hook が落ちないことを保証する。
        let payload: Box<dyn Any + Send> = Box::new(42u32);
        assert_eq!(describe_payload(payload.as_ref()), "<非文字列 payload>");
    }

    /// hook の実発火を目視確認するための手動 test。
    ///
    /// panic hook は process-global なので、通常の test run で install すると他 test の
    /// panic 出力まで乗っ取ってしまう。 よって `#[ignore]` で隔離し、明示実行のみとする:
    ///
    /// ```text
    /// cargo test -p vantage-point panic_hook_logs_spawned_task_panic -- --ignored --nocapture
    /// ```
    ///
    /// 期待: `panic 発生: ...` と backtrace が stderr に出て、**プロセスは生き延びる**
    /// （= task panic が unwind で封じ込められている現状の挙動を変えていないことの確認）。
    #[test]
    #[ignore = "process-global な panic hook を install するため、明示実行時のみ"]
    fn panic_hook_logs_spawned_task_panic() {
        tracing_subscriber::fmt().with_test_writer().init();
        install();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.spawn(async { panic!("hook 検証用の意図的な panic") });
        let joined = rt.block_on(handle);

        assert!(joined.is_err(), "spawn した task は panic しているはず");
        assert!(
            joined.unwrap_err().is_panic(),
            "JoinError は panic 由来であるはず"
        );
    }
}
