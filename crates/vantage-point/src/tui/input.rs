//! crossterm KeyEvent → PTY バイト列変換
//!
//! ratatui/crossterm のキーイベントを PTY に送るバイト列に変換する。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// キーイベントを PTY バイト列に変換
///
/// `app_cursor` が true の場合、矢印キーが `\x1bOA` 形式になる（DECCKM）。
pub fn key_to_pty_bytes(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let mods = key.modifiers;

    match key.code {
        // Ctrl+key
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
            if let Some(b) = ctrl_key_byte(c) {
                vec![b]
            } else {
                vec![]
            }
        }

        // Alt+key
        KeyCode::Char(c) if mods.contains(KeyModifiers::ALT) => {
            let mut bytes = vec![0x1b];
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            bytes
        }

        // 通常文字
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }

        // Enter
        KeyCode::Enter => vec![b'\r'],

        // Backspace
        KeyCode::Backspace => vec![0x7f],

        // Tab
        KeyCode::Tab => vec![b'\t'],

        // Shift+Tab (backtab)
        KeyCode::BackTab => b"\x1b[Z".to_vec(),

        // Escape
        KeyCode::Esc => vec![0x1b],

        // 矢印キー（DECCKM 対応）
        KeyCode::Up => arrow_key(b'A', app_cursor),
        KeyCode::Down => arrow_key(b'B', app_cursor),
        KeyCode::Right => arrow_key(b'C', app_cursor),
        KeyCode::Left => arrow_key(b'D', app_cursor),

        // Home / End
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),

        // Page Up / Down
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),

        // Insert / Delete
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),

        // F1-F12
        KeyCode::F(n) => f_key(n),

        _ => vec![],
    }
}

/// Ctrl+key → 制御コード
fn ctrl_key_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        'a'..='z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
        '[' | '3' => Some(0x1b), // Ctrl+[ = ESC
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        ' ' | '2' => Some(0x00), // Ctrl+Space = NUL
        _ => None,
    }
}

/// 矢印キーの DECCKM 対応
fn arrow_key(suffix: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', suffix]
    } else {
        vec![0x1b, b'[', suffix]
    }
}

/// F1-F12 エスケープシーケンス
fn f_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctrl_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_to_pty_bytes(key, false), vec![3]); // ETX
    }

    #[test]
    fn test_arrow_keys_normal() {
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key_to_pty_bytes(key, false), b"\x1b[A");
    }

    #[test]
    fn test_arrow_keys_app_cursor() {
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key_to_pty_bytes(key, true), b"\x1bOA");
    }

    #[test]
    fn test_enter() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(key_to_pty_bytes(key, false), vec![b'\r']);
    }

    #[test]
    fn test_unicode_char() {
        let key = KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE);
        assert_eq!(key_to_pty_bytes(key, false), "あ".as_bytes().to_vec());
    }
}
