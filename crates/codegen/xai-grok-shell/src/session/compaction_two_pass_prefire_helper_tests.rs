use super::{fingerprint_prefix, prefire_lead_percent};
use xai_grok_sampling_types::{ContentPart, ConversationItem, ToolCall};

#[test]
fn fingerprint_stable_for_same_prefix() {
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
        ConversationItem::assistant("hi"),
    ];
    let fingerprint = fingerprint_prefix(&items).expect("serializable prefix");
    assert_eq!(fingerprint_prefix(&items), Some(fingerprint));
}

#[test]
fn fingerprint_changes_when_prefix_content_changes() {
    let base = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("hello"),
    ];
    let edited = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("HELLO there"), // The changed text stands in for a real edit or rewind of the prefix
    ];
    assert_ne!(
        fingerprint_prefix(&base),
        fingerprint_prefix(&edited),
        "a changed prefix must invalidate the cached NOTE1 fingerprint"
    );
}

#[test]
fn fingerprint_changes_with_length() {
    let short = vec![ConversationItem::user("a")];
    let long = vec![
        ConversationItem::user("a"),
        ConversationItem::assistant("b"),
    ];
    assert_ne!(fingerprint_prefix(&short), fingerprint_prefix(&long));
}

#[test]
fn fingerprint_changes_when_tool_call_changes_without_text_changes() {
    let call = ToolCall {
        id: "call-1".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"before.rs"}"#.into(),
    };
    let base = ConversationItem::assistant_tool_calls(vec![call.clone()]);
    for edited_call in [
        ToolCall {
            arguments: r#"{"path":"after.rs"}"#.into(),
            ..call.clone()
        },
        ToolCall {
            name: "write_file".into(),
            ..call.clone()
        },
        ToolCall {
            id: "call-2".into(),
            ..call
        },
    ] {
        let edited = ConversationItem::assistant_tool_calls(vec![edited_call]);
        assert_eq!(base.text_content(), edited.text_content());
        assert_ne!(
            fingerprint_prefix(std::slice::from_ref(&base)),
            fingerprint_prefix(&[edited]),
        );
    }
}

#[test]
fn fingerprint_changes_when_user_image_changes_without_text_changes() {
    let mut before = ConversationItem::user("Inspect this screenshot");
    before.add_image("https://example.com/before.png");
    let mut after = ConversationItem::user("Inspect this screenshot");
    after.add_image("https://example.com/after.png");
    assert_eq!(before.text_content(), after.text_content());
    assert_ne!(fingerprint_prefix(&[before]), fingerprint_prefix(&[after]));
}

#[test]
fn fingerprint_changes_when_tool_result_link_or_image_changes() {
    let image = |url: &str| ContentPart::Image { url: url.into() };
    let before = ConversationItem::tool_result_with_images(
        "call-1",
        "Screenshot",
        vec![image("https://example.com/before.png")],
    );
    for after in [
        ConversationItem::tool_result_with_images(
            "call-2",
            "Screenshot",
            vec![image("https://example.com/before.png")],
        ),
        ConversationItem::tool_result_with_images(
            "call-1",
            "Screenshot",
            vec![image("https://example.com/after.png")],
        ),
    ] {
        assert_eq!(before.text_content(), after.text_content());
        assert_ne!(
            fingerprint_prefix(std::slice::from_ref(&before)),
            fingerprint_prefix(&[after]),
        );
    }
}

#[test]
fn prefire_lead_percent_defaults_to_10() {
    // SAFETY: single-threaded test mutation of our own env var.
    unsafe { std::env::remove_var("GROK_PREFIRE_LEAD_PERCENT") };
    assert_eq!(prefire_lead_percent(), 10);
}
