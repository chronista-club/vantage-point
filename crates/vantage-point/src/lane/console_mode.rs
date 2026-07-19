//! lane ごとの Console mode 永続化（doc 33 §2）
//!
//! Console のエンジンは排他スロット（Tui = PtySlot+claude TUI / Chat = EchoesAgentHost）。
//! 「この lane がどちらのエンジンで動くか」を lane 単位で永続し、SP 再起動を跨いで
//! モードが保たれるようにする（chat の lane に boot 時 PTY を立てない）。
//!
//! [`super::cc_session`] と同型の 1 lane 1 file 1 行 state file。
//! - **書き手**: `console_set_mode` dispatch（モード切替時）
//! - **読み手**: lane spawn 経路（boot 時に PTY spawn 可否を決める）/ `echoes_submit` guard
//! - 置き場: `vp_state_dir()/console_modes/<project>__<lane>`

use std::path::{Path, PathBuf};

/// Console のエンジンモード（doc 33 の状態機械の状態）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleMode {
    /// Act I: PtySlot + claude TUI（ANSI → xterm）。従来の既定。
    #[default]
    Tui,
    /// Act II: EchoesAgentHost（headless claude → EchoesEvent → ChatView）。
    Chat,
}

impl ConsoleMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConsoleMode::Tui => "tui",
            ConsoleMode::Chat => "chat",
        }
    }

    /// state file の 1 行からパース。未知値は None（壊れた file を黙って Tui 扱いしない —
    /// 呼び手が default を選ぶ）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "tui" => Some(ConsoleMode::Tui),
            "chat" => Some(ConsoleMode::Chat),
            _ => None,
        }
    }
}

/// file 名に使えない文字を潰す（cc_session と同一規則）。
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

/// state base dir 配下の mode file path（純関数、テスト用に base 注入）。
pub fn mode_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("console_modes")
        .join(format!("{}__{}", sanitize(project), sanitize(lane)))
}

/// mode を記録する（上書き、1 行）。
pub fn record_in(base: &Path, project: &str, lane: &str, mode: ConsoleMode) -> std::io::Result<()> {
    let path = mode_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, mode.as_str())
}

/// 最後に記録された mode を返す。無い / 形式外は None（呼び手が default = Tui を選ぶ）。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<ConsoleMode> {
    let raw = std::fs::read_to_string(mode_file_in(base, project, lane)).ok()?;
    ConsoleMode::parse(&raw)
}

/// 記録を消す（未記録なら no-op）。
///
/// lane 削除時の state GC 用（[`super::session_registry::clear_in`] と同型）。消さないと
/// 同名 lane を作り直した時に旧 mode が蘇る（ghost file の state leak）。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(mode_file_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// 本番 base（vp_state_dir）での record。
pub fn record(project: &str, lane: &str, mode: ConsoleMode) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, mode)
}

/// 本番 base（vp_state_dir）での clear（lane 削除経路から呼ぶ）。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base（vp_state_dir）での last。
pub fn last(project: &str, lane: &str) -> Option<ConsoleMode> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_last_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 未記録は None（呼び手が default Tui を選ぶ）
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        record_in(tmp.path(), "vp", "conductor", ConsoleMode::Chat).expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor"),
            Some(ConsoleMode::Chat)
        );
        // 上書き（最新が勝つ）
        record_in(tmp.path(), "vp", "conductor", ConsoleMode::Tui).expect("record 2");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor"),
            Some(ConsoleMode::Tui)
        );
    }

    #[test]
    fn clear_removes_record_and_is_idempotent() {
        // lane 削除 = 記録の終端。last が None に戻り、同名 lane 再作成で旧 mode が蘇らない。
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "chore", ConsoleMode::Chat).expect("record");
        clear_in(tmp.path(), "vp", "chore").expect("clear");
        assert_eq!(last_in(tmp.path(), "vp", "chore"), None);
        // 未記録 lane の clear は no-op（二重 delete で Err にしない）
        clear_in(tmp.path(), "vp", "chore").expect("未記録の clear は Ok");
        // 他 lane の記録は巻き添えにしない
        record_in(tmp.path(), "vp", "keep", ConsoleMode::Chat).expect("record");
        clear_in(tmp.path(), "vp", "chore").expect("clear");
        assert_eq!(last_in(tmp.path(), "vp", "keep"), Some(ConsoleMode::Chat));
    }

    #[test]
    fn last_rejects_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("console_modes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__conductor"), "gui\n").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        std::fs::write(dir.join("vp__conductor"), "chat\n").unwrap();
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor"),
            Some(ConsoleMode::Chat)
        );
    }

    #[test]
    fn serde_wire_shape() {
        // LaneInfo wire に載る形（snake_case string）を凍結。
        assert_eq!(
            serde_json::to_string(&ConsoleMode::Chat).unwrap(),
            r#""chat""#
        );
        assert_eq!(
            serde_json::from_str::<ConsoleMode>(r#""tui""#).unwrap(),
            ConsoleMode::Tui
        );
    }
}
