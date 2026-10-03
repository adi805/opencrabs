//! Discord outbound guards (C3)
//!
//! Pure pre-send checks so a payload Discord would reject with a 400 is
//! refused locally with a message that names the fix. Both the tool path
//! (`discord_send send_file`) and the handler's media-gallery batching go
//! through here, so the two cannot drift apart.
//!
//! Limits are quoted from the primary source, discord-api-docs, verified
//! 2026-10-03:
//!
//! | Limit | Value | Source |
//! |---|---|---|
//! | Max request size when sending a message | 25 MiB | `resources/message.mdx:1074`, `resources/channel.mdx:659` |
//! | Per-attachment upload limit (default) | 20 MiB | `reference.mdx:438` "The default limit is `20 MiB` for all users" |
//! | Items in one media gallery | 1 to 10 | `components/reference.mdx:1743` |
//!
//! Two notes on provenance, because the numbers are not all equally firm:
//!
//! * The 25 MiB request ceiling and the 20 MiB per-attachment default are
//!   stated outright in the docs. The 20 MiB is a *default*: Nitro and Boost
//!   raise it per guild, and an app cannot know the raised value without the
//!   interaction payload's `attachment_size_limit`. So a file between 20 and
//!   25 MiB is refused here even though some guilds would accept it. That is
//!   the conservative direction: a false refusal costs a retry, a false pass
//!   costs a 400 the caller cannot explain.
//! * The raw `files[n]` per-message count cap is **not** written in the API
//!   reference tables (`message.mdx`, `channel.mdx`, `reference.mdx` all read).
//!   The documented anchor is the media-gallery component's "1 to 10 media
//!   gallery items", which is the surface the handler actually builds, and 10
//!   is what the handler already batched at before this guard existed. The
//!   value is kept rather than invented; it is tagged here as gallery-anchored,
//!   not as a documented `files[n]` cap.

/// Max files attached to one message.
pub const MAX_FILES_PER_MESSAGE: usize = 10;
/// Max total request bytes for one message-send call.
pub const REQUEST_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// Max bytes for a single attachment, at the documented default tier.
pub const ATTACHMENT_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// One outbound file, as far as the guards care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSize {
    pub name: String,
    pub bytes: u64,
}

impl FileSize {
    pub fn new(name: impl Into<String>, bytes: u64) -> Self {
        Self {
            name: name.into(),
            bytes,
        }
    }
}

/// Why an outbound batch was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// More than [`MAX_FILES_PER_MESSAGE`] files in one message.
    TooManyFiles { count: usize, max: usize },
    /// One file is past the per-attachment ceiling.
    AttachmentTooLarge { name: String, bytes: u64, max: u64 },
    /// The batch's files together are past the request ceiling.
    RequestTooLarge { bytes: u64, max: u64 },
}

impl GuardError {
    /// Agent-facing message. Kept separate from the display impl so the tool
    /// arm can wrap it in `ToolResult::error` without a `String` round trip.
    pub fn message(&self) -> String {
        match self {
            Self::TooManyFiles { count, max } => format!(
                "Discord accepts at most {max} files per message; this batch has {count}. \
                 Split it into smaller batches."
            ),
            Self::AttachmentTooLarge { name, bytes, max } => format!(
                "File '{name}' is {bytes} bytes, over Discord's {max}-byte per-attachment limit \
                 (the default tier; Nitro/Boost can raise it, but this build cannot see the \
                 raised value). Send a smaller file or a link instead."
            ),
            Self::RequestTooLarge { bytes, max } => format!(
                "This batch totals {bytes} bytes, over Discord's {max}-byte request limit for one \
                 message. Send fewer or smaller files."
            ),
        }
    }
}

/// Refuse a batch Discord would reject.
///
/// Checked in the order the caller can act on: too many files first (split the
/// batch), then a single oversized file (shrink that file), then the total
/// (send fewer).
pub fn check_batch(files: &[FileSize]) -> Result<(), GuardError> {
    if files.len() > MAX_FILES_PER_MESSAGE {
        return Err(GuardError::TooManyFiles {
            count: files.len(),
            max: MAX_FILES_PER_MESSAGE,
        });
    }
    for f in files {
        if f.bytes > ATTACHMENT_MAX_BYTES {
            return Err(GuardError::AttachmentTooLarge {
                name: f.name.clone(),
                bytes: f.bytes,
                max: ATTACHMENT_MAX_BYTES,
            });
        }
    }
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    if total > REQUEST_MAX_BYTES {
        return Err(GuardError::RequestTooLarge {
            bytes: total,
            max: REQUEST_MAX_BYTES,
        });
    }
    Ok(())
}

