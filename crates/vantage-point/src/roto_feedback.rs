//! Pin display snapshots. Diff at the hardware consumer, after latest-wins delivery.
use crate::daemon::protocol::RotoSlotFeedback;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RotoFrames {
    pub tracks: Vec<Vec<u8>>,
    pub parameters: Vec<Vec<Vec<u8>>>,
}

pub(crate) fn project(_slots: &[RotoSlotFeedback]) -> RotoFrames {
    RotoFrames::default()
}

impl RotoFrames {
    pub(crate) fn changes_from(&self, _previous: &Self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot() -> RotoSlotFeedback {
        RotoSlotFeedback {
            index: 3, name: "ABCDEFGHIJKLMNO".into(), color: [240,180,90],
            id: "vp/topic".into(), status: "Ask 42%".into(), steps: 2,
            value: 1.0, labels: vec!["cc#2".into(),"review".into()], touched: false,
        }
    }
    #[test]
    fn projects_eight_framed_tracks_palette_names_and_session_detents() {
        let out = project(&[slot()]);
        assert_eq!(out.tracks.len(),11);
        let empty = &out.tracks[2];
        assert_eq!(&empty[9..12], &[b'#',b'1',0]);
        assert_eq!(empty[22], crate::roto_palette::closest_index(0,0,0));
        let track = &out.tracks[5];
        assert_eq!(&track[9..22], b"ABCDEFGHIJKL\0");
        assert_eq!(track[22], crate::roto_palette::closest_index(240,180,90));
        let learn = &out.parameters[3][0];
        assert_eq!(learn[17],2);
        assert_eq!(&learn[18..20], &[127,127]);
        assert_eq!(&learn[20..27], b"Ask 42%");
        assert_eq!(&learn[46..52], b"review");
    }
    #[test]
    fn consumer_diffs_latest_snapshot_without_losing_skipped_slot_updates() {
        let initial=project(&[]);
        let mut a=slot();
        let middle=project(&[a.clone()]);
        a.index=4;
        let latest=project(&[slot(),a]);
        // middle may never be read by watch consumer: latest must contain both changes.
        assert!(!latest.changes_from(&initial).is_empty());
        assert!(latest.changes_from(&latest).is_empty());
        assert_ne!(middle,latest);
        assert_eq!(latest.changes_from(&RotoFrames::default()).len(),35);
    }
    #[test]
    fn touch_defers_parameter_and_motor_then_release_restores_both() {
        let initial=project(&[slot()]);
        let mut held=slot(); held.touched=true;
        let touching=project(&[held]);
        assert!(touching.parameters[3].is_empty());
        assert!(touching.changes_from(&initial).is_empty());
        assert_eq!(initial.changes_from(&touching),initial.parameters[3]);
    }
}
