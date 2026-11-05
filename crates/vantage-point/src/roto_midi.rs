//! VP の MIDI モード契約（design 84）。Serial 書き込みは midistage が担当する。
use crate::device_input::{ControlEvent, DeviceInput};

pub(crate) struct MidiButtonSetup {
    /// Human MIDI channel (1..=16).
    pub channel: u8,
    /// 物理列順の switch CC。列1〜8 = pin #1〜#8。
    pub buttons: [u8; 8],
    pub press: u8,
    pub release: u8,
}

pub(crate) const VP_ROTO_SETUP: MidiButtonSetup = MidiButtonSetup {
    channel: 16,
    buttons: [20, 21, 22, 23, 24, 25, 26, 27],
    press: 127,
    release: 0,
};

/// switch `index`（0〜7 = pin #1〜#8）の LED を灯す / 消す CC 1 本。
///
/// ROTO は MIDI mode でも、switch に割り当てた CC を**受信**すると値で LED の色を切り替える
/// （`press` = `led_on` 色 / `release` = `led_off` 色。実測 2026-11-05、`mem_1CgjSvYj7p3jAE7zKLhMgR`）。
/// 入力 parser と同じ定数から組むので、送った bytes を [`RotoMidiInput`] に通すと同じ index の
/// press / release に戻る（契約は 1 か所）。
pub(crate) fn switch_led(index: usize, on: bool) -> [u8; 3] {
    [
        0xb0 | (VP_ROTO_SETUP.channel - 1),
        VP_ROTO_SETUP.buttons[index],
        if on {
            VP_ROTO_SETUP.press
        } else {
            VP_ROTO_SETUP.release
        },
    ]
}

/// Input only: no handshake, mode switching, learning or feedback frames.
pub(crate) struct RotoMidiInput;
impl DeviceInput for RotoMidiInput {
    fn parse(&mut self, msg: &[u8]) -> Option<ControlEvent> {
        let [status, cc, value] = *msg else {
            return None;
        };
        if status != 0xb0 | (VP_ROTO_SETUP.channel - 1) {
            return None;
        }
        let index = VP_ROTO_SETUP
            .buttons
            .iter()
            .position(|button| *button == cc)? as u8;
        let pressed = if value == VP_ROTO_SETUP.press {
            true
        } else if value == VP_ROTO_SETUP.release {
            false
        } else {
            return None;
        };
        Some(ControlEvent::Button { index, pressed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_led_speaks_the_same_contract_as_the_input_parser() {
        assert_eq!(switch_led(0, true), [0xBF, 20, 127]);
        assert_eq!(switch_led(7, false), [0xBF, 27, 0]);
        let mut input = RotoMidiInput;
        assert!(matches!(
            input.parse(&switch_led(3, true)),
            Some(ControlEvent::Button {
                index: 3,
                pressed: true
            })
        ));
        assert!(matches!(
            input.parse(&switch_led(3, false)),
            Some(ControlEvent::Button {
                index: 3,
                pressed: false
            })
        ));
    }
}