/// A batch plan: how to split a file list into sendable messages.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct BatchPlan {
    /// Index groups, in order, each satisfying both ceilings.
    pub batches: Vec<Vec<usize>>,
    /// Indices of files that can never be sent as-is (over the per-attachment
    /// ceiling). Reported separately so the caller logs them loudly instead of
    /// silently dropping them into a batch that would 400.
    pub oversized: Vec<usize>,
}

impl BatchPlan {
    /// Total files that will actually be sent.
    pub fn sent_count(&self) -> usize {
        self.batches.iter().map(Vec::len).sum()
    }
}

/// Split files into batches that each fit the count and byte ceilings.
///
/// Greedy in order, because a report's attachments read top to bottom and the
/// caller's order is the priority order. A file that alone exceeds the
/// per-attachment ceiling goes to [`BatchPlan::oversized`] rather than being
/// placed in a batch that is guaranteed to fail.
pub fn plan_batches(files: &[FileSize]) -> BatchPlan {
    let mut plan = BatchPlan::default();
    let mut current: Vec<usize> = Vec::new();
    let mut current_bytes: u64 = 0;

    for (i, f) in files.iter().enumerate() {
        if f.bytes > ATTACHMENT_MAX_BYTES {
            plan.oversized.push(i);
            continue;
        }
        let would_exceed_count = current.len() >= MAX_FILES_PER_MESSAGE;
        let would_exceed_bytes = current_bytes + f.bytes > REQUEST_MAX_BYTES;
        if !current.is_empty() && (would_exceed_count || would_exceed_bytes) {
            plan.batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(i);
        current_bytes += f.bytes;
    }
    if !current.is_empty() {
        plan.batches.push(current);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(sizes: &[u64]) -> Vec<FileSize> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, b)| FileSize::new(format!("f{i}.png"), *b))
            .collect()
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn ten_small_files_pass() {
        assert_eq!(check_batch(&files(&[MIB; 10])), Ok(()));
    }

    #[test]
    fn eleven_files_are_refused_by_the_count_cap() {
        assert_eq!(
            check_batch(&files(&[MIB; 11])),
            Err(GuardError::TooManyFiles { count: 11, max: 10 })
        );
    }

    #[test]
    fn empty_batch_passes() {
        assert_eq!(check_batch(&[]), Ok(()));
    }

    #[test]
    fn a_file_over_the_per_attachment_ceiling_is_refused_by_name() {
        let batch = vec![
            FileSize::new("ok.png", MIB),
            FileSize::new("huge.bin", ATTACHMENT_MAX_BYTES + 1),
        ];
        match check_batch(&batch) {
            Err(GuardError::AttachmentTooLarge { name, bytes, max }) => {
                assert_eq!(name, "huge.bin");
                assert_eq!(bytes, ATTACHMENT_MAX_BYTES + 1);
                assert_eq!(max, ATTACHMENT_MAX_BYTES);
            }
            other => panic!("expected AttachmentTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn a_file_exactly_at_the_per_attachment_ceiling_passes() {
        // The boundary: the docs' limit is inclusive, so exactly 20 MiB is fine.
        assert_eq!(
            check_batch(&[FileSize::new("edge.bin", ATTACHMENT_MAX_BYTES)]),
            Ok(())
        );
    }

    #[test]
    fn total_exactly_at_the_request_ceiling_passes() {
        // 5 x 5 MiB = 25 MiB exactly, which is the documented ceiling.
        let batch = files(&[5 * MIB; 5]);
        assert_eq!(
            batch.iter().map(|f| f.bytes).sum::<u64>(),
            REQUEST_MAX_BYTES
        );
        assert_eq!(check_batch(&batch), Ok(()));
    }

    #[test]
    fn one_byte_over_the_request_ceiling_is_refused() {
        // 5 x 5 MiB is at the line; 4 MiB + 1 byte pushes it over while every
        // individual file stays under the per-attachment ceiling.
        let batch = vec![
            FileSize::new("a.bin", 20 * MIB),
            FileSize::new("b.bin", 4 * MIB),
            FileSize::new("c.bin", 1 * MIB + 1),
        ];
        assert!(batch.iter().all(|f| f.bytes <= ATTACHMENT_MAX_BYTES));
        match check_batch(&batch) {
            Err(GuardError::RequestTooLarge { bytes, max }) => {
                assert_eq!(bytes, 25 * MIB + 1);
                assert_eq!(max, REQUEST_MAX_BYTES);
            }
            other => panic!("expected RequestTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn count_cap_is_reported_before_the_size_ceiling() {
        // A batch that breaks both should name the actionable one first:
        // splitting fixes the count, and the size then follows.
        let batch = files(&[ATTACHMENT_MAX_BYTES; 11]);
        assert!(matches!(
            check_batch(&batch),
            Err(GuardError::TooManyFiles { .. })
        ));
    }

    #[test]
    fn plan_splits_on_the_count_cap() {
        let plan = plan_batches(&files(&[MIB; 23]));
        assert_eq!(plan.batches.len(), 3);
        assert_eq!(plan.batches[0].len(), 10);
        assert_eq!(plan.batches[1].len(), 10);
        assert_eq!(plan.batches[2].len(), 3);
        assert_eq!(plan.sent_count(), 23);
        assert!(plan.oversized.is_empty());
        // Order is preserved: the plan is the input order, regrouped.
        let flat: Vec<usize> = plan.batches.iter().flatten().copied().collect();
        assert_eq!(flat, (0..23).collect::<Vec<_>>());
    }

    #[test]
    fn plan_splits_on_the_byte_ceiling_before_hitting_ten_files() {
        // 3 x 10 MiB = 30 MiB, so a batch of three would be over 25 MiB even
        // though it is under the file count. Two per batch fits.
        let plan = plan_batches(&files(&[10 * MIB; 3]));
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(plan.batches[0], vec![0, 1]);
        assert_eq!(plan.batches[1], vec![2]);
        for batch in &plan.batches {
            let bytes: u64 = batch.iter().map(|i| 10 * MIB).sum();
            assert!(
                bytes <= REQUEST_MAX_BYTES,
                "batch {batch:?} = {bytes} bytes"
            );
        }
    }

    #[test]
    fn plan_passes_an_exact_fit_through_as_one_batch() {
        let plan = plan_batches(&files(&[5 * MIB; 5]));
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0].len(), 5);
    }

    #[test]
    fn plan_isolates_an_oversized_file_instead_of_breaking_the_batch() {
        let plan = plan_batches(&[
            FileSize::new("a.png", MIB),
            FileSize::new("huge.bin", ATTACHMENT_MAX_BYTES + 1),
            FileSize::new("b.png", MIB),
        ]);
        assert_eq!(plan.oversized, vec![1]);
        assert_eq!(plan.batches, vec![vec![0, 2]]);
        assert_eq!(plan.sent_count(), 2);
    }

    #[test]
    fn plan_on_an_all_oversized_input_sends_nothing() {
        let plan = plan_batches(&files(&[
            ATTACHMENT_MAX_BYTES + 1,
            ATTACHMENT_MAX_BYTES + 2,
        ]));
        assert!(plan.batches.is_empty());
        assert_eq!(plan.oversized, vec![0, 1]);
        assert_eq!(plan.sent_count(), 0);
    }

    #[test]
    fn plan_on_empty_input_is_empty() {
        assert_eq!(plan_batches(&[]), BatchPlan::default());
    }

    #[test]
    fn error_messages_name_the_limit_and_the_actual_value() {
        assert_eq!(
            GuardError::TooManyFiles { count: 12, max: 10 }.message(),
            "Discord accepts at most 10 files per message; this batch has 12. \
             Split it into smaller batches."
        );
        let msg = GuardError::RequestTooLarge {
            bytes: 30 * MIB,
            max: REQUEST_MAX_BYTES,
        }
        .message();
        assert!(msg.contains("31457280"), "{msg}");
        assert!(msg.contains("26214400"), "{msg}");
    }
}
