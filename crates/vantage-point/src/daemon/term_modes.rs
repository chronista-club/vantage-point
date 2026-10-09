//! replay が端末の**入力側**を汚さないための 2 道具（純関数 + 小さな追跡器）。
//!
//! ## 何が起きていたか（2026-10-09、chat → console 遷移で実測）
//!
//! PtySlot の replay ring buffer には claude TUI が出した **端末への問い合わせ**（DA1 `ESC[c` 等）と
//! **private mode の設定**（focus 通知 `?1004h` / マウス `?1000-1006h`）がそのまま残る
//! （実機 256 KB file に DA1 2〜3 件、mouse 45〜51 件）。console に戻る時に xterm.js がそれを
//! 再生すると:
//!
//! - DA1 に xterm が**応答**し（`ESC[?1;2c`）、それが pty の入力 = shell の行に打たれる
//! - focus / mouse の mode が ON になり、以後 focus のたび `ESC[I`、マウス移動のたび
//!   `ESC[<35;146;5M` が shell に打たれる
//!
//! codex TUI はこれらを使わないので出ない。shell の行に `^[[?1;2c^[[I^[[<35;…M` が並ぶ
//! スクリーンショットがそのままこの 3 種だった。
//!
//! ## 直し方
//!
//! 1. [`strip_queries`]: replay snapshot から「応答を要求する」sequence を落とす（DA / DSR /
//!    XTVERSION / DECRQM / kitty keyboard / window size / OSC color query / DCS DECRQSS）。
//!    live stream は触らない — live の program が問い合わせたなら応答が要る。
//! 2. [`TermModeTracker`]: reader が通す出力から private mode の ON/OFF を追跡し、replay の
//!    末尾で**今の本当の状態**に合わせる sequence（[`TermModeTracker::replay_suffix`]）を足す。
//!    live の claude が mouse ON なら ON のまま、shell が live なら OFF に戻る。無条件に
//!    OFF にしないのは、TUI が生きている pty に attach し直す経路（GUI 再起動）で mouse を
//!    奪わないため。
//!
//! 追跡するのは**入力の形を変える** mode だけ（mouse 系 / focus / bracketed paste）。画面の
//! 見た目の mode（alt screen 等）は replay の clear prefix と後続の再描画に任せる。

use std::collections::BTreeSet;

/// 追跡する DEC private mode。入力（pty への書き込み）の形を変えるものに限る。
///
/// - 1000 / 1002 / 1003: マウス報告（click / drag / motion）
/// - 1004: focus in/out 通知（`ESC[I` / `ESC[O`）
/// - 1005 / 1006 / 1015 / 1016: マウス報告の encoding（UTF-8 / SGR / urxvt / SGR-pixel）
/// - 2004: bracketed paste（貼り付けを `ESC[200~ … ESC[201~` で包む）
pub const TRACKED_MODES: &[u16] = &[1000, 1002, 1003, 1004, 1005, 1006, 1015, 1016, 2004];

/// 不完全な ESC sequence を次 chunk へ持ち越す上限。CSI は実用上これより短い。
const CARRY_MAX: usize = 32;

/// 出力 stream から private mode の ON/OFF を追う。chunk 境界で切れた sequence は持ち越す。
#[derive(Debug, Default, Clone)]
pub struct TermModeTracker {
    on: BTreeSet<u16>,
    carry: Vec<u8>,
}

