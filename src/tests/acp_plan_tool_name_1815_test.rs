//! F6 of #1815: the ACP `plan` session update and the machine-readable tool
//! identity on `tool_call` frames.
//!
//! Two claims are pinned here, both from `schema/v1/schema.json`:
//!
//! - `ToolCall` requires `toolCallId` + `title` and carries an optional stable
//!   `name`. The bridge used to send only the title, so a client could not
//!   recognize which tool a card belonged to.
//! - `Plan` requires `entries`, and each `PlanEntry` requires `content`,
//!   `priority` and `status`, where `status` allows exactly `pending`,
//!   `in_progress` and `completed`. Our plan has six task states, so the
//!   collapse is asserted AND the lost distinction is required to survive in
//!   `_meta` (the only place v1 lets an implementation add information without
//!   inventing reserved vocabulary).

use crate::acp::protocol;
use crate::acp::turn::{plan_entries, tool_call_frame, tool_call_update_frame};
use crate::tui::plan::PlanDocument;

/// Build a real plan document through the store's own deserializer, so these
/// tests break if the persisted shape changes rather than drifting with it.
fn doc(tasks: &str) -> PlanDocument {
    let raw = format!(r#"{{"title":"Ship it","description":"","tasks":{tasks}}}"#);
    serde_json::from_str::<PlanDocument>(&raw).expect("plan document parses")
}

fn one_task(status: &str, complexity: u8) -> PlanDocument {
    doc(&format!(
        r#"[{{"title":"Wire the header","description":"do the thing","task_type":"edit","status":{status},"complexity":{complexity}}}]"#
    ))
}

// ---------------------------------------------------------------- `name`

#[test]
fn tool_call_frame_sends_both_identities() {
    let frame = tool_call_frame(
        "call-1",
        "analyze_image",
        &serde_json::json!({"image": "x"}),
    );
    assert_eq!(frame["sessionUpdate"], "tool_call");
    assert_eq!(frame["toolCallId"], "call-1");
    assert_eq!(
        frame["title"], "analyze_image",
        "title is the display label the client shows"
    );
    assert_eq!(
        frame["name"], "analyze_image",
        "name is the stable identity the client keys on (#1815 F6)"
    );
    assert_eq!(frame["status"], "in_progress");
}

#[test]
fn tool_call_update_frame_keeps_the_machine_name_and_maps_status() {
    let ok = tool_call_update_frame("call-1", "plan", true, "Plan saved");
    assert_eq!(ok["sessionUpdate"], "tool_call_update");
    assert_eq!(ok["name"], "plan");
    assert_eq!(ok["status"], "completed");

    let bad = tool_call_update_frame("call-2", "bash", false, "exit 2");
    assert_eq!(bad["name"], "bash");
    assert_eq!(bad["status"], "failed");
}

// ------------------------------------------------------------ `plan` frame

#[test]
fn plan_frame_is_always_a_full_snapshot() {
    let empty = protocol::plan_update(vec![]);
    assert_eq!(empty["sessionUpdate"], "plan");
    assert!(
        empty["entries"].is_array(),
        "`entries` is the only required field and must be present even when the \
         plan is gone, otherwise the client keeps a stale card"
    );
    assert_eq!(empty["entries"].as_array().unwrap().len(), 0);
}

#[test]
fn every_task_is_emitted_because_the_client_replaces_the_whole_plan() {
    let document = doc(
        r#"[{"title":"one","description":"","task_type":"edit","status":"InProgress","complexity":3},
            {"title":"two","description":"","task_type":"test","status":"Pending","complexity":1}]"#,
    );
    let entries = plan_entries(&document);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["content"], "one");
    assert_eq!(entries[1]["content"], "two");
}

#[test]
fn only_the_three_v1_statuses_are_ever_emitted() {
    const V1: &[&str] = &["pending", "in_progress", "completed"];
    for status in [
        r#""Pending""#,
        r#""InProgress""#,
        r#""Completed""#,
        r#""Skipped""#,
        r#""Failed""#,
        r#"{"Blocked":"waiting on the gate"}"#,
    ] {
        let entries = plan_entries(&one_task(status, 3));
        let emitted = entries[0]["status"].as_str().unwrap_or("<missing>");
        assert!(
            V1.contains(&emitted),
            "`{status}` mapped to `{emitted}`, which is not a v1 PlanEntryStatus"
        );
    }
}

#[test]
fn done_and_running_survive_and_the_rest_collapse_to_pending() {
    let completed = plan_entries(&one_task(r#""Completed""#, 3))[0]["status"].clone();
    assert_eq!(completed, "completed");
    let running = plan_entries(&one_task(r#""InProgress""#, 3))[0]["status"].clone();
    assert_eq!(running, "in_progress");
    for status in [r#""Pending""#, r#""Skipped""#, r#""Failed""#] {
        let entries = plan_entries(&one_task(status, 3));
        assert_eq!(entries[0]["status"], "pending", "for {status}");
    }
    let blocked = plan_entries(&one_task(r#"{"Blocked":"no receipt yet"}"#, 2));
    assert_eq!(blocked[0]["status"], "pending");
}

#[test]
fn a_collapsed_state_is_not_a_lie_because_its_truth_goes_in_meta() {
    let skipped = plan_entries(&one_task(r#""Skipped""#, 3))[0].clone();
    assert_eq!(skipped["_meta"]["status"], "Skipped");

    let failed = plan_entries(&one_task(r#""Failed""#, 3))[0].clone();
    assert_eq!(failed["status"], "pending");
    assert_eq!(
        failed["_meta"]["status"], "Failed",
        "the client that only reads `status` sees 'not done'; the one that reads \
         `_meta` sees why (#1815 F6)"
    );

    let blocked = plan_entries(&one_task(r#"{"Blocked":"deps"}"#, 3))[0].clone();
    assert_eq!(blocked["_meta"]["status"], "Blocked: deps");
}

#[test]
fn priority_is_the_closest_thing_we_have_and_the_real_number_stays_visible() {
    let high = plan_entries(&one_task(r#""Pending""#, 5))[0].clone();
    assert_eq!(high["priority"], "high");
    assert_eq!(high["_meta"]["complexity"], 5);

    let medium = plan_entries(&one_task(r#""Pending""#, 3))[0].clone();
    assert_eq!(medium["priority"], "medium");

    let low = plan_entries(&one_task(r#""Pending""#, 1))[0].clone();
    assert_eq!(low["priority"], "low");

    // v1 `PlanEntryPriority` is exactly these three strings.
    for entry in [high, medium, low] {
        let p = entry["priority"].as_str().unwrap_or("<missing>");
        assert!(
            ["high", "medium", "low"].contains(&p),
            "`{p}` is not a v1 priority"
        );
    }
}

#[test]
fn an_untitled_task_falls_back_to_its_description() {
    let document = doc(
        r#"[{"title":"","description":"Gate the candidates","task_type":"edit","status":"Pending","complexity":2}]"#,
    );
    let entries = plan_entries(&document);
    assert_eq!(entries[0]["content"], "Gate the candidates");
}

#[test]
fn each_entry_carries_every_required_plan_entry_field() {
    let entries = plan_entries(&one_task(r#""InProgress""#, 4));
    for field in ["content", "priority", "status"] {
        assert!(
            !entries[0][field].is_null(),
            "`{field}` is required on PlanEntry and is missing"
        );
    }
}
