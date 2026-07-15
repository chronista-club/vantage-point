//! lane ごとの Cursor chat id 永続化（`cc_session` の cursor 版、 Act I `--resume` の土台）
//!
//! cursor-agent は claude と CLI surface が酷似する:
//! - `cursor-agent` = 対話 TUI 起動
//! - `cursor-agent create-chat` = 空チャットを作り chatId を stdout に返す（headless、 handoff 用）
//! - `cursor-agent --resume '<chatId>'` = ID 指名 resume
//!
//! claude の `--continue`（「cwd の最新」を拾う dashboard 罠、 `cc_session` doc / stand_spawner
//! の該当コメント参照）と同型のリスクを避けるため、 cursor でも `--continue` / latest resume は
//! 使わず、 **create-chat で先取りした chatId を指名 resume する**方針に統一する。 その chatId を
//! lane 単位で保持するのが本 module。
//!
//! - **書き手**: `ensure_chat_id`（Act I spawn 時に create-chat を exec して採番・記録）
//! - **読み手**: `ensure_chat_id`（既存 id あれば exec せず再利用）/ `build_stand_command`
//! - 置き場: `vp_state_dir()/cursor_sessions/<project>__<lane>`（1 lane 1 file 1 行 = chatId）
//!
//! **fail-open 原則**: create-chat の失敗（cursor-agent 不在 / timeout / 出力不正）は全て
//! `None` に倒す。 素の `cursor-agent` 起動（新規チャット）に落ちるだけで lane は必ず成立する。

use std::path::{Path, PathBuf};

/// file 名に使えない文字を潰す（`cc_session::sanitize` と同一規則）。
/// separator (`/` `\`) と `.` を `-` に置換 — `__` 結合後も単一 path segment なので
/// join での traversal は元々起きないが、 `..` を残さない方が読み手に安全性が自明。
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '.' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// chat id の正規形（英数 + ハイフン + アンダースコア、 非空）。
///
/// `--resume '<id>'` の single-quote 埋め込みが shell injection にならないための防壁。
/// claude 版（`cc_session::is_valid_session_id`）は `-` のみ許容だが、 cursor の chatId 形式は
/// 未知なので `_` も許容する（`[A-Za-z0-9_-]`）。 書き込み・読み出しの両側で同じ検証を使い、
/// state file が常に正規形であることを保証する。
pub(crate) fn is_valid_chat_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// state base dir 配下の chat file path（純関数、 テスト用に base 注入）
pub fn chat_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("cursor_sessions")
        .join(format!("{}__{}", sanitize(project), sanitize(lane)))
}

/// chat id を記録する（上書き、 1 行）。
///
/// 形式外（空 / injection 形）は**書かずに** Ok を返す — 既存の正常な記録を壊れた値で
/// 上書きしない（silent 退化防止、 `cc_session` と同方針）。
pub fn record_in(base: &Path, project: &str, lane: &str, chat_id: &str) -> std::io::Result<()> {
    if !is_valid_chat_id(chat_id) {
        return Ok(());
    }
    let path = chat_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, chat_id)
}

/// 最後に記録された chat id を返す。
///
/// 無い / 空 / 形式外（`[A-Za-z0-9_-]` 以外を含む）は None — 壊れた file を `--resume` に
/// 渡さない + single-quote 埋め込みを quote 安全にする。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    let raw = std::fs::read_to_string(chat_file_in(base, project, lane)).ok()?;
    let trimmed = raw.trim();
    if !is_valid_chat_id(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

/// 記録を消す（未記録なら no-op）。
///
/// 「素の新規チャットを張る」（= fresh）の意味を state 側で表現する手段。 消した後は
/// `last` が None になるので、 次の非 fresh spawn で `ensure_chat_id` が新しい chatId を採番する。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(chat_file_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// 本番 base（vp_state_dir）での record
pub fn record(project: &str, lane: &str, chat_id: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, chat_id)
}

