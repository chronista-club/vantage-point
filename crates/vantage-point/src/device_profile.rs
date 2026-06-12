//! 物理 controller への state projection を device 非依存に抽象化する trait。
//!
//! 設計 SSOT: `docs/design/20-roto-control-sysex-protocol.md` §8（E1）。
//! ROTO の protocol 解読（doc 20）で確定した知見 —「色は state 同梱」
//! 「parameter は learn という rich な記述単位で teach する」— を踏まえ、
//! 単発の `set_lcd` / `set_color` ではなく **state projection 粒度**で抽象化する。
//!
//! 責務分離（data / calculations / actions）:
//! - data: `Rgb` / `ParamSpec`（論理 lane state の断片）
//! - calculations: `DeviceProfile` の各メソッド（state → MIDI byte 列。純粋関数、I/O なし）
//! - actions: 呼び出し側が `midi::send_sysex` で送出（trait は送信を持たない）
//!
//! impl 順は確度順: X-Touch（Ardour MCU 実装由来）→ LPD8（`midi::lpd8` 移行）→ ROTO。

/// RGB 色（各 0–255）。device 側の表現（パレット index / 固定 7 色など）への
/// 量子化は各 impl の責務。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// knob / fader に teach する parameter の記述単位（doc 20 §5 の learn model）。
///
/// 「値を 1 つ送る」のではなく「パラメータの意味（名前・型・detent・段階・現在値）を
/// 一括 projection する」ための器。ROTO 以外の device では表現できる範囲だけ使う
/// （例: X-Touch は name → LCD、value → モーターフェーダー / V-Pot ring）。
#[derive(Debug, Clone)]
pub struct ParamSpec {
    /// 表示名（device の文字数制限・ASCII 化は impl 側で行う）
    pub name: String,
    /// 正規化済み現在値（0.0–1.0）
    pub value: f32,
    /// 中央 detent（双極パラメータの 0 位置でクリック感）
    pub center_detent: bool,
    /// 段階数（0 = 連続）
    pub steps: u8,
    /// steps > 0 のときの各ステップのラベル（不足分は impl 側で空欄扱い）
    pub step_names: Vec<String>,
    /// macro parameter フラグ（ROTO 固有。他 device は無視してよい）
    pub is_macro: bool,
}

impl ParamSpec {
    /// 連続値パラメータの最小構成
    pub fn continuous(name: impl Into<String>, value: f32) -> Self {
        Self {
            name: name.into(),
            value,
            center_detent: false,
            steps: 0,
            step_names: Vec::new(),
            is_macro: false,
        }
    }
}

/// 物理 controller 1 機種ぶんの「論理 state → MIDI byte 列」変換。
///
/// 各メソッドは送信すべき MIDI メッセージのバッチ（`Vec<Vec<u8>>`）を返す。
/// 1 projection が複数メッセージになる device（X-Touch: LCD + 色 + フェーダー）と
/// 単一 SysEx の device（ROTO）を同じ形で扱うため、戻り値は常にバッチ。
/// SysEx に限らず channel message（pitch bend / CC / note）も同じ枠で返す。
///
/// projection 系が `&mut self` なのは、全 strip 一括 command しか持たない device
/// （X-Touch の LCD 1 行書き込み・色 8 byte）に対応するため。profile は device 側
/// 表示の shadow state を保持し、1 slot の更新でも全体メッセージを再構成できる。
pub trait DeviceProfile {
    /// MIDI port 照合用のパターン（`midi::send_sysex` の `port_pattern` に渡す）
    fn port_pattern(&self) -> &str;

    /// 接続開始時に送る初期化メッセージ列（doc 20 §6）。
    /// ack 待ちが必要な device の状態管理は呼び出し側 flow の責務。
    fn handshake(&self) -> Vec<Vec<u8>>;

    /// track / slot 単位の state projection（名前 + 色 + group flag を一括、doc 20 §4）
    fn project_track(&mut self, index: u8, name: &str, color: Rgb, is_group: bool) -> Vec<Vec<u8>>;

    /// knob / fader への parameter teach（doc 20 §5）
    fn learn_parameter(&mut self, index: u8, spec: &ParamSpec) -> Vec<Vec<u8>>;
}

