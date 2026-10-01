//! Collapsible tool-call group for Discord (#380), matching the Telegram
//! block and the Slack Block Kit port: ONE message per turn, collapsed to
//! a live summary with an Expand button, toggled in place via component
//! interaction. State lives in [`super::DiscordState`] keyed by message id
//! so the click handler can re-render after the turn's closures are gone.
//! Expansion is per-message: everyone in the channel shares it.
//!
//! With `trace_narration` enabled the same bubble also carries the turn's
//! intermediate narration as dim subtext notes (agent-disco-style live
//! trace): one editable work-log per turn instead of one message per
//! intermediate.

use serenity::builder::{CreateActionRow, CreateButton};
use serenity::model::application::ButtonStyle;
use std::time::{Duration, Instant};

use super::DiscordState;

/// One tool row in a group.
#[derive(Debug, Clone)]
pub(crate) struct GroupEntry {
    pub name: String,
    pub context: String,
    /// None = running, Some(success) = finished.
    pub status: Option<bool>,
}

/// A turn's tool group: contents plus display state.
#[derive(Debug, Clone)]
pub(crate) struct GroupState {
    pub entries: Vec<GroupEntry>,
    /// Narration lines folded into the bubble (live trace). Authoritative
    /// state lives in [`DiscordState`]; only [`DiscordState::append_note`]
    /// and [`DiscordState::drop_note_if`] mutate them —
    /// [`DiscordState::upsert_tool_group`] preserves the stored notes the
    /// way it preserves `expanded`.
    pub notes: Vec<String>,
    pub expanded: bool,
    /// Turn-start anchor for the live `🕒` segment and the settled `⏱` one
    /// (#1841). Stamped at first insert and preserved across updates so the
    /// clock never restarts mid-turn.
    pub started_at: Instant,
    /// Post-delivery status (#1841). `None` while the turn is live; stamped
    /// once by [`DiscordState::settle_tool_group`] and preserved by every
    /// later upsert. `elapsed` freezes at settle so toggling Expand later
    /// never grows the clock.
    pub settled: Option<SettledStatus>,
}

/// Frozen post-delivery chrome (#1841): the Discord twin of Slack's
/// `SettledStatus`. The clock stops at settle and the ctx budget line moves
/// into the flow group, so the chrome owns it (the answer-message footer
/// goes away in #1842, leaving the settled line as the single home).
#[derive(Debug, Clone)]
pub(crate) struct SettledStatus {
    pub elapsed: Duration,
    pub ctx: Option<String>,
}

/// Keep at most this many narration lines in the bubble (newest win).
pub(crate) const NOTE_CAP: usize = 6;

/// Clip each narration line to this many chars — the bubble stays a glance,
/// not a transcript.
pub(crate) const NOTE_MAX_CHARS: usize = 160;

/// First non-empty line, trimmed to [`NOTE_MAX_CHARS`] — the bubble form of
/// one narration event.
pub(crate) fn clip_note(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(NOTE_MAX_CHARS).collect();
    if line.chars().count() > NOTE_MAX_CHARS {
        out.push('…');
    }
    out
}

/// Narration lines as Discord subtext (`-# ` renders dim and small).
fn notes_block(notes: &[String]) -> String {
    notes
        .iter()
        .map(|n| format!("-# {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_icon(status: Option<bool>) -> &'static str {
    match status {
        None => "⚙️",
        Some(true) => "✅",
        Some(false) => "❌",
    }
}

/// `M:SS` elapsed clock (`H:MM:SS` past an hour), the Discord twin of
/// Telegram's flow clock. The glyph lives with the caller so the live and
/// settled segments can differ (`🕒` rolls, `⏱️` freezes at settle).
fn clock(elapsed: Duration) -> String {
    let (h, m, s) = (
        elapsed.as_secs() / 3600,
        (elapsed.as_secs() % 3600) / 60,
        elapsed.as_secs() % 60,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn summary_line(group: &GroupState) -> String {
    let n = group.entries.len();
    let running = group.entries.iter().filter(|e| e.status.is_none()).count();
    let failed = group
        .entries
        .iter()
        .filter(|e| e.status == Some(false))
        .count();
    let (icon, tail) = if running > 0 {
        ("⚙️", format!(" · {running} running"))
    } else if failed > 0 {
        ("❌", format!(" · {failed} failed"))
    } else {
        ("✅", String::new())
    };
    let counts = format!("**{n} tool call{}**", if n == 1 { "" } else { "s" });
    match &group.settled {
        Some(s) => {
            // Settled chrome (#1841): frozen clock, ctx budget as the last
            // word before it, mirroring the Telegram settled header order.
            let mut line = format!("{icon} {counts}{tail}");
            if let Some(ctx) = &s.ctx {
                line.push_str(&format!(" · {ctx}"));
            }
            line.push_str(&format!(" · ⏱️ {}", clock(s.elapsed)));
            line
        }
        None => format!(
            "{icon} {counts}{tail} · 🕒 {}",
            clock(group.started_at.elapsed())
        ),
    }
}

/// Message body for the group in its current display state.
pub(crate) fn render_content(group: &GroupState) -> String {
    let tools_part = if group.entries.len() == 1 && !group.expanded {
        let e = &group.entries[0];
        format!("{} **{}**{}", entry_icon(e.status), e.name, e.context)
    } else if group.expanded {
        let lines: Vec<String> = group
            .entries
            .iter()
            .map(|e| format!("{} **{}**{}", entry_icon(e.status), e.name, e.context))
            .collect();
        format!("{}\n{}", summary_line(group), lines.join("\n"))
    } else {
        summary_line(group)
    };
    if group.notes.is_empty() {
        tools_part
    } else {
        format!("{tools_part}\n{}", notes_block(&group.notes))
    }
}

/// Toggle components for the group message; empty for single-tool groups
/// (a lone line has nothing extra to reveal).
pub(crate) fn render_components(group: &GroupState, message_id: u64) -> Vec<CreateActionRow> {
    if group.entries.len() < 2 {
        return Vec::new();
    }
    let label = if group.expanded {
        "Collapse ▲"
    } else {
        "Expand ▼"
    };
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("toolgroup:{message_id}"))
            .label(label)
            .style(ButtonStyle::Secondary),
    ])]
}

