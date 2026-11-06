//! 型生成と fixture の正本は agent-chat/crates/protocol/tests/fixtures.rs。
use vantage_point::conversation::ConversationEvent;

#[test]
fn shared_protocol_is_the_consumer_event_type() {
    // adapter で別 enum に変換して情報を落とさず、同じ型を公開していること。
    let event = agent_chat_protocol::ConversationEvent::MessageChunk {
        text: "hello".into(),
    };
    let consumer: ConversationEvent = event;
    assert_eq!(serde_json::to_value(consumer).unwrap()["text"], "hello");
}
