use bytes::Bytes;
use study_server::protocol::GameFrame;

#[test]
fn game_packet_has_symmetric_fixed_header_and_preserves_payload() {
    let wire = Bytes::from_static(&[10, 0, 0, 0, 0, 0, 0, 0, 42, 0, 7, 9, 8]);
    let frame = GameFrame::decode(wire.clone()).unwrap();
    assert_eq!(frame.room_id, 42);
    assert_eq!(frame.opcode, 7);
    assert_eq!(&frame.payload[..], &[9, 8]);
    assert_eq!(frame.encode(), wire);
}

#[test]
fn truncated_or_wrong_kind_game_frames_are_rejected() {
    for n in 0..11 {
        assert!(GameFrame::decode(Bytes::from(vec![10; n])).is_err());
    }
    assert!(GameFrame::decode(Bytes::from_static(&[3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0])).is_err());
}

#[test]
fn manual_create_control_deserializes_with_request_id() {
    let request: study_server::protocol::Request =
        serde_json::from_str(r#"{"request_id":7,"op":"create","capacity":2}"#).unwrap();
    assert_eq!(request.request_id, 7);
    assert!(matches!(
        request.operation,
        study_server::protocol::Operation::Create { capacity: 2 }
    ));
}