pub mod xtouch {
    //! Behringer X-Touch（MCU mode、device ID `0x14`）の `DeviceProfile` impl。
    //!
    //! byte 仕様の出典は Ardour `libs/surfaces/mackie/`（production 実装）:
    //! - `surface.cc`: SysEx ヘッダ / LCD `0x12` / X-Touch 色 `0x72` / wake-up
    //! - `fader.cc`: モーターフェーダー = pitch bend 14bit
    //! - `pot.cc` / `pot.h`: V-Pot ring = CC `0x30+n`、value は mode/position の bit 合成
    //!
    //! X-Touch は Logic Control 型の challenge/response handshake が不要
    //! （Ardour は device ready byte `0x06` 受信で即 `turn_it_on()`）。
    //! wake-up 送出後すぐ projection を始めてよい。

    use super::{DeviceProfile, ParamSpec, Rgb};

    /// MCU SysEx ヘッダ（manufacturer `00 00 66` Mackie + device ID `0x14` = MCU/X-Touch）
    const MCU_HDR: [u8; 5] = [0xF0, 0x00, 0x00, 0x66, 0x14];
    /// SysEx 終端
    const EOX: u8 = 0xF7;
    /// LCD（scribble strip）行書き込み command
    const CMD_LCD: u8 = 0x12;
    /// X-Touch 固有: scribble strip 8 本の色を一括設定する command
    const CMD_STRIP_COLORS: u8 = 0x72;
    /// LCD 下段の先頭 offset（上段 = 0x00、下段 = 0x38 = 56）
    const LINE2_OFFSET: u8 = 0x38;
    /// channel strip 数
    const STRIPS: usize = 8;
    /// 1 strip の LCD 文字数
    const CHARS_PER_STRIP: usize = 7;
    /// 1 行書き込みの文字数（Ardour `Surface::display_line` 準拠。
    /// 8 strip × 7 = 56 のところ 55 文字 + padding で送るのが実機検証済みの形）
    const LINE_LEN: usize = 55;

    /// scribble strip の固定 8 色（`surface.h` `XTouchColors` enum 順）の RGB 代表値。
    /// index がそのまま `0x72` payload の色値（0=Off 1=Red 2=Green 3=Yellow
    /// 4=Blue 5=Purple 6=Cyan 7=White）。
    const STRIP_COLORS: [u32; 8] = [
        0x000000, 0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];

