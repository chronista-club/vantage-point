//! debug log tail — R sidebar（sidebar view modes、2026-08-01）への行供給。
//!
//! webview の `debuglog:watch` を受けて log file（`vp_log_dir()` の
//! `app.kdl.log` / `daemon.kdl.log`）を tail し、`AppEvent::DebugLogChunk` で
//! event loop に返す。event loop はそれを push envelope `debuglog:lines` で
//! webview へ流す（消費者主導 — 見ていない log は読まない）。
//!
//! ## 停止の仕組み（最後の watch が勝つ）
//!
//! thread は世代番号（`generation`）を持って生まれ、共有カウンタ（`current`）が
//! 自分の世代から動いたら次の poll で静かに退場する。unwatch も新しい watch も
//! カウンタを進めるだけ — join 待ちや channel の後始末が要らない。
//!
//! ## rotate 追従
//!
//! log は `file-rotate` が bare 名のまま切り詰める（`log_init.rs`）。file 長が
//! 既読 offset より縮んだら rotate とみなし、backlog を読み直して `reset=true` で
//! 表示ごと差し替える。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tao::event_loop::EventLoopProxy;

use crate::terminal::AppEvent;

/// watch 開始時に遡る backlog の上限 byte 数（≈ 数百行。表示側の cap は 2000 行）。
const BACKLOG_BYTES: u64 = 64 * 1024;
/// 追記 poll の間隔。tail -f 相当の体感と CPU の釣り合い。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// source 名 → log file path。`vp_log_dir()` は `VP_PROFILE` を honor 済み
/// （dev は `~/.local/state/vp-dev/log/`）なので、ここで profile を意識しない。
pub fn log_path(source: &str) -> Option<PathBuf> {
    let name = match source {
        "app" => "app.kdl.log",
        "daemon" => "daemon.kdl.log",
        _ => return None,
    };
    Some(vp_paths::vp_log_dir().join(name))
}

/// tail thread を起こす。停止は `current` を進めるだけ（上記 module doc）。
pub fn spawn_tail(
    source: String,
    path: PathBuf,
    generation: u64,
    current: Arc<AtomicU64>,
    proxy: EventLoopProxy<AppEvent>,
) {
    let spawned = std::thread::Builder::new()
        .name("vp-debuglog-tail".into())
        .spawn(move || tail_loop(&source, &path, generation, &current, &proxy));
    if let Err(e) = spawned {
        // 表示されないだけに留める（tail は診断の道具 — app を道連れにしない）
        tracing::warn!("debuglog tail thread の起動に失敗: {e}");
    }
}

fn tail_loop(
    source: &str,
    path: &Path,
    generation: u64,
    current: &AtomicU64,
    proxy: &EventLoopProxy<AppEvent>,
) {
    // 初回 backlog（file 不在も 1 行の案内で reset — 「開いても無反応」を作らない）
    let mut offset = match read_backlog(path) {
        Ok((lines, end)) => {
            send(proxy, source, true, lines, generation);
            end
        }
        Err(e) => {
            send(
                proxy,
                source,
                true,
                vec![format!("({} を読めない: {e})", path.display())],
                generation,
            );
            0
        }
    };
    // 追記読みで行末（\n）に達していない断片の持ち越し
    let mut partial = String::new();

    loop {
        std::thread::sleep(POLL_INTERVAL);
        if current.load(Ordering::Relaxed) != generation {
            return; // 新しい watch / unwatch に世代を譲った
        }
        let len = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(_) => continue, // 一時的な不在（rotate の瞬間等）は次の poll で拾う
        };
        if len < offset {
            // rotate: 現行 file が切り詰められた。表示ごと backlog から作り直す。
            partial.clear();
            if let Ok((lines, end)) = read_backlog(path) {
                offset = end;
                send(proxy, source, true, lines, generation);
            }
            continue;
        }
        if len == offset {
            continue;
        }
        let Some(chunk) = read_range(path, offset, len) else {
            continue;
        };
        offset = len;
        partial.push_str(&chunk);
        // 完全な行だけ切り出す（最後の \n 以降は次 chunk へ持ち越し）
        if let Some(idx) = partial.rfind('\n') {
            let complete: String = partial.drain(..=idx).collect();
            let lines: Vec<String> = complete.lines().map(str::to_string).collect();
            if !lines.is_empty() {
                send(proxy, source, false, lines, generation);
            }
        }
    }
}

/// `DebugLogChunk` を event loop へ返す。送れない = event loop 消滅なので以後の送信も無意味
/// だが、次の poll の世代チェックが thread を畳むためここでは無視でよい。
fn send(
    proxy: &EventLoopProxy<AppEvent>,
    source: &str,
    reset: bool,
    lines: Vec<String>,
    generation: u64,
) {
    let _ = proxy.send_event(AppEvent::DebugLogChunk {
        source: source.to_string(),
        reset,
        lines,
        generation,
    });
}

/// 末尾 `BACKLOG_BYTES` を行単位で読む。戻りは (行群, file 終端 offset)。
/// 途中から読んだ場合の先頭は行の断片なので捨てる。
fn read_backlog(path: &Path) -> std::io::Result<(Vec<String>, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(BACKLOG_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    // log は UTF-8 前提だが、切り出し境界が multi-byte を割る可能性があるので lossy で受ける
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Ok((lines, len))
}

/// `from..to` の byte range を lossy 文字列で読む。失敗は None（次の poll でやり直す）。
fn read_range(path: &Path, from: u64, to: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![0u8; usize::try_from(to - from).ok()?];
    f.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// source 名の解決: 既知 2 種は `vp_log_dir()` 配下の bare 名、未知は None。
    /// bare 名は `log_init.rs`（app）/ daemon 側の書き込み先と対 — rename 時は両側を揃える。
    #[test]
    fn log_path_resolves_known_sources() {
        assert!(log_path("app").is_some_and(|p| p.ends_with("log/app.kdl.log")));
        assert!(log_path("daemon").is_some_and(|p| p.ends_with("log/daemon.kdl.log")));
        assert!(log_path("evil").is_none());
    }

    /// backlog: 全文が上限内なら全行 + 終端 offset が返る。
    #[test]
    fn read_backlog_returns_all_lines_when_small() {
        let dir = std::env::temp_dir().join("vp-debuglog-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("small.log");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let (lines, end) = read_backlog(&path).unwrap();
        assert_eq!(lines, vec!["one", "two", "three"]);
        assert_eq!(end, 14);
        let _ = std::fs::remove_file(&path);
    }
}
