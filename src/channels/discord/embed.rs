//! Discord Embed Spec (C2: multi-embed report layout)
//!
//! Validates and clamps an embed request to what the Discord message API
//! accepts, so a scheduled report can arrive as one message of several blocks
//! instead of a stack of loose posts. The builder call itself is checked at
//! compile time (`CreateMessage::embeds(Vec<CreateEmbed>)`, serenity 0.12.5
//! `builder/create_message.rs`), so everything that can be wrong at runtime
//! lives in the limits below, which is why they are pure functions here and
//! unit-tested.
//!
//! Limits are quoted from the primary source, discord-api-docs
//! `developers/resources/message.mdx` (Embed Limits, verified 2026-10-03) and
//! `developers/resources/channel.mdx:697`:
//!
//! | Limit | Value | Source |
//! |---|---|---|
//! | Embeds per message | up to 10 | channel.mdx "Up to 10 `rich` embeds" |
//! | `title` | 256 chars | message.mdx Embed Limits table |
//! | `description` | 4096 chars | message.mdx Embed Limits table |
//! | Combined text across all embeds | 6000 chars | message.mdx "must not exceed 6000 characters" |
//!
//! The 6000 budget also covers `field.name`, `field.value`, `footer.text`, and
//! `author.name`; this module exposes only title and description, so spending
//! the whole budget on those two can never overflow what Discord counts. That
//! is deliberately conservative: if fields are added later they must come out
//! of the same budget, not sit on top of it.

use serenity::builder::CreateEmbed;

/// Max embeds attached to one message.
pub const MAX_EMBEDS: usize = 10;
/// Max characters in one embed title.
pub const TITLE_MAX: usize = 256;
/// Max characters in one embed description.
pub const DESCRIPTION_MAX: usize = 4096;
/// Max combined title+description characters across all embeds of one message.
pub const TOTAL_TEXT_MAX: usize = 6000;

/// One embed as the caller asked for it, before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedInput {
    pub title: String,
    pub description: String,
    pub color: u32,
}

impl EmbedInput {
    pub fn new(title: impl Into<String>, description: impl Into<String>, color: u32) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            color,
        }
    }

    /// Characters that count against the combined budget, whitespace excluded
    /// (Discord trims leading and trailing whitespace automatically, so
    /// counting it would reject a message the API would accept).
    fn budget_chars(&self) -> usize {
        self.title.trim().chars().count() + self.description.trim().chars().count()
    }

    fn is_blank(&self) -> bool {
        self.title.trim().is_empty() && self.description.trim().is_empty()
    }
}

/// Why an embed request was refused outright, as opposed to clamped.
#[derive(Debug, PartialEq, Eq)]
pub enum EmbedError {
    /// No embed carried any text after trimming. Discord rejects an embed with
    /// no title, description, or fields, so sending one is a guaranteed 400.
    NoContent,
}

impl EmbedError {
    /// Agent-facing message. Kept separate from the display impl so the tool
    /// arm can wrap it in `ToolResult::error` without a `String` round trip.
    pub fn message(&self) -> String {
        match self {
            Self::NoContent => "send_embed requires text: pass 'embed_title'/'embed_description' \
                 or a non-empty 'embeds' array."
                .to_string(),
        }
    }
}

/// An embed request that is safe to hand to the Discord API.
#[derive(Debug, PartialEq, Eq)]
pub struct EmbedSpec {
    /// Trimmed and per-field truncated, in the caller's order.
    pub embeds: Vec<EmbedInput>,
    /// Entries removed: blank ones, ones past [`MAX_EMBEDS`], and ones that did
    /// not fit the [`TOTAL_TEXT_MAX`] budget. Reported back to the caller so a
    /// dropped block is never silent.
    pub dropped_embeds: usize,
    /// True when any text was cut: a title past [`TITLE_MAX`], a description
    /// past [`DESCRIPTION_MAX`], or the combined [`TOTAL_TEXT_MAX`] budget.
    pub truncated: bool,
}

