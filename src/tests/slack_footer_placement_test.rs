//! The ctx budget footer lives on the settled step-group line ONLY (#1806).
//!
//! Slack used to append the footer to every answer message: the final post
//! carried it as a context block, the streaming chunks rode it on the last
//! chunk, the kept-intermediate path edited it in via chat.update, and the
//! settled line showed it again, so every turn displayed the budget twice.
//! Telegram shows ctx once, in the flow chrome; this pins Slack to the same
//! shape.
//!
//! These are source-scan sentinels: the answer-delivery paths are network
//! calls (chat.post / chat.update against a live Slack session), so the
//! regression guard is lexical. If any footer-on-answer attachment sneaks
//! back into `handler.rs`, these fail before CI does.

/// The handler must have no footer-on-answer attachment anywhere: no context
/// block builder call, no edit-in helper, no intermediate ts bookkeeping for
/// footer rebuilds (the list went fully dead with #943's folding).
#[test]
fn handler_has_no_footer_on_answer_attachment() {
    let handler = include_str!("../channels/slack/handler.rs");
    assert!(
        !handler.contains("context_footer"),
        "handler.rs re-attached a ctx footer context block to a message; \
         the budget lives on the settled step-group line only (#1806)"
    );
    assert!(
        !handler.contains("append_footer_via_update"),
        "handler.rs resurrected the footer-edit-into-answer path (#1806)"
    );
    assert!(
        !handler.contains("sent_intermediate_ts"),
        "handler.rs resurrected the intermediate ts list; it has been dead \
         since #943 folded narration into the step group"
    );
}

/// The settled line keeps the budget: settle_step_group still takes the
/// footer string, and blocks.rs no longer ships a footer context block for
/// anyone else to misuse.
#[test]
fn settled_line_is_the_only_footer_carrier() {
    let handler = include_str!("../channels/slack/handler.rs");
    assert!(
        handler.contains("ctx_footer: &str"),
        "settle_step_group lost its ctx_footer param; the settled line is the \
         one place the budget is allowed to render (#1806)"
    );
    let blocks = include_str!("../channels/slack/blocks.rs");
    assert!(
        !blocks.contains("fn context_footer"),
        "blocks.rs still ships a footer context block builder; with answer \
         messages clean there is no legitimate consumer left (#1806)"
    );
}
