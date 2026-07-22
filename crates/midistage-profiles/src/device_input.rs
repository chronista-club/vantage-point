//! 物理 controller からの入力イベントを device 非依存に解釈する（E2 input flow）。
//!
//! [`crate::device_profile`]（出力 = VP → 機材の state projection）の対。
//! こちらは機材 → VP の方向で、raw MIDI byte を論理 [`ControlEvent`] に変換する。
//! 「双方向 = 2 つの片方向 flow の合成」（doc 20 §9）の input 側。
//!
//! 責務分離（data / calculations / actions）:
//! - data: [`ControlEvent`]（機材操作の論理表現）
//! - calculations: [`DeviceInput::parse`]（byte → event。純粋、I/O なし）
//! - actions: 呼び出し側が ControlEvent を VP の lane command に routing（別 flow）

/// 機材から来た物理操作を device 非依存に論理イベント化したもの。
///
/// index はその機材内での 0 始まりの要素番号（knob 0–7 等）。値は正規化（0.0–1.0）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// knob / encoder を回した（正規化値）
    Knob { index: u8, value: f32 },
    /// knob の touch センサー（触れた / 離した）
    KnobTouch { index: u8, pressed: bool },
    /// button の押下 / 解放
    Button { index: u8, pressed: bool },
    /// motor fader を動かした（正規化値）
    Fader { index: u8, value: f32 },
    /// fader の touch センサー（触れた / 離した）。MCU 系 fader は touch → 値 → release の
    /// 順で届き、release が「手放し = settle」のタイミングを与える
    FaderTouch { index: u8, pressed: bool },
    /// pad を叩いた（velocity 1–127、0 は離した扱いで pressed=false 相当）
    Pad { index: u8, velocity: u8 },
}

/// 機材 1 機種ぶんの「MIDI byte 列 → 論理イベント」変換。
///
/// hi-res CC（ROTO の knob は hi/lo の 2 メッセージで 1 値）のように、複数 byte 列で
/// 1 イベントが確定する機材があるため `&mut self`（途中状態を保持する）。
/// 1 メッセージでイベントが確定しない（hi byte だけ等）場合は `None` を返す。
pub trait DeviceInput {
    /// 受信した 1 MIDI メッセージを解釈する。確定イベントが無ければ `None`。
    fn parse(&mut self, msg: &[u8]) -> Option<ControlEvent>;
}

pub mod roto {
    //! ROTO-CONTROL の入力 parser（`MidiProcessor.handleMidiIn` 準拠）。
    //!
    //! 全入力は ch16 の CC（status `0xBF`）で来る（decompile で確定）:
    //! - knob 値: CC `12+i`(hi) + CC `44+i`(lo) の 14bit（`setCcKnobMatcher`）
    //! - knob touch: CC `52+i`（`setCcMatcher(touchButton, 52+index)`）
    //! - button: CC `20+i`（本体 8 = 20-27）/ `28+i`（transport 8 = 28-35）/ `36,37`
    //!   （left/right transport = `RotoCcButton`、transport セクションの ◄ ► = 素の CC button）
    //!
    //! 注意: **左 ctrl 列の ← / → は別物**で、mode/nav の semantic SysEx（`0C 01` mixer nav 等）
    //! を送る（CC ではない）。prev/next に使える素の CC は transport ◄ ► = CC 36/37。
    //! addressing は decompile (RotoHwElements) + creo mem_1CbwsgkwHx7YbEzb7JWTJd で確定。
    //!
    //! 値は hi → lo の順で届く前提で、lo 受信時に 14bit を合成して確定させる。

    use super::{ControlEvent, DeviceInput};

    const KNOB_HI_BASE: u8 = 12;
    const KNOB_LO_BASE: u8 = 44;
    const KNOB_TOUCH_BASE: u8 = 52;
    const BUTTON_BASE: u8 = 20;
    /// button の CC 連続範囲は 20–37: 本体 8 (20-27) + transport 8 (28-35) +
    /// left/right transport ◄ ► (36,37)。後者は Button index 16 (◄=CC36) / 17 (►=CC37)。
    const BUTTON_COUNT: u8 = 18;
    /// CC value が押下とみなす閾値（127=押下 / 0=解放）
    const PRESS_THRESHOLD: u8 = 64;

