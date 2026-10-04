//! FR-009 / AC-020: a long answer is a summary plus a pager, never a wall of
//! consecutive messages.
//!
//! The pager itself is unit-tested in `src/channels/discord/long_answer.rs`
//! (page body, button label, custom-id round trip, one row per button). What
//! those tests cannot see is the *decision* that routes a long answer into the
//! pager at all, because it lives inside `handle_message` next to the send.
//!
//! That decision is the thing AC-020 actually names, and a doc comment cannot
//! fail a build, so this test reads the send path and pins the shape:
//!
//! - the pager is gated on the body splitting into more than one page;
//! - page 0 goes out in-channel with the pager row attached;
//! - the auto-thread path is mutually exclusive with the pager, which is what
//!   keeps the `for chunk in &chunks` loop from ever running on a paged answer.
//!
//! Scope, stated so a later reader does not "fix" it by widening or narrowing:
//! this pins the routing decision in `handler.rs`. It does not re-test the
//! pager's rendering, and it does not assert anything about Telegram.

use std::path::Path;

fn discord_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord")
}

/// `handler.rs` as written, for the positional checks.
fn handler_src() -> String {
    let path = discord_dir().join("handler.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The same source with all whitespace removed, so a call split across lines
/// still matches a needle written on one line.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A long answer must route into the pager rather than into a run of messages.
#[test]
fn a_long_answer_routes_into_the_pager_not_a_wall() {
    let flat_src = flat(&handler_src());

    assert!(
        flat_src.contains("chunks.len()>1"),
        "the send path no longer gates the pager on the body splitting into \
         more than one page (AC-020). Without that gate a long answer either \
         goes out as one oversized message or falls back to a wall."
    );
    assert!(
        flat_src.contains("content(&chunks[0])"),
        "page 0 is no longer what gets posted in-channel: re-check that a \
         long answer still posts a summary plus a pager (AC-020)"
    );
    assert!(
        flat_src.contains("pager_row("),
        "the pager row is no longer attached to page 0, so there is nothing \
         to press and the rest of the answer is unreachable (AC-020/AC-021)"
    );
    assert!(
        flat_src.contains("store_long_answer("),
        "the later pages are no longer stored, so the pager would have \
         nothing to serve on press (AC-020)"
    );
}

/// The in-channel chunk loop must stay inside the thread branch, and the
/// thread branch must exclude a paged answer. Together those two facts are
/// what stop a long answer from becoming a wall in the channel.
#[test]
fn the_pager_and_the_auto_thread_path_are_mutually_exclusive() {
    let src = handler_src();
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("auto_thread=!paged"),
        "the auto-thread condition no longer excludes a paged answer, so a \
         long answer would post page 0 AND then the full body again (AC-020)"
    );

    // Positional: the loop that posts every chunk must sit inside the thread
    // branch. `!paged` guards that branch, so this is what keeps the loop
    // unreachable for a paged answer.
    let thread_guard = src.find("if auto_thread {").expect(
        "the auto-thread branch is gone: re-check that the chunk loop is still \
         unreachable for a paged answer (AC-020)",
    );
    let chunk_loop = src.find("for chunk in &chunks").expect(
        "the chunk loop moved or was renamed: re-check which branch posts the \
         remaining chunks (AC-020)",
    );
    assert!(
        chunk_loop > thread_guard,
        "the chunk loop now runs outside the auto-thread branch, so a long \
         answer can be posted as a wall of consecutive messages (AC-020)"
    );
}

/// The scan must be able to fail. A path typo or a rename would make the
/// tests above pass while checking nothing, which reports safety it never
/// verified.
#[test]
fn the_scan_sees_the_send_path_it_governs() {
    let src = handler_src();
    assert!(
        src.len() > 10_000,
        "handler.rs came back suspiciously small ({} bytes): the scan is \
         reading the wrong file",
        src.len()
    );
    let flat_src = flat(&src);
    for needle in ["writes::send(", "writes::edit("] {
        assert!(
            flat_src.contains(needle),
            "handler.rs no longer calls {needle}, so this scan is looking at \
             a file that no longer owns the send path"
        );
    }
}