/// Truncate to `max` characters on a `char` boundary.
///
/// Byte slicing would panic on the multibyte input agents actually send
/// (emoji, accented text), so this counts `chars`, not bytes.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Validate and clamp a raw embed request.
///
/// Order is preserved because a report reads top to bottom: the caller puts the
/// most important block first, and when the budget runs out the tail is what
/// gets trimmed and dropped, never the head.
pub fn build_spec(input: &[EmbedInput]) -> Result<EmbedSpec, EmbedError> {
    // Trim and per-field truncate first, so the budget below is computed on
    // exactly the text that would ship. A cut here is a cut worth reporting,
    // so it sets `truncated` too.
    let mut truncated = false;
    let normalized: Vec<EmbedInput> = input
        .iter()
        .filter(|e| !e.is_blank())
        .map(|e| {
            let title = e.title.trim();
            let description = e.description.trim();
            if title.chars().count() > TITLE_MAX || description.chars().count() > DESCRIPTION_MAX {
                truncated = true;
            }
            EmbedInput {
                title: truncate_chars(title, TITLE_MAX),
                description: truncate_chars(description, DESCRIPTION_MAX),
                color: e.color,
            }
        })
        .collect();

    if normalized.is_empty() {
        return Err(EmbedError::NoContent);
    }

    let mut dropped_embeds = input.len() - normalized.len();
    let mut used = 0usize;
    let mut embeds: Vec<EmbedInput> = Vec::new();

    for entry in normalized {
        if embeds.len() >= MAX_EMBEDS {
            dropped_embeds += 1;
            continue;
        }
        let need = entry.budget_chars();
        if used + need <= TOTAL_TEXT_MAX {
            used += need;
            embeds.push(entry);
            continue;
        }
        // Partial fit: keep the title, cut the description to what remains.
        let remaining = TOTAL_TEXT_MAX - used;
        let title_len = entry.title.chars().count();
        if remaining > title_len {
            let mut clipped = entry;
            clipped.description = truncate_chars(&clipped.description, remaining - title_len);
            used += clipped.budget_chars();
            embeds.push(clipped);
        } else {
            dropped_embeds += 1;
        }
        // Everything after this point is past the budget.
        truncated = true;
        break;
    }

    Ok(EmbedSpec {
        embeds,
        dropped_embeds,
        truncated,
    })
}