impl DiscordState {
    /// Retained tool groups; older ones stop being toggleable (their last
    /// rendered state stays on screen, like Telegram's frozen blocks).
    const TOOL_GROUP_CAP: usize = 20;

    /// Insert or update a group, PRESERVING the stored expanded/collapsed
    /// choice on updates (a completing tool must not snap an expanded group
    /// shut) and the stored narration notes (only `append_note`/`drop_note_if`
    /// mutate those). Returns the stored state so callers render what is kept.
    pub(crate) async fn upsert_tool_group(
        &self,
        message_id: u64,
        mut group: GroupState,
    ) -> GroupState {
        let mut guard = self.tool_groups.lock().await;
        let (order, map) = &mut *guard;
        match map.get(&message_id) {
            Some(existing) => {
                group.expanded = existing.expanded;
                group.notes = existing.notes.clone();
                group.started_at = existing.started_at;
                group.settled = existing.settled.clone();
            }
            None => {
                order.push(message_id);
                while order.len() > Self::TOOL_GROUP_CAP {
                    let oldest = order.remove(0);
                    map.remove(&oldest);
                }
            }
        }
        map.insert(message_id, group.clone());
        group
    }

    /// Flip a group's expanded state; None when it aged out of retention.
    pub(crate) async fn toggle_tool_group(&self, message_id: u64) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        group.expanded = !group.expanded;
        Some(group.clone())
    }

    /// Append one narration line to the stored group, keeping only the
    /// newest [`NOTE_CAP`]. Returns the updated state, or None when the
    /// message has no stored group (aged out of retention).
    pub(crate) async fn append_note(&self, message_id: u64, note: String) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        group.notes.push(note);
        if group.notes.len() > NOTE_CAP {
            group.notes.remove(0);
        }
        Some(group.clone())
    }

    /// Stamp the post-delivery status (#1841): freeze the clock at now and
    /// record the ctx budget line for the settled chrome. A `None` ctx keeps
    /// whatever a previous settle stamped, so a re-settle never clears the
    /// budget. Returns the updated state, or None when the message has no
    /// stored group (aged out of retention).
    pub(crate) async fn settle_tool_group(
        &self,
        message_id: u64,
        ctx: Option<String>,
    ) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        let prev_ctx = group.settled.as_ref().and_then(|s| s.ctx.clone());
        group.settled = Some(SettledStatus {
            elapsed: group.started_at.elapsed(),
            ctx: ctx.or(prev_ctx),
        });
        Some(group.clone())
    }

    /// Clone the live or settled group for out-of-loop renderers (#1843):
    /// the flow ticker snapshots under the lock, renders outside it, and
    /// re-snapshots after its edit so the settled line keeps the last word.
    pub(crate) async fn tool_group_snapshot(&self, message_id: u64) -> Option<GroupState> {
        let guard = self.tool_groups.lock().await;
        let (_, map) = &*guard;
        map.get(&message_id).cloned()
    }

    /// Remove the LAST narration line matching `pred` — the final-response
    /// dedup drops the trailing note that mirrors the answer, so the trace
    /// does not double-post it as a clip. Returns the updated state, or None
    /// when nothing matched or no group is stored.
    pub(crate) async fn drop_note_if<F>(&self, message_id: u64, pred: F) -> Option<GroupState>
    where
        F: Fn(&str) -> bool,
    {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        let idx = group.notes.iter().rposition(|n| pred(n))?;
        group.notes.remove(idx);
        Some(group.clone())
    }
}
