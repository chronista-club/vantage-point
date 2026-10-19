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