impl TermModeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 出力 chunk を観測して mode 集合を更新する（`CSI ? Pm h` / `CSI ? Pm l` だけを見る）。
    pub fn observe(&mut self, chunk: &[u8]) {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            match parse_escape(&data[i..]) {
                Parsed::Complete(len, Some(seq)) => {
                    if let Seq::Csi {
                        private: Some(b'?'),
                        params,
                        intermediates,
                        final_byte,
                    } = &seq
                        && intermediates.is_empty()
                        && matches!(final_byte, b'h' | b'l')
                    {
                        for p in params {
                            if TRACKED_MODES.contains(p) {
                                if *final_byte == b'h' {
                                    self.on.insert(*p);
                                } else {
                                    self.on.remove(p);
                                }
                            }
                        }
                    }
                    i += len;
                }
                Parsed::Complete(len, None) => i += len,
                Parsed::Incomplete => {
                    // 末尾の不完全 sequence を持ち越す（長すぎるものは sequence ではないとみなして捨てる）
                    let tail = &data[i..];
                    self.carry = if tail.len() <= CARRY_MAX {
                        tail.to_vec()
                    } else {
                        Vec::new()
                    };
                    return;
                }
            }
        }
    }

    /// 今 ON の mode（テスト / 診断用）。
    pub fn enabled(&self) -> impl Iterator<Item = u16> + '_ {
        self.on.iter().copied()
    }

    /// replay の末尾に足して、xterm の mode を「今の本当の状態」に合わせる sequence。
    /// 追跡対象の全 mode について ON なら `h`、OFF なら `l` を明示する。
    pub fn replay_suffix(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRACKED_MODES.len() * 10);
        for m in TRACKED_MODES {
            let state = if self.on.contains(m) { 'h' } else { 'l' };
            out.extend_from_slice(format!("\x1b[?{m}{state}").as_bytes());
        }
        out
    }
}

/// replay snapshot から、端末に**応答を要求する** sequence を落とす。
///
/// 落とすもの: DA1/2/3（`CSI c` / `CSI > c` / `CSI = c`）、DSR（`CSI 5n` / `CSI 6n` / `CSI ? 6n`）、
/// XTVERSION（`CSI > q`）、DECRQM（`CSI ? Pm $ p`）、kitty keyboard query（`CSI ? u`）、
/// window 問い合わせ（`CSI 14/16/18/19/20/21 t`）、DECID（`ESC Z`）、OSC の色問い合わせ
/// （`OSC 10..19 ; ? ST`）、DCS の DECRQSS / XTGETTCAP（`DCS $ q …` / `DCS + q …`）。
/// それ以外（描画・mode 設定・応答そのもの）は無傷で通す。不完全な末尾はそのまま残す。
pub fn strip_queries(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b {
            out.push(data[i]);
            i += 1;
            continue;
        }
        match parse_escape(&data[i..]) {
            Parsed::Complete(len, Some(seq)) => {
                if !seq.is_query() {
                    out.extend_from_slice(&data[i..i + len]);
                }
                i += len;
            }
            Parsed::Complete(len, None) => {
                out.extend_from_slice(&data[i..i + len]);
                i += len;
            }
            Parsed::Incomplete => {
                out.extend_from_slice(&data[i..]);
                break;
            }
        }
    }
    out
}

/// 解釈した escape sequence（必要な種類だけ）。
#[derive(Debug, PartialEq, Eq)]
enum Seq {
    Csi {
        private: Option<u8>,
        params: Vec<u16>,
        intermediates: Vec<u8>,
        final_byte: u8,
    },
    /// OSC 本文（`ESC ]` と終端を除いた中身）
    Osc(Vec<u8>),
    /// DCS 本文（`ESC P` と終端を除いた中身）
    Dcs(Vec<u8>),
    /// `ESC Z`（DECID、DA1 の旧形）
    DecId,
}