/// Turn the validated embeds into serenity builders.
///
/// An empty field is omitted rather than sent as `""`: Discord treats an
/// embed with no title, description, or fields as a bad request, and an empty
/// string would otherwise serialise a title the API has no use for.
pub fn embed_builders(spec: &EmbedSpec) -> Vec<CreateEmbed> {
    spec.embeds
        .iter()
        .map(|e| {
            let mut b = CreateEmbed::new().color(e.color);
            if !e.title.is_empty() {
                b = b.title(e.title.as_str());
            }
            if !e.description.is_empty() {
                b = b.description(e.description.as_str());
            }
            b
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLURPLE: u32 = 0x5865F2;

    fn one(title: &str, description: &str) -> Vec<EmbedInput> {
        vec![EmbedInput::new(title, description, BLURPLE)]
    }

    #[test]
    fn empty_input_is_refused() {
        assert_eq!(build_spec(&[]), Err(EmbedError::NoContent));
    }

    #[test]
    fn all_blank_entries_are_refused() {
        // Whitespace-only text is blank: Discord trims it and then has nothing.
        let input = vec![
            EmbedInput::new("", "", BLURPLE),
            EmbedInput::new("   ", "\n\t ", BLURPLE),
        ];
        assert_eq!(build_spec(&input), Err(EmbedError::NoContent));
    }

    #[test]
    fn blank_entries_are_dropped_not_sent() {
        let input = vec![
            EmbedInput::new("Real", "body", BLURPLE),
            EmbedInput::new("", "", BLURPLE),
            EmbedInput::new("  ", "  ", BLURPLE),
        ];
        let spec = build_spec(&input).unwrap();
        assert_eq!(spec.embeds.len(), 1);
        assert_eq!(spec.dropped_embeds, 2);
        assert!(!spec.truncated);
    }

    #[test]
    fn more_than_ten_embeds_is_capped_and_reported() {
        let twelve: Vec<EmbedInput> = (1..=12)
            .map(|i| EmbedInput::new(format!("T{i}"), format!("body {i}"), BLURPLE))
            .collect();
        let spec = build_spec(&twelve).unwrap();
        assert_eq!(spec.embeds.len(), MAX_EMBEDS);
        assert_eq!(spec.dropped_embeds, 2);
        assert_eq!(spec.embeds.first().unwrap().title, "T1");
        assert_eq!(spec.embeds.last().unwrap().title, "T10");
    }

    #[test]
    fn exactly_ten_embeds_is_not_capped() {
        let ten: Vec<EmbedInput> = (1..=10)
            .map(|i| EmbedInput::new(format!("T{i}"), format!("b{i}"), BLURPLE))
            .collect();
        let spec = build_spec(&ten).unwrap();
        assert_eq!(spec.embeds.len(), 10);
        assert_eq!(spec.dropped_embeds, 0);
        assert!(!spec.truncated);
    }

    #[test]
    fn long_title_truncates_to_256_chars() {
        let long = "t".repeat(400);
        let spec = build_spec(&one(&long, "body")).unwrap();
        assert_eq!(spec.embeds[0].title.chars().count(), TITLE_MAX);
        assert!(spec.truncated, "a cut title must be reported");
    }

    #[test]
    fn long_description_truncates_to_4096_chars() {
        let long = "d".repeat(5000);
        let spec = build_spec(&one("T", &long)).unwrap();
        assert_eq!(spec.embeds[0].description.chars().count(), DESCRIPTION_MAX);
        assert!(spec.truncated, "a cut description must be reported");
    }

    #[test]
    fn combined_text_over_6000_trims_the_tail_and_reports() {
        // Three embeds of 4000 description chars each: the first two fit
        // (8000 > 6000 already), so the second gets cut and the third is gone.
        let big = "d".repeat(4000);
        let input = vec![
            EmbedInput::new("A", &big, BLURPLE),
            EmbedInput::new("B", &big, BLURPLE),
            EmbedInput::new("C", &big, BLURPLE),
        ];
        let spec = build_spec(&input).unwrap();
        assert!(spec.truncated, "budget overflow must set truncated");
        let total: usize = spec.embeds.iter().map(EmbedInput::budget_chars).sum();
        assert!(
            total <= TOTAL_TEXT_MAX,
            "total {total} must fit the 6000 budget"
        );
        assert_eq!(spec.embeds[0].title, "A");
        // C never made it in.
        assert!(spec.embeds.iter().all(|e| e.title != "C"));
        assert!(spec.dropped_embeds >= 1);
    }

    #[test]
    fn budget_exact_fit_is_not_truncated() {
        // 6000 = 2000 + 4000 across two embeds, exactly on the line.
        let a = "a".repeat(2000);
        let b = "b".repeat(4000);
        let input = vec![
            EmbedInput::new("", &a, BLURPLE),
            EmbedInput::new("", &b, BLURPLE),
        ];
        let spec = build_spec(&input).unwrap();
        assert!(!spec.truncated);
        assert_eq!(spec.embeds.len(), 2);
        let total: usize = spec.embeds.iter().map(EmbedInput::budget_chars).sum();
        assert_eq!(total, TOTAL_TEXT_MAX);
    }

    #[test]
    fn whitespace_does_not_count_against_the_budget() {
        // Discord trims leading/trailing whitespace, so 6000 spaces around a
        // short body is not an overflow.
        let padded = format!("{}body{}", " ".repeat(3000), " ".repeat(3000));
        let spec = build_spec(&one("T", &padded)).unwrap();
        assert!(!spec.truncated);
        assert_eq!(spec.embeds[0].description, "body");
    }

    #[test]
    fn single_embed_path_still_validates_the_same_way() {
        // The tool builds a one-element input from embed_title/embed_description,
        // so the single path is just the degenerate case of the multi path.
        let spec = build_spec(&one("Title", "Body")).unwrap();
        assert_eq!(spec.embeds.len(), 1);
        assert_eq!(spec.embeds[0].title, "Title");
        assert_eq!(spec.embeds[0].description, "Body");
        assert_eq!(spec.dropped_embeds, 0);
        assert!(!spec.truncated);
    }

    #[test]
    fn truncation_is_char_safe_on_multibyte_input() {
        // 5000 emoji is 20000 bytes; a byte slice would split a code point.
        let d = "🦀".repeat(5000);
        let spec = build_spec(&one("T", &d)).unwrap();
        assert_eq!(spec.embeds[0].description.chars().count(), DESCRIPTION_MAX);
        assert!(spec.embeds[0].description.chars().all(|c| c == '🦀'));
        assert!(std::str::from_utf8(spec.embeds[0].description.as_bytes()).is_ok());
    }

    #[test]
    fn color_passes_through() {
        let spec = build_spec(&[EmbedInput::new("T", "B", 0x00FF00)]).unwrap();
        assert_eq!(spec.embeds[0].color, 0x00FF00);
    }

    #[test]
    fn embed_builders_mirror_the_spec() {
        let spec = build_spec(&[
            EmbedInput::new("First", "one", 0x111111),
            EmbedInput::new("Second", "two", 0x222222),
        ])
        .unwrap();
        let builders = embed_builders(&spec);
        assert_eq!(builders.len(), 2);
        let json = serde_json::to_value(&builders).unwrap();
        assert_eq!(json[0]["title"], serde_json::json!("First"));
        assert_eq!(json[0]["description"], serde_json::json!("one"));
        assert_eq!(json[0]["color"], serde_json::json!(0x111111));
        assert_eq!(json[1]["title"], serde_json::json!("Second"));
    }

    #[test]
    fn empty_field_is_omitted_from_the_wire_payload() {
        let spec = build_spec(&one("Only a title", "")).unwrap();
        let json = serde_json::to_value(&embed_builders(&spec)).unwrap();
        assert_eq!(json[0]["title"], serde_json::json!("Only a title"));
        assert_eq!(json[0]["color"], serde_json::json!(BLURPLE));
        assert!(
            json[0].get("description").is_none(),
            "an empty description must not ship as \"\": {json}"
        );
    }

    #[test]
    fn spec_serialises_into_a_multi_embed_message() {
        use serenity::builder::CreateMessage;
        let spec = build_spec(&[
            EmbedInput::new("Report", "line one", BLURPLE),
            EmbedInput::new("Detail", "line two", BLURPLE),
        ])
        .unwrap();
        let msg = CreateMessage::new().embeds(embed_builders(&spec));
        let json = serde_json::to_value(&msg).unwrap();
        let embeds = json["embeds"].as_array().unwrap();
        assert_eq!(embeds.len(), 2);
        assert_eq!(embeds[0]["title"], serde_json::json!("Report"));
        assert_eq!(embeds[1]["title"], serde_json::json!("Detail"));
    }

    #[test]
    fn error_message_names_the_params() {
        assert_eq!(
            EmbedError::NoContent.message(),
            "send_embed requires text: pass 'embed_title'/'embed_description' or a non-empty \
             'embeds' array."
        );
    }
}
