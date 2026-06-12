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
//! - actions: 呼び出し側が `midi::send_batch` で送出（trait は送信を持たない）
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
    /// 安定 ID（lane の slot path 等）。ROTO の hash6（doc 20 §3）の入力。
    /// None なら name を ID として使う
    pub id: Option<String>,
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
            id: None,
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
    /// MIDI port 照合用のパターン（`midi::send_batch` の `port_pattern` に渡す）
    fn port_pattern(&self) -> &str;

    /// 接続開始時に送る初期化メッセージ列（doc 20 §6）。
    /// ack 待ちが必要な device の状態管理は呼び出し側 flow の責務。
    fn handshake(&self) -> Vec<Vec<u8>>;

    /// track / slot 単位の state projection（名前 + 色 + group flag を一括、doc 20 §4）
    fn project_track(&mut self, index: u8, name: &str, color: Rgb, is_group: bool) -> Vec<Vec<u8>>;

    /// knob / fader への parameter teach（doc 20 §5）
    fn learn_parameter(&mut self, index: u8, spec: &ParamSpec) -> Vec<Vec<u8>>;
}

/// RGB を固定パレットへ最近傍量子化（二乗距離。`roto_palette::closest_index` と同方式）。
/// パレット色しか持たない device（X-Touch 8 色 / ROTO 83 色）で共用する。
fn closest_palette_index(palette: &[u32], color: Rgb) -> u8 {
    let mut best_index = 0usize;
    let mut best_dist = i64::MAX;
    for (index, &candidate) in palette.iter().enumerate() {
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
    /// 1 行書き込みの文字数（8 strip × 7 = 56）。
    /// Ardour は 55 文字送り（strip 8 の 7 文字目が欠ける。X-Touch 実機で
    /// `Param 8` → `Param ` になるのを確認）だが、X-Touch は 56 文字を受けるため全送する。
    const LINE_LEN: usize = 56;

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

    /// RGB を strip 8 色へ最近傍量子化
    fn closest_strip_color(color: Rgb) -> u8 {
        super::closest_palette_index(&STRIP_COLORS, color)
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

    /// LCD 1 行ぶんの SysEx を組む（`F0 00 00 66 14 12 <offset> <56 chars> F7`）
    fn lcd_line(offset: u8, cells: &[String; STRIPS]) -> Vec<u8> {
        let mut msg = Vec::with_capacity(MCU_HDR.len() + 2 + LINE_LEN + 1);
        msg.extend_from_slice(&MCU_HDR);
        msg.push(CMD_LCD);
        msg.push(offset);
        let mut text = Vec::with_capacity(STRIPS * CHARS_PER_STRIP);
        for cell in cells {
            text.extend_from_slice(&strip_cell(cell));
        }
        text.truncate(LINE_LEN);
        msg.extend_from_slice(&text);
        msg.push(EOX);
        msg
    }

    /// モーターフェーダー位置（`fader.cc` 準拠）。
    /// pitch bend `E0+ch` に 14bit 値（`round(16383 × normalized)`）を LSB/MSB 順で。
    /// 連続駆動（wave 等）の caller 向けに単体公開。
    pub fn fader_position(channel: u8, normalized: f32) -> Vec<u8> {
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

            // LCD 上段: ヘッダ + 0x12 + offset 0x00 + 56 文字 + F7
            let lcd = &messages[0];
            assert_eq!(&lcd[..7], &[0xF0, 0x00, 0x00, 0x66, 0x14, 0x12, 0x00]);
            assert_eq!(lcd.len(), 5 + 2 + 56 + 1);
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

pub mod lpd8 {
    //! AKAI LPD8 mk2 の `DeviceProfile` impl（E1 第二号）。
    //!
    //! protocol 出典: Wireshark 実機キャプチャ由来のリバースエンジニアリング 3 repo
    //! （stephensrmmartin/lpd8mk2・john-kuan/lpd8mk2sysex・john-kuan/lpd8mk2-traktor）が
    //! 独立一致。mk2 の model ID は `0x4C`（既存 `midi::lpd8` の `0x75` は初代 LPD8 で別物）。
    //!
    //! pad LED は program 設定とは独立した専用 command で、**フル RGB（各色 0–255）**を
    //! 7bit×2 分割で送る。4 色インデックスではないため、lane 色を量子化なしで投影できる
    //! 艦隊唯一の pad。色 command は 8 pad 一括のため shadow state を保持する（X-Touch 同型）。
    //!
    //! program 構造（note/CC 割当、128 byte entry 型）は flag bit が未確定のため未実装
    //! （input flow 実装時に config.py / hex_diagram.svg を pin して対応）。

    use super::{DeviceProfile, ParamSpec, Rgb};

    /// mk2 SysEx ヘッダ（manufacturer `47` Akai / device `7F` broadcast / model `4C` = LPD8 mk2）
    const MK2_HDR: [u8; 4] = [0xF0, 0x47, 0x7F, 0x4C];
    /// SysEx 終端
    const EOX: u8 = 0xF7;
    /// pad LED 一括更新 command（`06` + sub-ID `00 30`、8 pad × RGB 各 2 byte）
    const CMD_PAD_LED: [u8; 3] = [0x06, 0x00, 0x30];
    /// pad 数
    const PADS: usize = 8;

    /// 8bit 値（0–255）を MIDI data byte 2 つ（7bit×2、MSB→LSB 順）に分割する
    fn pack7(value: u8) -> [u8; 2] {
        [value >> 7, value & 0x7F]
    }

    /// LPD8 mk2 profile 本体。LED command が 8 pad 一括のため色の shadow を保持する。
    pub struct Lpd8Profile {
        /// pad LED 色の shadow（index = pad 0–7）
        pad_colors: [Rgb; PADS],
    }

    impl Default for Lpd8Profile {
        fn default() -> Self {
            Self {
                pad_colors: [Rgb::new(0, 0, 0); PADS],
            }
        }
    }

    impl Lpd8Profile {
        /// 現在の shadow から pad LED 一括 SysEx を組む
        fn pad_led_sysex(&self) -> Vec<u8> {
            let mut msg = Vec::with_capacity(MK2_HDR.len() + CMD_PAD_LED.len() + PADS * 6 + 1);
            msg.extend_from_slice(&MK2_HDR);
            msg.extend_from_slice(&CMD_PAD_LED);
            for color in &self.pad_colors {
                msg.extend_from_slice(&pack7(color.r));
                msg.extend_from_slice(&pack7(color.g));
                msg.extend_from_slice(&pack7(color.b));
            }
            msg.push(EOX);
            msg
        }
    }

    impl DeviceProfile for Lpd8Profile {
        fn port_pattern(&self) -> &str {
            "LPD8"
        }

        /// mk2 に handshake 手順はない（LED command は前置きなしで効く）
        fn handshake(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }

        /// lane 色 → pad LED（フル RGB、量子化なし）。name / is_group は表示先がないため無視。
        fn project_track(
            &mut self,
            index: u8,
            _name: &str,
            color: Rgb,
            _is_group: bool,
        ) -> Vec<Vec<u8>> {
            let Some(slot) = self.pad_colors.get_mut(index as usize) else {
                return Vec::new(); // pad 範囲外は no-op
            };
            *slot = color;
            vec![self.pad_led_sysex()]
        }

        /// LPD8 に parameter の表示先がないため no-op
        /// （knob の CC 割当 = program 書き込みは input flow 実装時に対応）
        fn learn_parameter(&mut self, _index: u8, _spec: &ParamSpec) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn handshake_is_empty() {
            let profile = Lpd8Profile::default();
            assert!(profile.handshake().is_empty());
        }

        #[test]
        fn project_track_emits_full_rgb_led_sysex() {
            let mut profile = Lpd8Profile::default();
            let messages = profile.project_track(0, "Lane A", Rgb::new(255, 0, 128), false);
            assert_eq!(messages.len(), 1);

            let sysex = &messages[0];
            // ヘッダ + cmd: F0 47 7F 4C 06 00 30、全長 = 7 + 8×6 + 1 = 56
            assert_eq!(&sysex[..7], &[0xF0, 0x47, 0x7F, 0x4C, 0x06, 0x00, 0x30]);
            assert_eq!(sysex.len(), 7 + 48 + 1);
            assert_eq!(*sysex.last().unwrap(), 0xF7);
            // pad 0: R=255 → [01 7F]、G=0 → [00 00]、B=128 → [01 00]
            assert_eq!(&sysex[7..13], &[0x01, 0x7F, 0x00, 0x00, 0x01, 0x00]);
            // pad 1 以降は未設定 = 黒
            assert_eq!(&sysex[13..19], &[0, 0, 0, 0, 0, 0]);
        }

        #[test]
        fn shadow_keeps_previous_pad_colors() {
            let mut profile = Lpd8Profile::default();
            profile.project_track(0, "A", Rgb::new(255, 0, 0), false);
            let messages = profile.project_track(7, "B", Rgb::new(0, 0, 255), false);
            let sysex = &messages[0];
            // pad 0 の赤が保持されたまま pad 7 の青が乗る
            assert_eq!(&sysex[7..9], &[0x01, 0x7F]); // pad0 R=255
            assert_eq!(&sysex[7 + 7 * 6 + 4..7 + 7 * 6 + 6], &[0x01, 0x7F]); // pad7 B=255
        }

        #[test]
        fn pack7_splits_msb_lsb() {
            assert_eq!(pack7(0), [0x00, 0x00]);
            assert_eq!(pack7(127), [0x00, 0x7F]);
            assert_eq!(pack7(128), [0x01, 0x00]);
            assert_eq!(pack7(255), [0x01, 0x7F]);
        }

        #[test]
        fn out_of_range_pad_is_noop() {
            let mut profile = Lpd8Profile::default();
            assert!(
                profile
                    .project_track(8, "X", Rgb::new(0, 0, 0), false)
                    .is_empty()
            );
        }
    }
}

pub mod roto {
    //! Melbourne Instruments ROTO-CONTROL の `DeviceProfile` impl（E1 最終・第三号）。
    //!
    //! byte 仕様の SSOT は `docs/design/20-roto-control-sysex-protocol.md`
    //! （公式 Bitwig 拡張の full decompile で確定した grammar）。
    //! - フレーム: `F0 00 22 03 02 <type> <id> <payload> F7`（§1）
    //! - track 表示: `02 0A 07`（名前 + 色 + group を一括、§2/§4）
    //! - parameter learn: `02 0B 0A`（hash6 + detent + steps + 値 + 名前、§5）
    //! - handshake: `02 0A 01`（DAW_START、§6。ack 受信の state machine は呼び出し側）
    //!
    //! track / parameter とも per-slot command のため shadow state は不要
    //! （X-Touch / LPD8 の一括 command とは対照的）。

    use super::{DeviceProfile, ParamSpec, Rgb};
    use sha1::{Digest, Sha1};

    /// ROTO SysEx ヘッダ（manufacturer `00 22 03` Melbourne + prefix `02`、doc 20 §1）
    const ROTO_HDR: [u8; 5] = [0xF0, 0x00, 0x22, 0x03, 0x02];
    /// SysEx 終端
    const EOX: u8 = 0xF7;
    /// command type: GENERAL（DAW 制御・track・LCD）
    const TYPE_GENERAL: u8 = 0x0A;
    /// command type: PLUGIN（device/parameter）
    const TYPE_PLUGIN: u8 = 0x0B;
    /// GENERAL: handshake 開始（DAW_START）
    const CMD_DAW_START: u8 = 0x01;
    /// GENERAL: track 表示更新（名前+色+group flag）
    const CMD_TRACK_UPDATE: u8 = 0x07;
    /// PLUGIN: parameter learn（knob への teach）
    const CMD_PARAM_LEARN: u8 = 0x0A;
    /// name フィールドのスロット数（13、doc 20 §3）
    const NAME_SLOTS: usize = 13;

    /// `toAsciiDisplay` 等価（doc 20 §3）: 非 ASCII はラテン置換表で ASCII 化、
    /// 置換表にないものは drop。制御文字も drop。
    fn to_ascii_display(name: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(name.len());
        for ch in name.chars() {
            match ch {
                c if c.is_ascii() && !c.is_ascii_control() => out.push(c as u8),
                'ä' => out.push(b'a'),
                'ö' => out.push(b'o'),
                'ü' => out.push(b'u'),
                'Ä' => out.push(b'A'),
                'Ö' => out.push(b'O'),
                'Ü' => out.push(b'U'),
                'ß' => out.extend_from_slice(b"ss"),
                'é' | 'è' | 'ê' => out.push(b'e'),
                'â' | 'á' | 'à' => out.push(b'a'),
                'û' | 'ú' | 'ù' => out.push(b'u'),
                'ô' | 'ó' | 'ò' => out.push(b'o'),
                _ => {} // 置換表にない非 ASCII は drop（原実装準拠）
            }
        }
        out
    }

    /// `toSysExName` 等価（doc 20 §3）: display 用 13 文字、不足は 0x00 padding
    fn sysex_name13(name: &str) -> [u8; NAME_SLOTS] {
        let ascii = to_ascii_display(name);
        let mut slots = [0u8; NAME_SLOTS];
        for (slot, byte) in slots.iter_mut().zip(ascii.iter()) {
            *slot = *byte;
        }
        slots
    }

    /// `nameToSysEx` 等価（doc 20 §3）: state 用。12 文字に切り 13 スロット出力
    /// （末尾は必ず 0x00 終端）
    fn name12_nul(name: &str) -> [u8; NAME_SLOTS] {
        let ascii = to_ascii_display(name);
        let mut slots = [0u8; NAME_SLOTS];
        for (slot, byte) in slots.iter_mut().take(NAME_SLOTS - 1).zip(ascii.iter()) {
            *slot = *byte;
        }
        slots
    }

    /// `createHash` 等価（doc 20 §3）: SHA-1(id) の先頭 6 byte を `& 0x7F`
    /// （MIDI data byte 化）した parameter 識別子
    fn hash6(id: &str) -> [u8; 6] {
        let digest = Sha1::digest(id.as_bytes());
        let mut out = [0u8; 6];
        for (slot, byte) in out.iter_mut().zip(digest.iter()) {
            *slot = byte & 0x7F;
        }
        out
    }

    /// index を 7bit×2 に分割（doc 20 §1: `index>>7 & 0x7F`, `index & 0x7F`）
    fn split14(index: u16) -> [u8; 2] {
        [((index >> 7) & 0x7F) as u8, (index & 0x7F) as u8]
    }

    /// ping（`02 0A 03 02`、doc 20 §2）。ROTO は接続中 `02 0A 02`（hello/heartbeat）を
    /// 約 1 秒間隔で送ってくる（実機 probe で確認）。host はこれに ping で応える。
    pub fn ping() -> Vec<u8> {
        vec![
            ROTO_HDR[0],
            ROTO_HDR[1],
            ROTO_HDR[2],
            ROTO_HDR[3],
            ROTO_HDR[4],
            TYPE_GENERAL,
            0x03,
            0x02,
            EOX,
        ]
    }

    /// knob モーター位置の feedback（`RotoKnob`/`sendCCHiRes` 準拠）。
    /// 14bit hi-res CC × 2: `BF <12+index> <hi>` + `BF <44+index> <lo>`、
    /// `value = round(normalized × 16383)`。連続駆動（anim 等）の caller 向けに単体公開。
    pub fn knob_position(index: u8, normalized: f32) -> Vec<Vec<u8>> {
        let value = (16383.0 * normalized.clamp(0.0, 1.0)).round() as u16;
        vec![
            vec![0xBF, 12 + index, (value >> 7) as u8],
            vec![0xBF, 44 + index, (value & 0x7F) as u8],
        ]
    }

    /// track 1 slot ぶんの shadow（名前 + 量子化済み色 + group flag）
    #[derive(Clone)]
    struct TrackSlot {
        name: String,
        color_index: u8,
        is_group: bool,
    }

    impl Default for TrackSlot {
        fn default() -> Self {
            Self {
                name: String::new(),
                color_index: 70, // 未割当のデフォルト = 黒（doc 20 §4）
                is_group: false,
            }
        }
    }

    /// ROTO が扱う track slot 数（本体表示は 8 knob/LCD 単位）
    const TRACKS: usize = 8;

    /// ROTO profile 本体。
    ///
    /// track 表示は裸の `02 0A 07` 単発では反映されず、**枠付きバッチ**
    /// （track 数 `0A 04` → 先頭 offset `0A 05` → track 更新 × N → end detail `0A 08`）
    /// で送って初めて表示が確定する（`MixLayerSet.sendStates` 準拠、実機検証 2026-06-13）。
    /// このため track の shadow state を保持し、1 slot の更新でも完全バッチを返す。
    #[derive(Default)]
    pub struct RotoProfile {
        tracks: [TrackSlot; TRACKS],
        /// projection 済みの最大 index + 1（= device に通知する track 数）
        track_count: usize,
    }

    /// GENERAL の index 付き command（`02 0A <code> <hi> <lo>`、doc 20 §2）を組む
    fn general_index_command(code: u8, value: u16) -> Vec<u8> {
        let mut msg = Vec::with_capacity(ROTO_HDR.len() + 4);
        msg.extend_from_slice(&ROTO_HDR);
        msg.push(TYPE_GENERAL);
        msg.push(code);
        msg.extend_from_slice(&split14(value));
        msg.push(EOX);
        msg
    }

    impl RotoProfile {
        /// track 1 本ぶんの更新 SysEx（`02 0A 07`、doc 20 §2/§4）
        fn track_update(&self, index: usize) -> Vec<u8> {
            let slot = &self.tracks[index];
            let mut msg = Vec::with_capacity(ROTO_HDR.len() + 4 + NAME_SLOTS + 3);
            msg.extend_from_slice(&ROTO_HDR);
            msg.push(TYPE_GENERAL);
            msg.push(CMD_TRACK_UPDATE);
            msg.extend_from_slice(&split14(index as u16));
            msg.extend_from_slice(&name12_nul(&slot.name));
            msg.push(slot.color_index);
            msg.push(slot.is_group as u8);
            msg.push(EOX);
            msg
        }
    }

    impl DeviceProfile for RotoProfile {
        fn port_pattern(&self) -> &str {
            // 実機の CoreMIDI port 名は "Roto-Control"（部分一致は大文字小文字を区別する）
            "Roto"
        }

        /// DAW_START（doc 20 §6）。接続成立までの応答（hello への ping +
        /// meter threshold、`0A 0C` への `0A 0D`）は呼び出し側 flow の責務。
        fn handshake(&self) -> Vec<Vec<u8>> {
            vec![vec![
                ROTO_HDR[0],
                ROTO_HDR[1],
                ROTO_HDR[2],
                ROTO_HDR[3],
                ROTO_HDR[4],
                TYPE_GENERAL,
                CMD_DAW_START,
                EOX,
            ]]
        }

        /// track 表示更新（doc 20 §2 `02 0A 07` / §4）。
        /// 色は 83 色パレットへ最近傍量子化して colorIndex を state に同梱し、
        /// 全 track の枠付きバッチとして返す（module doc 参照）。
        fn project_track(
            &mut self,
            index: u8,
            name: &str,
            color: Rgb,
            is_group: bool,
        ) -> Vec<Vec<u8>> {
            let Some(slot) = self.tracks.get_mut(index as usize) else {
                return Vec::new(); // slot 範囲外は no-op
            };
            *slot = TrackSlot {
                name: name.to_string(),
                color_index: crate::roto_palette::closest_index(color.r, color.g, color.b),
                is_group,
            };
            self.track_count = self.track_count.max(index as usize + 1);

            let mut batch = Vec::with_capacity(self.track_count + 3);
            batch.push(general_index_command(0x04, self.track_count as u16)); // track 数
            batch.push(general_index_command(0x05, 0)); // 先頭 offset
            for i in 0..self.track_count {
                batch.push(self.track_update(i));
            }
            // end detail（doc 20 §2 `02 0A 08`）= 表示コミット
            batch.push(vec![
                ROTO_HDR[0],
                ROTO_HDR[1],
                ROTO_HDR[2],
                ROTO_HDR[3],
                ROTO_HDR[4],
                TYPE_GENERAL,
                0x08,
                EOX,
            ]);
            batch
        }

        /// parameter learn（doc 20 §5 `02 0B 0A`）。knob に「パラメータの意味」
        /// （識別 hash・型・detent・段階・現在値・名前）を一括 teach する。
        fn learn_parameter(&mut self, index: u8, spec: &ParamSpec) -> Vec<Vec<u8>> {
            let id = spec.id.as_deref().unwrap_or(&spec.name);
            let position = (16383.0 * spec.value.clamp(0.0, 1.0)).round() as u16;

            let mut msg = Vec::with_capacity(ROTO_HDR.len() + 14 + NAME_SLOTS + 1);
            msg.extend_from_slice(&ROTO_HDR);
            msg.push(TYPE_PLUGIN);
            msg.push(CMD_PARAM_LEARN);
            msg.extend_from_slice(&split14(index as u16));
            msg.extend_from_slice(&hash6(id));
            msg.push(spec.is_macro as u8);
            msg.push(spec.center_detent as u8);
            msg.push(spec.steps & 0x7F);
            msg.extend_from_slice(&split14(position));
            msg.extend_from_slice(&sysex_name13(&spec.name));
            // steps が 1–16 のとき各ステップのラベルを付与（不足分は空欄、doc 20 §5）
            if (1..=16).contains(&spec.steps) {
                for step in 0..spec.steps as usize {
                    let label = spec.step_names.get(step).map(String::as_str).unwrap_or("");
                    msg.extend_from_slice(&sysex_name13(label));
                }
            }
            msg.push(EOX);
            vec![msg]
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn handshake_is_daw_start() {
            let profile = RotoProfile::default();
            assert_eq!(
                profile.handshake(),
                vec![vec![0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x01, 0xF7]]
            );
        }

        #[test]
        fn project_track_emits_framed_batch() {
            let mut profile = RotoProfile::default();
            let messages = profile.project_track(1, "Lane A", Rgb::new(255, 0, 0), false);
            // track 数 + offset + track 0..=1 + end detail = 5（実機検証済みの枠付きバッチ）
            assert_eq!(messages.len(), 5);
            assert_eq!(
                messages[0].as_slice(),
                &[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x04, 0x00, 0x02, 0xF7]
            ); // track 数 = 2（projection 済み最大 index + 1）
            assert_eq!(
                messages[1].as_slice(),
                &[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x05, 0x00, 0x00, 0xF7]
            ); // 先頭 offset = 0

            // track 0 は未割当 = 空名 + 黒（colorIndex 70）
            assert_eq!(messages[2][9], 0x00);
            assert_eq!(messages[2][22], 70);

            // track 1 = 今回の projection
            let msg = &messages[3];
            // ヘッダ + 0A 07 + idx(7bit×2) + name13 + color + group + F7 = 25 byte
            assert_eq!(&msg[..7], &[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x07]);
            assert_eq!(&msg[7..9], &[0x00, 0x01]); // index 1
            assert_eq!(&msg[9..15], b"Lane A");
            assert_eq!(msg[21], 0x00); // name13 の終端
            // 純赤は palette の 0xFF0000（index 71）に量子化される
            assert_eq!(msg[22], 71);
            assert_eq!(msg[23], 0); // is_group = false
            assert_eq!(*msg.last().unwrap(), 0xF7);
            assert_eq!(msg.len(), 25);

            // 末尾は end detail（表示コミット）
            assert_eq!(
                messages[4].as_slice(),
                &[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0A, 0x08, 0xF7]
            );
        }

        #[test]
        fn out_of_range_track_is_noop() {
            let mut profile = RotoProfile::default();
            assert!(
                profile
                    .project_track(8, "X", Rgb::new(0, 0, 0), false)
                    .is_empty()
            );
        }

        #[test]
        fn name12_nul_truncates_to_12_with_terminator() {
            let slots = name12_nul("ABCDEFGHIJKLMNOP"); // 16 文字
            assert_eq!(&slots[..12], b"ABCDEFGHIJKL");
            assert_eq!(slots[12], 0x00); // 13 スロット目は必ず終端
        }

        #[test]
        fn sysex_name13_fills_13_chars() {
            let slots = sysex_name13("ABCDEFGHIJKLMNOP");
            assert_eq!(&slots[..], b"ABCDEFGHIJKLM"); // display 用は 13 文字フル
        }

        #[test]
        fn ascii_display_substitutes_latin_and_drops_others() {
            assert_eq!(to_ascii_display("Tür"), b"Tur".to_vec());
            assert_eq!(to_ascii_display("Straße"), b"Strasse".to_vec());
            assert_eq!(to_ascii_display("日本語"), Vec::<u8>::new());
        }

        #[test]
        fn hash6_is_sha1_prefix_masked() {
            // SHA-1("test") = a94a8fe5ccb1… → 先頭 6 byte を & 0x7F
            assert_eq!(hash6("test"), [0x29, 0x4A, 0x0F, 0x65, 0x4C, 0x31]);
        }

        #[test]
        fn learn_parameter_emits_learn_message() {
            let mut profile = RotoProfile::default();
            let spec = ParamSpec::continuous("Cutoff", 1.0);
            let messages = profile.learn_parameter(2, &spec);
            let msg = &messages[0];

            // ヘッダ + 0B 0A + idx2 + hash6 + macro + detent + steps + pos2 + name13 + F7 = 34
            assert_eq!(&msg[..7], &[0xF0, 0x00, 0x22, 0x03, 0x02, 0x0B, 0x0A]);
            assert_eq!(&msg[7..9], &[0x00, 0x02]); // knob index 2
            assert_eq!(hash6("Cutoff"), msg[9..15]); // id 省略時は name から
            assert_eq!(msg[15], 0); // is_macro
            assert_eq!(msg[16], 0); // center_detent
            assert_eq!(msg[17], 0); // steps = 連続
            assert_eq!(&msg[18..20], &[0x7F, 0x7F]); // 値 1.0 = 16383
            assert_eq!(&msg[20..26], b"Cutoff");
            assert_eq!(msg.len(), 34);
        }

        #[test]
        fn learn_parameter_appends_step_names() {
            let mut profile = RotoProfile::default();
            let mut spec = ParamSpec::continuous("Mode", 0.0);
            spec.steps = 3;
            spec.step_names = vec!["A".into(), "B".into()]; // 3 step 目は不足 = 空欄
            let messages = profile.learn_parameter(0, &spec);
            let msg = &messages[0];

            // 基本 34 byte + step ラベル 13×3
            assert_eq!(msg.len(), 34 + 39);
            assert_eq!(msg[33], b'A');
            assert_eq!(msg[33 + 13], b'B');
            assert_eq!(msg[33 + 26], 0x00); // 不足分は空欄
        }
    }
}
