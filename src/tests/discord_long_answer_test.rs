//! FR-009 (#1880): a long Discord answer ships as page 0 plus a pager, never
//! as a wall of consecutive messages (AC-020), with exactly one Action Row
//! (AC-021).
//!
//! The pure parts — the page body and the pager row — are pinned by the
//! inline tests in `long_answer.rs`. What those cannot reach is the DECISION:
//! which answers get paged, and what must not fire alongside the pager. That
//! lives in `handler.rs` behind an `Http` call, so it is pinned here by
//! scanning the source, the same way `discord_fr004_one_message_test` pins
//! AC-008 and `discord_write_discipline_test` pins AC-024.
//!
//! The three properties worth locking, each of which can silently regress:
//!
//! * paging is keyed on `chunks.len() > 1` — the split result, not a char
//!   count re-derived from the text (a second threshold would drift);
//! * `paged` gates BOTH the auto-thread and the plain-split fallback, or one
//!   long answer posts twice;
//! * page 0 is stored before the pager is attached, or the button points at
//!   a message whose pages nobody kept.

use std::path::Path;

fn flattened(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// AC-020: the decision to page is the split result, and page 0 is what goes
/// out in-channel.
#[test]
fn a_split_answer_is_posted_as_page_zero_with_a_pager() {
    let src = flattened("src/channels/discord/handler.rs");

    assert!(
        src.contains(
            "letchunks:Vec<String>=split_message(&text_only,super::long_answer::PAGE_CHARS);"
        ),
        "the final answer must be split at PAGE_CHARS to learn the page count"
    );
    assert!(
        src.contains("letpaged=ifchunks.len()>1{"),
        "paging must be keyed on the split result (chunks.len() > 1)"
    );
    assert!(
        src.contains("CreateMessage::new().content(&chunks[0])"),
        "page 0 must be the message posted in-channel"
    );
}

/// The button is useless unless the pages it indexes were kept first, and it
/// must be attached to the message that was actually sent.
#[test]
fn the_pages_are_stored_before_the_pager_is_attached() {
    let src = flattened("src/channels/discord/handler.rs");

    let store = src
        .find("discord_state.store_long_answer(mid,chunks.clone()).await")
        .expect("page 0 must be stored before the pager is attached");
    let attach = src
        .find("super::long_answer::pager_row(mid,0,chunks.len())")
        .expect("the pager row must be built from the stored message id");
    assert!(
        store < attach,
        "store_long_answer must run BEFORE pager_row is attached, or the \
         button indexes pages that were never kept"
    );
}

/// AC-020 again, from the other side: an answer that already went out as page
/// 0 must not ALSO be auto-threaded or split into plain messages. Both paths
/// are gated on `paged`.
#[test]
fn paging_suppresses_the_thread_and_the_plain_split() {
    let src = flattened("src/channels/discord/handler.rs");

    assert!(
        src.contains("letauto_thread=!paged"),
        "the auto-thread must be gated on !paged, or a long answer posts twice"
    );
    assert!(
        src.contains("}elseif!paged{"),
        "the plain-split fallback must be gated on !paged"
    );
}

/// AC-021: one Action Row rides the paged message. More than one row is what
/// Discord rejects (5 rows max, and the pager needs exactly one).
#[test]
fn the_pager_is_exactly_one_action_row() {
    let src = flattened("src/channels/discord/long_answer.rs");

    assert!(
        src.contains("CreateActionRow::Buttons(vec![CreateButton::new(format!("),
        "the pager must be a single button row"
    );
    // The row builder is the only place a row is constructed.
    assert_eq!(
        src.matches("CreateActionRow::Buttons(").count(),
        1,
        "exactly one Action Row constructor belongs in long_answer.rs"
    );
}
