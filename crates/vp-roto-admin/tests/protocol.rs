use vp_roto_admin::{Get, Parser, Event};

#[test]
fn only_get_frames_and_valid_indices() {
    assert_eq!(Get::Firmware.frame().unwrap(), [0x5a, 1, 1, 0, 0]);
    assert_eq!(Get::Mode.frame().unwrap(), [0x5a, 1, 2, 0, 0]);
    assert_eq!(Get::CurrentSetup.frame().unwrap(), [0x5a, 2, 1, 0, 0]);
    assert_eq!(Get::Setup(63).frame().unwrap(), [0x5a, 2, 2, 0, 1, 63]);
    assert_eq!(Get::Knob(63,31).frame().unwrap(), [0x5a,2,5,0,2,63,31]);
    assert_eq!(Get::Switch(0,0).frame().unwrap(), [0x5a,2,6,0,2,0,0]);
    assert!(Get::Setup(64).frame().is_err());
    assert!(Get::Knob(0,32).frame().is_err());
}

#[test]
fn fragmented_notification_noise_and_marker_bytes_inside_response() {
    let mut p = Parser::new(3);
    assert_eq!(p.feed(&[9,0x5a,1]).unwrap(), vec![]);
    assert_eq!(p.feed(&[8,0,2,0xa5]).unwrap(), vec![]);
    assert_eq!(p.feed(&[0x5a,0xa5,0,0x5a]).unwrap(), vec![Event::Notification { family:1, sub:8, data:vec![0xa5,0x5a] }]);
    assert_eq!(p.feed(&[0xa5,3]).unwrap(), vec![Event::Response { code:0, data:vec![0x5a,0xa5,3] }]);
}

#[test]
fn unconfigured_has_no_payload_and_parser_rejects_unbounded_notification() {
    let mut p = Parser::new(237);
    assert_eq!(p.feed(&[0xa5,0xfd]).unwrap(), vec![Event::Response {code:0xfd,data:vec![]}]);
    assert!(Parser::new(2).feed(&[0x5a,1,1,0xff,0xff]).is_err());
}

#[test]
fn all_split_points_preserve_binary_payload() {
    let payload: Vec<_> = (0..237).map(|i| i as u8).collect();
    let mut wire = vec![0xa5,0]; wire.extend(&payload);
    for split in 0..=wire.len() {
        let mut p = Parser::new(237);
        let mut events = p.feed(&wire[..split]).unwrap();
        events.extend(p.feed(&wire[split..]).unwrap());
        assert_eq!(events, vec![Event::Response{code:0,data:payload.clone()}]);
    }
}