impl Seq {
    fn is_query(&self) -> bool {
        match self {
            Seq::Csi {
                private,
                params,
                intermediates,
                final_byte,
            } => match (final_byte, private, intermediates.as_slice()) {
                // DA1 / DA2 / DA3（`CSI c` / `CSI 0 c` / `CSI > c` / `CSI = c`）
                (b'c', None | Some(b'>') | Some(b'='), []) => true,
                // DSR（`CSI 5 n` / `CSI 6 n` / `CSI ? 6 n` 等）
                (b'n', None | Some(b'?'), []) => true,
                // XTVERSION（`CSI > q`）
                (b'q', Some(b'>'), []) => true,
                // DECRQM（`CSI ? Pm $ p`）
                (b'p', Some(b'?'), [b'$']) => true,
                // kitty keyboard protocol query（`CSI ? u`）
                (b'u', Some(b'?'), []) => true,
                // window 問い合わせ（text area size / size in chars / title 等）
                (b't', None, []) => matches!(params.first(), Some(14 | 16 | 18 | 19 | 20 | 21)),
                _ => false,
            },
            // OSC 10..19 の `;?`（前景 / 背景 / cursor 色の問い合わせ）
            Seq::Osc(body) => {
                let s = String::from_utf8_lossy(body);
                let mut it = s.splitn(2, ';');
                let num = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("");
                arg.starts_with('?')
                    && num
                        .parse::<u16>()
                        .map(|n| (10..=19).contains(&n))
                        .unwrap_or(false)
            }
            // DCS: DECRQSS（`$q`）/ XTGETTCAP（`+q`）
            Seq::Dcs(body) => body.starts_with(b"$q") || body.starts_with(b"+q"),
            Seq::DecId => true,
        }
    }
}

enum Parsed {
    /// (消費 byte 数, 解釈結果。None = 興味の無い sequence だが長さは確定)
    Complete(usize, Option<Seq>),
    /// data の末尾で切れている
    Incomplete,
}

/// `data[0] == ESC` 前提で 1 sequence を読む。
fn parse_escape(data: &[u8]) -> Parsed {
    debug_assert_eq!(data.first(), Some(&0x1b));
    let Some(&kind) = data.get(1) else {
        return Parsed::Incomplete;
    };
    match kind {
        b'[' => parse_csi(data),
        b']' => parse_string_seq(data).map_or(Parsed::Incomplete, |(len, body)| {
            Parsed::Complete(len, Some(Seq::Osc(body)))
        }),
        b'P' => parse_string_seq(data).map_or(Parsed::Incomplete, |(len, body)| {
            Parsed::Complete(len, Some(Seq::Dcs(body)))
        }),
        b'Z' => Parsed::Complete(2, Some(Seq::DecId)),
        // 他の 2 byte ESC sequence / charset 指定（`ESC ( B` 等は 3 byte）
        b'(' | b')' | b'*' | b'+' => {
            if data.len() >= 3 {
                Parsed::Complete(3, None)
            } else {
                Parsed::Incomplete
            }
        }
        _ => Parsed::Complete(2, None),
    }
}

/// `ESC [` 以降: private marker（`?` `>` `=` `<`）→ params（数字と `;`）→ intermediates
/// （0x20..=0x2F）→ final（0x40..=0x7E）。
fn parse_csi(data: &[u8]) -> Parsed {
    let mut i = 2;
    let mut private = None;
    if let Some(&b) = data.get(i)
        && matches!(b, b'?' | b'>' | b'=' | b'<')
    {
        private = Some(b);
        i += 1;
    }
    let mut params = Vec::new();
    let mut cur: Option<u16> = None;
    loop {
        let Some(&b) = data.get(i) else {
            return Parsed::Incomplete;
        };
        match b {
            b'0'..=b'9' => {
                let d = u16::from(b - b'0');
                cur = Some(cur.unwrap_or(0).saturating_mul(10).saturating_add(d));
                i += 1;
            }
            b';' | b':' => {
                params.push(cur.take().unwrap_or(0));
                i += 1;
            }
            0x20..=0x2F => break,
            0x40..=0x7E => break,
            // 範囲外（制御文字等）= CSI として壊れている。ESC 1 byte だけ消費して先へ
            _ => return Parsed::Complete(1, None),
        }
    }
    if let Some(c) = cur {
        params.push(c);
    }
    let mut intermediates = Vec::new();
    loop {
        let Some(&b) = data.get(i) else {
            return Parsed::Incomplete;
        };
        match b {
            0x20..=0x2F => {
                intermediates.push(b);
                i += 1;
            }
            0x40..=0x7E => {
                return Parsed::Complete(
                    i + 1,
                    Some(Seq::Csi {
                        private,
                        params,
                        intermediates,
                        final_byte: b,
                    }),
                );
            }
            _ => return Parsed::Complete(1, None),
        }
    }
}