/// 本番 base（vp_state_dir）での clear（fresh restart から呼ぶ）
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base（vp_state_dir）での last
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

/// cursor-agent の実行パスを解決する（`agent::get_claude_cli_path` と同じ問題への対処）。
///
/// daemon は launchd 起動だと PATH が細く、 Rust から exec する create-chat は login shell を
/// 経由しない（床の login shell 注入は Act I の `cursor-agent` 起動だけに効く）。 明示解決で
/// 確実に見つける:
/// 1. 現在の PATH で `which cursor-agent` が当たればそれ
/// 2. `$HOME/.local/bin/cursor-agent`（well-known install 先）が実在すればそれ
/// 3. どちらも無ければ素の `"cursor-agent"`（PATH に委ねる）
///
/// Act II（[`crate::echoes::cursor_host`]）の turn spawn も同じ解決を使うため crate 内公開。
pub(crate) fn cursor_cli_path() -> String {
    if let Ok(output) = std::process::Command::new("which")
        .arg("cursor-agent")
        .output()
        && output.status.success()
        && let Ok(path) = String::from_utf8(output.stdout)
    {
        let path = path.trim();
        if !path.is_empty() {
            return path.to_string();
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let well_known = format!("{}/.local/bin/cursor-agent", home);
    if std::path::Path::new(&well_known).exists() {
        return well_known;
    }

    "cursor-agent".to_string()
}

/// `cursor-agent create-chat` の実行に許す最大時間。 超過は kill して None に倒す（fail-open）。
const CREATE_CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// lane の chatId を確保する（base 注入版、 テスト用）。
///
/// 1. `last_in` に有効値があればそれを返す（**exec しない** — テスト決定性の要）
/// 2. 無ければ `cursor-agent create-chat` を cwd=project_dir で exec（chat の workspace 紐付けの
///    ため cwd が重要）。 timeout `CREATE_CHAT_TIMEOUT`
/// 3. stdout の最後の非空行を chatId として取り、 `is_valid_chat_id` 検証 → record → 返す
/// 4. exec 失敗 / timeout / 検証失敗は全て None（fail-open、 warn ログ）
pub fn ensure_chat_id_in(
    base: &Path,
    project: &str,
    lane: &str,
    project_dir: &Path,
) -> Option<String> {
    // (1) 既存 id があれば exec せず再利用（決定的）。
    if let Some(existing) = last_in(base, project, lane) {
        return Some(existing);
    }

    // (2) create-chat を exec して chatId を採番する。
    let created = run_create_chat(project_dir)?;

    // (3) 検証 → record。 record_in は形式外を no-op で吸収するが、 返す前に自分でも検証して
    //     不正なら fail-open で None に倒す（素の cursor-agent 起動に落とす）。
    if !is_valid_chat_id(&created) {
        tracing::warn!(
            "cursor create-chat の出力が chatId 形式外（project={project} lane={lane}）: {:?}",
            created
        );
        return None;
    }
    if let Err(e) = record_in(base, project, lane, &created) {
        tracing::warn!("cursor chatId の永続に失敗（project={project} lane={lane}）: {e}");
        // 記録に失敗しても採番済 id 自体は今回の spawn に使えるので返す。
    }
    Some(created)
}

/// `cursor-agent create-chat` を cwd=project_dir で exec し、 stdout 末尾の非空行を返す。
///
/// spawn_blocking 文脈から呼ばれる前提なので blocking（try_wait ポーリング）で書く。
/// exec 不能 / timeout は None（fail-open）。
fn run_create_chat(project_dir: &Path) -> Option<String> {
    let program = cursor_cli_path();
    let mut child = match std::process::Command::new(&program)
        .arg("create-chat")
        .current_dir(project_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("cursor-agent create-chat の spawn に失敗（program={program}）: {e}");
            return None;
        }
    };

    // timeout ポーリング: try_wait で終了を待ち、 超過したら kill して None。
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() >= CREATE_CHAT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "cursor-agent create-chat が {}s を超過 → kill（fail-open）",
                        CREATE_CHAT_TIMEOUT.as_secs()
                    );
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("cursor-agent create-chat の wait に失敗: {e}");
                return None;
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("cursor-agent create-chat の出力取得に失敗: {e}");
            return None;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "cursor-agent create-chat が非 0 終了（status={:?}）: {}",
            output.status.code(),
            stderr.trim()
        );
        return None;
    }

    // stdout の最後の非空行を chatId として取る（前後の飾り行があっても末尾を採る）。
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// 本番 base（vp_state_dir）での ensure_chat_id（Act I spawn から呼ぶ）。
pub fn ensure_chat_id(project: &str, lane: &str, project_dir: &Path) -> Option<String> {
    ensure_chat_id_in(&crate::config::vp_state_dir(), project, lane, project_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_sanitizes_project_and_lane() {
        let p = chat_file_in(Path::new("/base"), "creo.memories", "conductor");
        assert_eq!(
            p,
            Path::new("/base/cursor_sessions/creo-memories__conductor")
        );
        let p = chat_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/cursor_sessions/a-b__---evil"));
    }

    #[test]
    fn is_valid_chat_id_allows_underscore() {
        // cursor の chatId は形式未知なので `_` も許容（claude 版と唯一異なる点）。
        assert!(is_valid_chat_id("abc-123_XYZ"));
        assert!(is_valid_chat_id("chat_0196"));
        // injection 形 / 空 / 空白は拒否。
        assert!(!is_valid_chat_id(""));
        assert!(!is_valid_chat_id("bad id"));
        assert!(!is_valid_chat_id("a'; rm -rf /"));
        assert!(!is_valid_chat_id("has.dot"));
    }

    #[test]
    fn record_rejects_invalid_chat_id() {
        // 形式外は書かない — 既存の正常な記録を壊れた値で上書きしない。
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "good_id-1").expect("record");
        record_in(tmp.path(), "vp", "conductor", "").expect("空は no-op");
        record_in(tmp.path(), "vp", "conductor", "bad id'; rm").expect("形式外は no-op");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("good_id-1"),
            "正常な記録が保持される"
        );
    }

    #[test]
    fn clear_removes_record_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "old-id").expect("record");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        // 未記録 lane の clear は no-op。
        clear_in(tmp.path(), "vp", "conductor").expect("未記録の clear は Ok");
        // 他 lane の記録は巻き添えにしない。
        record_in(tmp.path(), "vp", "performer-a", "keep-id").expect("record");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        assert_eq!(
            last_in(tmp.path(), "vp", "performer-a").as_deref(),
            Some("keep-id")
        );
    }

    #[test]
    fn record_and_last_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "0196_chat-id").expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("0196_chat-id")
        );
        assert_eq!(last_in(tmp.path(), "vp", "w1"), None);
        // 上書き（最新が勝つ）。
        record_in(tmp.path(), "vp", "conductor", "newer_id").expect("record 2");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("newer_id")
        );
    }

    #[test]
    fn last_rejects_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("cursor_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__conductor"), "  \n").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        std::fs::write(dir.join("vp__conductor"), "abc'; rm -rf /'").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        std::fs::write(dir.join("vp__conductor"), "0196_abc\n").unwrap();
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("0196_abc")
        );
    }

    /// 既存 id があれば `ensure_chat_id_in` は create-chat を **exec せず**再利用する
    /// （決定性の要 — 開発機で cursor-agent が exec されずネットワークにも触れない）。
    #[test]
    fn ensure_chat_id_reuses_existing_without_exec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "preset_chat-42").expect("record");
        // project_dir は使われない（exec に到達しないため）。存在しない path でも OK。
        let got = ensure_chat_id_in(tmp.path(), "vp", "conductor", Path::new("/nonexistent"));
        assert_eq!(got.as_deref(), Some("preset_chat-42"));
    }
}