    /// V-Pot LED ring の表示モード（`pot.h` `Mode` enum）。
    /// value byte の bit4-5 に `mode << 4` で乗る。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum RingMode {
        /// 単点表示
        Dot = 0,
        /// 中央からの増減表示（双極パラメータ向け）
        BoostCut = 1,
        /// 連続量の塗り表示
        Wrap = 2,
        /// 中央から両側へ広がる表示
        Spread = 3,
    }

    /// RGB を strip 8 色へ最近傍量子化（`roto_palette::closest_index` と同じ二乗距離）
    fn closest_strip_color(color: Rgb) -> u8 {
        let mut best_index = 0usize;
        let mut best_dist = i64::MAX;
        for (index, &candidate) in STRIP_COLORS.iter().enumerate() {
            let cr = ((candidate >> 16) & 0xFF) as i64;
            let cg = ((candidate >> 8) & 0xFF) as i64;
            let cb = (candidate & 0xFF) as i64;
            let dist = (color.r as i64 - cr).pow(2)
                + (color.g as i64 - cg).pow(2)
                + (color.b as i64 - cb).pow(2);
            if dist < best_dist {
                best_dist = dist;
                best_index = index;
            }
        }
        best_index as u8
    }

    /// 表示名を 1 strip ぶん（7 文字）の ASCII byte 列に整形。
    /// 非 ASCII / 制御文字は `_` 置換（Ardour の ISO-8859-1 fallback と同方針）、
    /// 超過は切り詰め、不足は空白 padding。
    fn strip_cell(name: &str) -> [u8; CHARS_PER_STRIP] {
        let mut cell = [b' '; CHARS_PER_STRIP];
        for (slot, ch) in cell.iter_mut().zip(name.chars()) {
            *slot = if ch.is_ascii() && !ch.is_ascii_control() {
                ch as u8
            } else {
                b'_'
            };
        }
        cell
    }

    /// LCD 1 行ぶんの SysEx を組む（`F0 00 00 66 14 12 <offset> <55 chars> F7`）
    fn lcd_line(offset: u8, cells: &[String; STRIPS]) -> Vec<u8> {
        let mut msg = Vec::with_capacity(MCU_HDR.len() + 2 + LINE_LEN + 1);
        msg.extend_from_slice(&MCU_HDR);
        msg.push(CMD_LCD);
        msg.push(offset);
        let mut text = Vec::with_capacity(STRIPS * CHARS_PER_STRIP);
        for cell in cells {
            text.extend_from_slice(&strip_cell(cell));
        }
        // 8 strip 目の 7 文字目（56 文字目）は Ardour 準拠で送出しない（LINE_LEN = 55）
        text.truncate(LINE_LEN);
        msg.extend_from_slice(&text);
        msg.push(EOX);
        msg
    }

    /// モーターフェーダー位置（`fader.cc` 準拠）。
    /// pitch bend `E0+ch` に 14bit 値（`round(16383 × normalized)`）を LSB/MSB 順で。
    fn fader_position(channel: u8, normalized: f32) -> Vec<u8> {
        let value = (16383.0 * normalized.clamp(0.0, 1.0)).round() as u16;
        vec![
            0xE0 | (channel & 0x0F),
            (value & 0x7F) as u8,
            ((value >> 7) & 0x7F) as u8,
        ]
    }

    /// V-Pot ring 表示（`pot.cc` 準拠）。CC `0x30+index`、
    /// value = `(mode << 4) | position`（position 1–11）+ bit6 = 中央 LED。
    fn vpot_ring(index: u8, mode: RingMode, normalized: f32) -> Vec<u8> {
        let normalized = normalized.clamp(0.0, 1.0);
        let position = ((normalized * 10.0).round() as u8 + 1).min(11);
        let mut value = ((mode as u8) << 4) | (position & 0x0F);
        // 中央付近で中央 LED を点灯（Ardour: val が 0.48..0.58 のとき bit6）
        if (0.48..0.58).contains(&normalized) {
            value |= 1 << 6;
        }
        vec![0xB0, 0x30 + index, value]
    }

    /// X-Touch profile 本体。
    ///
    /// LCD 行・色は全 strip 一括 command のため、device 表示の shadow state
    /// （上段 = track 名、下段 = parameter 名、色 8 本）を保持する。
    pub struct XTouchProfile {
        /// LCD 上段（track 名）の shadow
        strip_names: [String; STRIPS],
        /// LCD 下段（parameter 名）の shadow
        param_names: [String; STRIPS],
        /// strip 色（0–7）の shadow
        strip_colors: [u8; STRIPS],
    }

    impl Default for XTouchProfile {
        fn default() -> Self {
            Self {
                strip_names: std::array::from_fn(|_| String::new()),
                param_names: std::array::from_fn(|_| String::new()),
                strip_colors: [0; STRIPS],
            }
        }
    }

    impl DeviceProfile for XTouchProfile {
        fn port_pattern(&self) -> &str {
            "X-Touch"
        }

        /// MCU wake-up / device query。ack（device ready）を待たず projection
        /// を始めてよい（X-Touch は handshake 不要、module doc 参照）。
        fn handshake(&self) -> Vec<Vec<u8>> {
            vec![vec![0xF0, 0x00, 0x00, 0x66, 0x14, 0x00, EOX]]
        }

        /// track 名 → LCD 上段、色 → strip 8 色一括 SysEx。
        /// `is_group` は X-Touch に対応表現がないため無視。
        fn project_track(
            &mut self,
            index: u8,
            name: &str,
            color: Rgb,
            _is_group: bool,
        ) -> Vec<Vec<u8>> {
            let Some(slot) = self.strip_names.get_mut(index as usize) else {
                return Vec::new(); // strip 範囲外は no-op
            };
            *slot = name.to_string();
            self.strip_colors[index as usize] = closest_strip_color(color);

            let mut colors_msg = Vec::with_capacity(MCU_HDR.len() + 1 + STRIPS + 1);
            colors_msg.extend_from_slice(&MCU_HDR);
            colors_msg.push(CMD_STRIP_COLORS);
            colors_msg.extend_from_slice(&self.strip_colors);
            colors_msg.push(EOX);

            vec![lcd_line(0x00, &self.strip_names), colors_msg]
        }

        /// parameter 名 → LCD 下段、現在値 → モーターフェーダー + V-Pot ring。
        /// steps / step_names は X-Touch に表現がないため ring mode の選択にのみ反映。
        fn learn_parameter(&mut self, index: u8, spec: &ParamSpec) -> Vec<Vec<u8>> {
            let Some(slot) = self.param_names.get_mut(index as usize) else {
                return Vec::new(); // strip 範囲外は no-op
            };
            *slot = spec.name.clone();

            let mode = if spec.center_detent {
                RingMode::BoostCut
            } else {
                RingMode::Wrap
            };

            vec![
                lcd_line(LINE2_OFFSET, &self.param_names),
                fader_position(index, spec.value),
                vpot_ring(index, mode, spec.value),
            ]
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn handshake_is_mcu_wakeup() {
            let profile = XTouchProfile::default();
            assert_eq!(
                profile.handshake(),
                vec![vec![0xF0, 0x00, 0x00, 0x66, 0x14, 0x00, 0xF7]]
            );
        }

        #[test]
        fn project_track_emits_lcd_line_and_colors() {
            let mut profile = XTouchProfile::default();
            let messages = profile.project_track(0, "Lane A", Rgb::new(255, 0, 0), false);
            assert_eq!(messages.len(), 2);

            // LCD 上段: ヘッダ + 0x12 + offset 0x00 + 55 文字 + F7
            let lcd = &messages[0];
            assert_eq!(&lcd[..7], &[0xF0, 0x00, 0x00, 0x66, 0x14, 0x12, 0x00]);
            assert_eq!(lcd.len(), 5 + 2 + 55 + 1);
            assert_eq!(&lcd[7..14], b"Lane A ");
            assert_eq!(*lcd.last().unwrap(), 0xF7);

            // 色: ヘッダ + 0x72 + 8 色 + F7。strip 0 = Red(1)、残りは Off(0)
            let colors = &messages[1];
            assert_eq!(
                colors.as_slice(),
                &[
                    0xF0, 0x00, 0x00, 0x66, 0x14, 0x72, 1, 0, 0, 0, 0, 0, 0, 0, 0xF7
                ]
            );
        }

        #[test]
        fn strip_color_quantizes_to_fixed_eight() {
            assert_eq!(closest_strip_color(Rgb::new(0, 0, 0)), 0); // Off
            assert_eq!(closest_strip_color(Rgb::new(255, 0, 0)), 1); // Red
            assert_eq!(closest_strip_color(Rgb::new(250, 250, 250)), 7); // White
            assert_eq!(closest_strip_color(Rgb::new(0, 200, 255)), 6); // Cyan 寄り
        }

        #[test]
        fn learn_parameter_emits_lcd_fader_and_ring() {
            let mut profile = XTouchProfile::default();
            let spec = ParamSpec::continuous("Cutoff", 1.0);
            let messages = profile.learn_parameter(2, &spec);
            assert_eq!(messages.len(), 3);

            // LCD 下段 offset = 0x38、strip 2 の位置（7 + 2×7 = 21 文字目）に名前
            let lcd = &messages[0];
            assert_eq!(lcd[6], 0x38);
            assert_eq!(&lcd[7 + 14..7 + 21], b"Cutoff ");

            // フェーダー: ch 2、フルスケール 16383 = LSB 0x7F / MSB 0x7F
            assert_eq!(messages[1].as_slice(), &[0xE2, 0x7F, 0x7F]);

            // V-Pot ring: CC 0x32、Wrap(2)<<4 | position 11
            assert_eq!(messages[2].as_slice(), &[0xB0, 0x32, 0x2B]);
        }

        #[test]
        fn vpot_center_detent_uses_boost_cut_with_center_led() {
            let mut profile = XTouchProfile::default();
            let mut spec = ParamSpec::continuous("Pan", 0.5);
            spec.center_detent = true;
            let messages = profile.learn_parameter(0, &spec);
            // BoostCut(1)<<4 | position(0.5→6) | 中央 LED bit6
            assert_eq!(messages[2].as_slice(), &[0xB0, 0x30, 0x56]);
        }

        #[test]
        fn non_ascii_name_is_sanitized() {
            let mut profile = XTouchProfile::default();
            let messages = profile.project_track(0, "あbc", Rgb::new(0, 0, 0), false);
            // 非 ASCII は '_' 置換（Ardour の fallback と同方針）
            assert_eq!(&messages[0][7..14], b"_bc    ");
        }

        #[test]
        fn out_of_range_strip_is_noop() {
            let mut profile = XTouchProfile::default();
            assert!(
                profile
                    .project_track(8, "X", Rgb::new(0, 0, 0), false)
                    .is_empty()
            );
        }
    }
}