    /// ROTO 入力 parser。knob の hi byte を lo 受信まで保持する。
    #[derive(Default)]
    pub struct RotoInput {
        /// 各 knob（0–7）の最新 hi byte（lo 受信時に 14bit へ合成）
        knob_hi: [u8; 8],
    }

    impl DeviceInput for RotoInput {
        fn parse(&mut self, msg: &[u8]) -> Option<ControlEvent> {
            // ROTO 入力は ch16 CC（0xBF）のみ。SysEx / 他 channel は無視
            if msg.len() < 3 || msg[0] != 0xBF {
                return None;
            }
            let cc = msg[1];
            let val = msg[2] & 0x7F;

            if (KNOB_HI_BASE..KNOB_HI_BASE + 8).contains(&cc) {
                // hi byte は保持のみ（lo で確定）
                self.knob_hi[(cc - KNOB_HI_BASE) as usize] = val;
                None
            } else if (KNOB_LO_BASE..KNOB_LO_BASE + 8).contains(&cc) {
                let index = cc - KNOB_LO_BASE;
                let value14 = ((self.knob_hi[index as usize] as u16) << 7) | val as u16;
                Some(ControlEvent::Knob {
                    index,
                    value: value14 as f32 / 16383.0,
                })
            } else if (KNOB_TOUCH_BASE..KNOB_TOUCH_BASE + 8).contains(&cc) {
                Some(ControlEvent::KnobTouch {
                    index: cc - KNOB_TOUCH_BASE,
                    pressed: val >= PRESS_THRESHOLD,
                })
            } else if (BUTTON_BASE..BUTTON_BASE + BUTTON_COUNT).contains(&cc) {
                Some(ControlEvent::Button {
                    index: cc - BUTTON_BASE,
                    pressed: val >= PRESS_THRESHOLD,
                })
            } else {
                None
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn knob_hi_then_lo_combines_to_14bit() {
            let mut input = RotoInput::default();
            // knob 2: hi=0x7F, lo=0x7F → 16383 = 1.0
            assert_eq!(input.parse(&[0xBF, 14, 0x7F]), None); // hi だけは未確定
            let ev = input.parse(&[0xBF, 46, 0x7F]).unwrap(); // lo で確定
            match ev {
                ControlEvent::Knob { index, value } => {
                    assert_eq!(index, 2);
                    assert!((value - 1.0).abs() < 1e-6);
                }
                _ => panic!("expected Knob"),
            }
        }

        #[test]
        fn knob_zero_value() {
            let mut input = RotoInput::default();
            input.parse(&[0xBF, 12, 0x00]); // knob 0 hi = 0
            assert_eq!(
                input.parse(&[0xBF, 44, 0x00]),
                Some(ControlEvent::Knob {
                    index: 0,
                    value: 0.0
                })
            );
        }

        #[test]
        fn knob_touch_press_and_release() {
            let mut input = RotoInput::default();
            assert_eq!(
                input.parse(&[0xBF, 52, 127]),
                Some(ControlEvent::KnobTouch {
                    index: 0,
                    pressed: true
                })
            );
            assert_eq!(
                input.parse(&[0xBF, 59, 0]),
                Some(ControlEvent::KnobTouch {
                    index: 7,
                    pressed: false
                })
            );
        }

        #[test]
        fn button_and_transport() {
            let mut input = RotoInput::default();
            assert_eq!(
                input.parse(&[0xBF, 20, 127]),
                Some(ControlEvent::Button {
                    index: 0,
                    pressed: true
                })
            ); // 本体 button 0
            assert_eq!(
                input.parse(&[0xBF, 35, 127]),
                Some(ControlEvent::Button {
                    index: 15,
                    pressed: true
                })
            ); // transport button 末尾
            // left/right transport ◄ ► = CC 36/37 → Button 16/17（RotoCcButton、素の CC）
            assert_eq!(
                input.parse(&[0xBF, 36, 127]),
                Some(ControlEvent::Button {
                    index: 16,
                    pressed: true
                })
            ); // ◄ left transport
            assert_eq!(
                input.parse(&[0xBF, 37, 0]),
                Some(ControlEvent::Button {
                    index: 17,
                    pressed: false
                })
            ); // ► right transport
        }

        #[test]
        fn sysex_and_other_channels_ignored() {
            let mut input = RotoInput::default();
            assert_eq!(input.parse(&[0xF0, 0x00, 0x22, 0xF7]), None); // SysEx
            assert_eq!(input.parse(&[0x90, 60, 100]), None); // note on ch1
            assert_eq!(input.parse(&[0xBF, 99, 0]), None); // 範囲外 CC
        }
    }
}

pub mod xtouch {
    //! X-Touch（MCU mode）の入力 parser。byte 仕様の出典は Ardour `libs/surfaces/mackie/`
    //! （[`crate::device_profile::xtouch`] 出力側と同一ソース）:
    //! - fader 値: pitch bend `0xE0|ch`（ch 0–7 = strip、8 = master）の 14bit
    //! - fader touch: Note `0x68+i`（104–111）の on/off（`fader.cc` touch handling）
    //!
    //! V-Pot（CC 16–23、relative 2's complement）と button 群は現時点で mapping 先が
    //! 無いため解釈しない（[[writer-without-reader]] — 読み手が現れた時に足す）。

