//! The ctx budget line lives on the settled step-group chrome ONLY (#1842).
//!
//! Discord used to append the ctx footer to every answer message: the final
//! post carried it inline on the last chunk (or as its own chunk), the
//! kept-intermediate dedup path edited it into the kept message via
//! edit_message, and the settled line showed it again — every turn displayed
//! the budget twice, and the completion message dragged a token count under
//! prose nobody asked for it on. #1841 gave the budget its home on the flow
//! group; this pins the answer messages clean, matching Telegram and the
//! Slack #1806 shape.
//!
//! Source-scan sentinels: the delivery paths are network calls
//! (send_message / edit_message against a live Discord session), so the
//! regression guard is lexical. If a footer-on-answer attachment sneaks
//! back into `handler.rs`, these fail before CI does.

#[test]
fn handler_has_no_footer_on_answer_attachment() {
    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        !handler.contains("footer_edit_target"),
        "handler.rs resurrected the kept-intermediate footer edit path; \
         answer messages carry no ctx footer (#1842)"
    );
    assert!(
        !handler.contains("last_chunk"),
        "handler.rs resurrected last-chunk capture; the MessageId + chunk \
         bookkeeping existed only to position the footer edit (#1842)"
    );
    assert!(
        !handler.contains("Option<(MessageId, String)>"),
        "handler.rs resurrected the intermediate id/chunk tuple; \
         SentIntermediate records bodies only since #1842"
    );
}

#[test]
fn ctx_line_feeds_only_the_settled_chrome() {
    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        handler.contains("let ctx_line = crate::utils::format_ctx_footer("),
        "the settled-line ctx source went missing; the budget must still \
         render on the flow group's settled chrome (#1841/#1842)"
    );
    assert!(
        handler.contains("settle_tool_group("),
        "settle_tool_group went missing; the settled chrome is the one place \
         the budget renders (#1842)"
    );
}

#[test]
fn intermediate_dedup_still_guards_the_final_post() {
    // Removing the footer must not have taken the dedup with it: the
    // keep-intermediate outcome (#459) and the empty-final guard
    // (#943/#951) are load-bearing.
    let handler = include_str!("../channels/discord/handler.rs");
    assert!(
        handler.contains("let skip_final_post = {"),
        "the final-post dedup guard went missing; removing the footer must \
         not remove the duplicate-answer protection (#1842)"
    );
    assert!(
        handler.contains("type SentIntermediate = String;"),
        "SentIntermediate must record only the post body; id/chunk \
         bookkeeping is gone for good (#1842)"
    );
}
