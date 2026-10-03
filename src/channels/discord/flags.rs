//! Discord message-flag helpers (C1: silent delivery).
//!
//! `SUPPRESS_NOTIFICATIONS` is a create-time message flag documented to post
//! the message without triggering push/desktop notifications (the recipient
//! still sees the unread badge). That is the right shape for scheduled and
//! report output and the wrong shape for a conversational reply, so the
//! decision is a parameter here rather than a hard-coded builder call.
//!
//! Flags are quoted from the primary source, discord-api-docs
//! `developers/resources/message.mdx` (message flags table) and serenity
//! 0.12.5 `model/channel/message.rs:1245-1282`, verified 2026-10-02:
//!
//! | Flag | Value | Meaning |
//! |---|---|---|
//! | `SUPPRESS_EMBEDS` | 1 << 2 | Do not render embeds on this message |
//! | `SUPPRESS_NOTIFICATIONS` | 1 << 12 | No push/desktop notification (badge only) |
//! | `IS_VOICE_MESSAGE` | 1 << 13 | Message is a voice message |
//!
//! Only `SUPPRESS_NOTIFICATIONS` is exposed here. The others need a shape this
//! module does not own (an embed-less render path, the voice-message contract),
//! and attaching them speculatively would change unrelated messages.

use serenity::builder::CreateMessage;
use serenity::model::channel::MessageFlags;

/// The raw numeric value of `SUPPRESS_NOTIFICATIONS`, for the delivery paths
/// that build their JSON body by hand instead of through a serenity builder
/// (`cron::scheduler::deliver_discord`). Kept as one constant so the raw path
/// and the builder path cannot drift; a test pins it against
/// `MessageFlags::SUPPRESS_NOTIFICATIONS`.
pub const SUPPRESS_NOTIFICATIONS_BITS: u64 = 1 << 12;

/// Resolve whether an outbound message should suppress notifications.
///
/// An explicit caller choice (the `silent` tool param) wins over the channel
/// default, so one scheduled job can stay loud on a quiet channel and a chatty
/// channel can be muted without touching every call site.
pub fn resolve_silent(explicit: Option<bool>, channel_default: bool) -> bool {
    explicit.unwrap_or(channel_default)
}

/// Build the create-time flag set for a message, or `None` when no flag
/// applies.
///
/// Returning `None` rather than an empty flag set keeps callers on the plain
/// builder path: a no-op `flags(MessageFlags::empty())` serialises to
/// `"flags": 0`, which the API accepts but which hides the intent and would
/// make every message carry a flags field.
pub fn create_flags(silent: bool) -> Option<MessageFlags> {
    silent.then_some(MessageFlags::SUPPRESS_NOTIFICATIONS)
}

/// Apply the resolved silent choice to a `CreateMessage` builder.
pub fn apply_silent(msg: CreateMessage, silent: bool) -> CreateMessage {
    match create_flags(silent) {
        Some(flags) => msg.flags(flags),
        None => msg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_choice_beats_the_channel_default() {
        assert!(resolve_silent(Some(true), false));
        assert!(!resolve_silent(Some(false), true));
    }

    #[test]
    fn absent_choice_falls_back_to_the_channel_default() {
        assert!(resolve_silent(None, true));
        assert!(!resolve_silent(None, false));
    }

    #[test]
    fn silent_maps_to_suppress_notifications() {
        let flags = create_flags(true).expect("silent must produce a flag");
        assert!(flags.contains(MessageFlags::SUPPRESS_NOTIFICATIONS));
        // Assert the numeric bit, not just the variant: a serenity renumbering
        // (or a wrong variant picked from the flags table) would still pass a
        // contains() check against itself but ship the wrong wire value.
        assert_eq!(flags.bits(), 1 << 12);
    }

    #[test]
    fn raw_bits_constant_matches_the_serenity_variant() {
        // The cron delivery path hand-writes `"flags": SUPPRESS_NOTIFICATIONS_BITS`
        // into its JSON body; if serenity ever renumbers the variant, this fails
        // here instead of silently shipping a wrong bit on every scheduled post.
        assert_eq!(
            SUPPRESS_NOTIFICATIONS_BITS,
            MessageFlags::SUPPRESS_NOTIFICATIONS.bits()
        );
    }

    #[test]
    fn loud_produces_no_flags() {
        assert!(create_flags(false).is_none());
    }

    #[test]
    fn apply_silent_sets_the_flags_field_and_loud_omits_it() {
        let silent = apply_silent(CreateMessage::new().content("x"), true);
        let json = serde_json::to_value(&silent).unwrap();
        assert!(
            json.get("flags").is_some(),
            "silent must serialise a flags field: {json}"
        );

        let loud = apply_silent(CreateMessage::new().content("x"), false);
        let json = serde_json::to_value(&loud).unwrap();
        assert!(
            json.get("flags").is_none(),
            "loud must omit the flags field: {json}"
        );
    }

    #[test]
    fn apply_silent_preserves_the_content() {
        let msg = apply_silent(CreateMessage::new().content("hello"), true);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["content"], serde_json::json!("hello"));
    }
}