    use super::{ControlEvent, DeviceInput};

    /// fader touch の note 先頭（`0x68` = strip 1）
    const FADER_TOUCH_BASE: u8 = 0x68;
    /// strip 8 + master = 9 本
    const FADER_COUNT: u8 = 9;

    /// X-Touch 入力 parser。fader は 1 メッセージで確定するため状態を持たない。
    #[derive(Default)]
    pub struct XTouchInput;

    impl DeviceInput for XTouchInput {
        fn parse(&mut self, msg: &[u8]) -> Option<ControlEvent> {
            if msg.len() < 3 {
                return None;
            }
            let status = msg[0] & 0xF0;
            let channel = msg[0] & 0x0F;
            match status {
                // pitch bend = fader 値（LSB, MSB の順）
                0xE0 if channel < FADER_COUNT => {
                    let value14 = ((msg[2] as u16) << 7) | (msg[1] & 0x7F) as u16;
                    Some(ControlEvent::Fader {
                        index: channel,
                        value: value14 as f32 / 16383.0,
                    })
                }
                // note on/off = fader touch（velocity 0 = release、MCU は 0x90 で両方送る個体もある）
                0x90 | 0x80
                    if (FADER_TOUCH_BASE..FADER_TOUCH_BASE + FADER_COUNT).contains(&msg[1]) =>
                {
                    Some(ControlEvent::FaderTouch {
                        index: msg[1] - FADER_TOUCH_BASE,
                        pressed: status == 0x90 && msg[2] > 0,
                    })
                }
                _ => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pitch_bend_becomes_fader() {
            let mut input = XTouchInput;
            // ch1 (index 0)、14bit 最大
            assert_eq!(
                input.parse(&[0xE0, 0x7F, 0x7F]),
                Some(ControlEvent::Fader {
                    index: 0,
                    value: 1.0
                })
            );
            // ch9 (index 8) = master、0
            match input.parse(&[0xE8, 0x00, 0x00]) {
                Some(ControlEvent::Fader { index: 8, value }) => assert!(value.abs() < 1e-6),
                other => panic!("expected master fader, got {:?}", other),
            }
        }

        #[test]
        fn fader_touch_press_and_release() {
            let mut input = XTouchInput;
            assert_eq!(
                input.parse(&[0x90, 0x68, 0x7F]),
                Some(ControlEvent::FaderTouch {
                    index: 0,
                    pressed: true
                })
            );
            // velocity 0 の note on = release（実機挙動）
            assert_eq!(
                input.parse(&[0x90, 0x68, 0x00]),
                Some(ControlEvent::FaderTouch {
                    index: 0,
                    pressed: false
                })
            );
            assert_eq!(
                input.parse(&[0x80, 0x70, 0x00]),
                Some(ControlEvent::FaderTouch {
                    index: 8,
                    pressed: false
                })
            );
        }

        #[test]
        fn vpot_and_buttons_ignored() {
            let mut input = XTouchInput;
            assert_eq!(input.parse(&[0xB0, 16, 0x01]), None); // V-Pot relative
            assert_eq!(input.parse(&[0x90, 0x5E, 0x7F]), None); // transport button
            assert_eq!(input.parse(&[0xE9, 0x00, 0x40]), None); // 範囲外 pitch bend ch
        }
    }
}

pub mod lpd8 {
    //! LPD8（mk2、VP program） の入力 parser。
    //!
    //! byte 配置は `vp midi lpd8 write` が書き込む VP program（`midi::lpd8::Program::vp_default`）
    //! が正: pad = Note 36–43（momentary）、knob = CC 70–77。channel は program 設定依存の
    //! ため見ない（note / CC 番号の一致だけで判定する — 他 program 使用時は範囲外で自然に無視）。