/// OSC / DCS: `ESC ] body ST` / `ESC P body ST`。ST は BEL か `ESC \`。
/// 戻り値 = (消費 byte 数, body)。終端が無ければ None（= Incomplete）。
fn parse_string_seq(data: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut i = 2;
    while i < data.len() {
        match data[i] {
            0x07 => return Some((i + 1, data[2..i].to_vec())),
            0x1b if data.get(i + 1) == Some(&b'\\') => return Some((i + 2, data[2..i].to_vec())),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_every_query_kind_and_keeps_drawing() {
        let input = b"\x1b[31mred\x1b[0m\x1b[c\x1b[>c\x1b[=c\x1b[6n\x1b[?6n\x1b[5n\x1b[>q\x1b[?2004$p\x1b[?u\x1b[14t\x1b[18t\x1bZ\x1b]11;?\x07\x1b]10;?\x1b\\\x1bP$qm\x1b\\\x1bP+q544e\x1b\\tail";
        assert_eq!(strip_queries(input), b"\x1b[31mred\x1b[0mtail");
    }

    #[test]
    fn keeps_responses_mode_sets_and_non_query_window_ops() {
        // DA の応答そのもの（`?1;2c`）は private が `?` なので DA query とは別 = 残る。
        // mode 設定 / OSC title / `CSI 22 t`（title push）/ DCS の sixel 風は残る。
        let input =
            b"\x1b[?1;2c\x1b[?1004h\x1b[?1000l\x1b]0;title\x07\x1b[22;0t\x1bPq#0\x1b\\\x1b[2J";
        assert_eq!(strip_queries(input), input);
    }

    #[test]
    fn incomplete_tail_is_preserved() {
        let input = b"abc\x1b[?100";
        assert_eq!(strip_queries(input), input);
        let input = b"abc\x1b]11;?";
        assert_eq!(strip_queries(input), input);
    }

    #[test]
    fn tracker_follows_set_and_reset_including_multi_param() {
        let mut t = TermModeTracker::new();
        t.observe(b"\x1b[?1004h\x1b[?1000;1006h\x1b[?25l");
        assert_eq!(t.enabled().collect::<Vec<_>>(), vec![1000, 1004, 1006]);
        t.observe(b"\x1b[?1000l\x1b[?1004l");
        assert_eq!(t.enabled().collect::<Vec<_>>(), vec![1006]);
        // `?25l`（cursor）は追跡外なので集合に入らない
        assert!(!t.enabled().any(|m| m == 25));
    }

    #[test]
    fn tracker_carries_sequence_split_across_chunks() {
        let mut t = TermModeTracker::new();
        t.observe(b"text\x1b[?10");
        assert!(t.enabled().next().is_none(), "途中では確定しない");
        t.observe(b"04h more");
        assert_eq!(t.enabled().collect::<Vec<_>>(), vec![1004]);
    }

    #[test]
    fn replay_suffix_states_every_tracked_mode_explicitly() {
        let mut t = TermModeTracker::new();
        let all_off = t.replay_suffix();
        for m in TRACKED_MODES {
            assert!(
                all_off
                    .windows(format!("\x1b[?{m}l").len())
                    .any(|w| w == format!("\x1b[?{m}l").as_bytes()),
                "{m} は OFF を明示"
            );
        }
        t.observe(b"\x1b[?1003h\x1b[?1006h");
        let s = t.replay_suffix();
        let s = String::from_utf8(s).unwrap();
        assert!(s.contains("\x1b[?1003h") && s.contains("\x1b[?1006h"));
        assert!(s.contains("\x1b[?1000l") && s.contains("\x1b[?1004l"));
    }
}
