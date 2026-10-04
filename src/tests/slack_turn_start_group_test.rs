//! #1808: the Slack flow group opens at turn start and the static thinking
//! placeholder is gone. The live group (rolling clock, step counts) IS the
//! processing feedback; nothing static is ever posted. Source-scan sentinels
//! pin the structural contract the way #1806/#1807 do.

use std::path::PathBuf;

fn handler_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/channels/slack/handler.rs");
    std::fs::read_to_string(path).expect("read slack handler source")
}

#[test]
fn static_thinking_placeholder_is_gone() {
    let src = handler_source();
    // The exact literal the placeholder posted must not appear anywhere in
    // the handler, not as a posted message, not in a comment.
    assert!(
        !src.contains("\"_thinking..._\""),
        "Slack handler still posts a static thinking placeholder (#1808)"
    );
    // The whole placeholder machinery is deleted, not just the literal:
    // no ts slot, no delete-on-first-tool-call, no delete-at-settle.
    assert!(
        !src.contains("thinking_ts"),
        "Slack handler still carries thinking-placeholder plumbing (#1808)"
    );
}

#[test]
fn flow_group_opens_at_turn_start_before_the_ticker() {
    let src = handler_source();
    let open_at = src
        .find("Open the flow group at turn start")
        .expect("turn-start group creation block missing (#1808)");
    let ticker_at = src
        // Anchor on the call-site comment: the fn definition earlier in the
        // file also contains "spawn_flow_ticker(" and would match first.
        .find("// Roll the flow clock between tool events (#1807)")
        .expect("flow ticker spawn missing (#1807)");
    assert!(
        open_at < ticker_at,
        "the flow group must be created BEFORE the ticker spawns so the clock covers thinking (#1808)"
    );
    // The turn-start block creates the group through sync_step_group with
    // empty entries (the same lazy-create path tool steps use), not a
    // bespoke chat_post_message.
    let between = &src[open_at..ticker_at];
    assert!(
        between.contains("sync_step_group(") && between.contains("Vec::new()"),
        "turn-start group creation must go through sync_step_group with empty entries (#1808)"
    );
}