    use super::{ControlEvent, DeviceInput};

    const PAD_NOTE_BASE: u8 = 36;
    const KNOB_CC_BASE: u8 = 70;

    /// LPD8 入力 parser。全メッセージが単発で確定するため状態を持たない。
    #[derive(Default)]
    pub struct Lpd8Input;

    impl DeviceInput for Lpd8Input {
        fn parse(&mut self, msg: &[u8]) -> Option<ControlEvent> {
            if msg.len() < 3 {
                return None;
            }
            let status = msg[0] & 0xF0;
            match status {
                0x90 | 0x80 if (PAD_NOTE_BASE..PAD_NOTE_BASE + 8).contains(&msg[1]) => {
                    Some(ControlEvent::Pad {
                        index: msg[1] - PAD_NOTE_BASE,
                        // note off / velocity 0 は「離した」= velocity 0 に正規化
                        velocity: if status == 0x90 { msg[2] & 0x7F } else { 0 },
                    })
                }
                0xB0 if (KNOB_CC_BASE..KNOB_CC_BASE + 8).contains(&msg[1]) => {
                    Some(ControlEvent::Knob {
                        index: msg[1] - KNOB_CC_BASE,
                        value: (msg[2] & 0x7F) as f32 / 127.0,
                    })
                }
                _ => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pad_press_and_release() {
            let mut input = Lpd8Input;
            assert_eq!(
                input.parse(&[0x90, 36, 100]),
                Some(ControlEvent::Pad {
                    index: 0,
                    velocity: 100
                })
            );
            assert_eq!(
                input.parse(&[0x80, 43, 64]),
                Some(ControlEvent::Pad {
                    index: 7,
                    velocity: 0
                })
            );
        }

        #[test]
        fn knob_cc_normalized() {
            let mut input = Lpd8Input;
            match input.parse(&[0xB0, 77, 127]) {
                Some(ControlEvent::Knob { index: 7, value }) => assert!((value - 1.0).abs() < 1e-6),
                other => panic!("expected knob, got {:?}", other),
            }
        }

        #[test]
        fn out_of_range_ignored() {
            let mut input = Lpd8Input;
            assert_eq!(input.parse(&[0x90, 60, 100]), None); // 範囲外 note
            assert_eq!(input.parse(&[0xB0, 1, 64]), None); // 範囲外 CC（pad の CC mode 等）
            assert_eq!(input.parse(&[0xC0, 0]), None); // program change（2 byte）
        }
    }
}
