//! tracing-subscriber 初期化 (filter resilience + KDL appender + KdlFormatter wiring)
//!
//! ## 役割
//! `pub fn run()` の起動冒頭で 1 回だけ呼ばれる、 vp-app の全 tracing log の入口。
//! - log file path 解決 (OS 別)
//! - `tracing_appender::rolling::never` で `app.kdl.log` に append
//! - `EnvFilter` の noise 抑制 + vp_app target silent 化対策 (PR #235)
//! - `KdlFormatter` を inject して KDL 1-line 出力
//! - 起動 info ログ
//!
//! ## なぜ別 module?
//! 元は `app.rs::run()` の冒頭 ~85 行を占めていたが、 `app.rs` 全体が 3126 行に
//! 膨らんでいた中で「責務 1 文で言える」 関数として独立させた (R-1、
//! `docs/design/11-vp-app-refactor.md` § 3.1 / `mem_1CaaaDoXHZvhR46ZfLN6jx`)。
//!
//! 将来 Phase B (`mem_1CaSiJkD9HATDY2srrv6D4`) で `vantage-core` crate に move する
//! 候補。 file 名を `log_init` (= `vantage-core::log_init::init_tracing()` のような
//! cross-crate 移管時に自然な名前) にしてある。
//!
//! ## file writer に切替 (Win 制約)
//!
//! Win GUI subsystem の vp-app では stderr handle が NUL 化される (CONIN$/CONOUT$ も無い)。
//! PowerShell の Start-Process -RedirectStandardOutput でも GUI subsystem に対しては
//! 確実に redirect が効かない。
//!
//! 解決: tracing-appender で **file に直接書き込む**。
//! XDG restructure: log dir は全 OS で `vp_log_dir()` (= `~/.local/state/vp/log/`、
//! `$XDG_STATE_HOME` 優先)。 macOS の Application Support / Library/Logs は廃止。
//!
//! mise run win の polling tail が同 file を見る。

use std::path::PathBuf;

/// `init_tracing` の戻り値。 caller が log_dir を後続処理で参照する場合の export。
///
/// 将来 (Phase B 以降) field 増加時に signature breaking change を避けるため struct で wrap。
pub struct LogInitResult {
    /// 解決された log dir (OS 別)。 `app.kdl.log` 等が書き込まれる場所。
    pub log_dir: PathBuf,
}

/// tracing-subscriber を 1 回だけ初期化する。 `pub fn run()` 冒頭で呼ぶ。
///
/// VP-100 follow-up: KDL 1-line formatter で構造化ログ出力
/// (color disable + KdlFormatter で機械可読 / grep 可能な log を吐く)
pub fn init_tracing() -> LogInitResult {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // XDG restructure: log は state zone 配下に全 OS 統一。
    // `~/.local/state/vp/log/app.kdl.log` (= `$XDG_STATE_HOME/vp/log/`)。
    // macOS の `~/Library/Logs/Vantage/` も daemon の `vp_data_dir()/logs/` も廃止、
    // 全 process の log を `vp_log_dir()` 一極集中。
    let log_dir = crate::paths::vp_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, "app.kdl.log");

    // Phase 5-C: log filter の noise 抑制 (2026-04-28 観測: 23MB log の 70% が hyper_util::pool、
    //   25% が vp_app::terminal の PTY I/O event だった)。 vp_app の他モジュールは info で残し、
    //   noise 源を warn まで上げる。 必要なら RUST_LOG 環境変数で override 可。
    //
    // Phase 5-D fix: ユーザ shell の `RUST_LOG=vantage_point=debug` 等が `try_from_default_env` で
    //   default を完全 override してしまい、 hyper_util の debug log が大量に残っていた。
    //   読み込み後に `add_directive` で noise 源を強制 warn 上書きする (same-target は replace)。
    //
    // Phase 5-D follow-up (2026-05-01, PR #235): `RUST_LOG=vantage_point=debug` のように VP module を
    //   含まない設定だと、 EnvFilter のデフォ「明示されてない target は OFF」 仕様で **vp_app::* が
    //   完全 silent** になる回帰が発生 (= `[osc99-keys:...]` / `[term-title:...]` 等の dbg log が
    //   どこにも flow しない)。 user が `vp_app=...` を明示してれば尊重、 無ければ `vp_app=info` を
    //   default で追加して dbg log を見える化する。
    let mut env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "vp_app=info,vp_app::terminal=warn,vantage_point=info",
            )
        })
        .add_directive("hyper_util=warn".parse().expect("static directive"))
        .add_directive("hyper=warn".parse().expect("static directive"))
        .add_directive("reqwest=warn".parse().expect("static directive"))
        .add_directive("h2=warn".parse().expect("static directive"))
        .add_directive("rustls=warn".parse().expect("static directive"));
    let user_rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    if !user_rust_log
        .split(',')
        .any(|d| d.trim().starts_with("vp_app"))
    {
        env_filter = env_filter.add_directive("vp_app=info".parse().expect("static directive"));
    }

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .event_format(crate::log_format::KdlFormatter)
                .with_writer(file_appender),
        )
        .try_init();

    tracing::info!(
        log_dir = %log_dir.display(),
        "vp-app 起動 (Creo UI mint-dark)"
    );

    LogInitResult { log_dir }
}
