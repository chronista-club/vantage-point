//! Claude CLI セッション一覧の取得
//!
//! `~/.claude/projects/{key}/` から JSONL ファイルを読み、
//! セッション ID・更新日時・最初のユーザーメッセージを抽出する。

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::SystemTime;

/// セッション起動モード
#[derive(Debug, Clone)]
pub enum SessionMode {
    /// 前回セッションを継続（--continue）
    Continue,
    /// 新規セッション（引数なし）
    New,
    /// 指定セッションを再開（--resume <id>）
    Resume(String),
}

/// Claude CLI セッション情報
#[derive(Debug, Clone)]
pub struct ClaudeSession {
    /// セッション UUID
    pub id: String,
    /// ファイル更新日時
    pub modified: SystemTime,
    /// 最初のユーザーメッセージ（サマリ用、80文字まで）
    pub summary: String,
    /// メッセージ数（概算: type=="user" と type=="assistant" の行数）
    pub message_count: usize,
}

/// プロジェクトディレクトリに対応する Claude セッション一覧を取得
///
/// `~/.claude/projects/{key}/` から JSONL ファイルを読み取り、
/// 更新日時の降順でソートして返す。
pub fn list_sessions(project_dir: &str) -> Vec<ClaudeSession> {
    let key = project_dir_to_key(project_dir);
    let sessions_dir = dirs::home_dir()
        .map(|h| h.join(".claude").join("projects").join(&key))
        .unwrap_or_default();

    if !sessions_dir.is_dir() {
        return vec![];
    }

    let mut sessions: Vec<ClaudeSession> = fs::read_dir(&sessions_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            // .jsonl ファイルのみ
            if path.extension()?.to_str()? != "jsonl" {
                return None;
            }

            let id = path.file_stem()?.to_str()?.to_string();

            // UUID 形式のみ（8-4-4-4-12）
            if id.len() != 36 || id.chars().filter(|c| *c == '-').count() != 4 {
                return None;
            }

            let metadata = fs::metadata(&path).ok()?;
            let modified = metadata.modified().ok()?;

            // 空ファイルは除外
            if metadata.len() == 0 {
                return None;
            }

            let (summary, message_count) = parse_session_summary(&path);

            Some(ClaudeSession {
                id,
                modified,
                summary,
                message_count,
            })
        })
        .collect();

    // 更新日時の降順
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// セッション JSONL からサマリ（最初のユーザーメッセージ）とメッセージ数を取得
fn parse_session_summary(path: &PathBuf) -> (String, usize) {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (String::new(), 0),
    };

    let reader = BufReader::new(file);
    let mut first_user_msg = String::new();
    let mut msg_count = 0;

    for line in reader.lines().take(500) {
        // 最初の 500 行だけ読む（パフォーマンス）
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // 軽量パース: type フィールドだけ確認
        if line.contains("\"type\":\"user\"") {
            msg_count += 1;
            if first_user_msg.is_empty() {
                // 最初のユーザーメッセージからテキストを抽出
                first_user_msg = extract_user_text(&line);
            }
        } else if line.contains("\"type\":\"assistant\"") {
            msg_count += 1;
        }
    }

    (first_user_msg, msg_count)
}

/// JSONL 行からユーザーメッセージテキストを抽出（軽量パース）
fn extract_user_text(line: &str) -> String {
    // serde_json でパースして content[0].text を取得
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
        && let Some(content) = val.get("message").and_then(|m| m.get("content"))
    {
        // content が文字列の場合
        if let Some(s) = content.as_str() {
            return truncate(s, 80);
        }
        // content が配列の場合（[{type: "text", text: "..."}]）
        if let Some(arr) = content.as_array()
            && let Some(first) = arr.first()
            && let Some(text) = first.get("text").and_then(|t| t.as_str())
        {
            return truncate(text, 80);
        }
    }
    String::new()
}

/// 文字列を指定文字数で切り詰め（改行も除去）
fn truncate(s: &str, max: usize) -> String {
    let clean: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if clean.chars().count() > max {
        let truncated: String = clean.chars().take(max).collect();
        format!("{}…", truncated)
    } else {
        clean
    }
}

/// プロジェクトディレクトリを Claude のプロジェクトキーに変換
///
/// `/path/to/vantage-point` → `-Users-makoto-repos-vantage-point`
fn project_dir_to_key(project_dir: &str) -> String {
    project_dir.replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_dir_to_key() {
        assert_eq!(
            project_dir_to_key("/path/to/vantage-point"),
            "-Users-makoto-repos-vantage-point"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn test_truncate_newlines() {
        assert_eq!(truncate("hello\nworld", 20), "helloworld");
    }
}
