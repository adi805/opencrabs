//! Deterministic UI snapshots of the REAL TUI render path.
//!
//! Why this exists: three separate gaps left this repo unable to show what the
//! UI actually renders on Windows. `src/assets/*.png` are hand-made upstream
//! screenshots; the Windows CI job only runs `cargo build`, never the tests; and
//! screen-scraping a console window needs an interactive desktop a hosted runner
//! does not reliably provide.
//!
//! This sidesteps all three by driving the real entry points -
//! `crate::tui::render::render`, `App::switch_mode` and
//! `App::open_usage_dashboard` - into ratatui's `TestBackend`, then writing each
//! frame twice:
//!
//! ```text
//! target/ui-dump/<name>.txt     character grid, one line per row
//! target/ui-dump/<name>.spans   style runs (fg/bg/modifier), so colour survives
//! ```
//!
//! The `.spans` file is run-length encoded per row: `SPAN <y> <x> <len> <fg>
//! <bg> <modifier> <text>`. Colours are written as `rgb:rrggbb`, `idx:N`,
//! `reset`, or the lowercased `Debug` of any other variant, so a renderer needs
//! no knowledge of the palette.
//!
//! These are `#[ignore]`d so the gating Test job stays fast and unaffected; the
//! Windows artifact workflow asks for them explicitly with `--ignored`.
//!
//! Run locally:
//! `cargo test --all-features --lib ui_snapshot_dump -- --ignored --nocapture`

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use uuid::Uuid;

use crate::brain::agent::service::AgentService;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use crate::tests::agent_service_mocks::MockProvider;
use crate::tui::app::{App, DisplayMessage};
use crate::tui::events::AppMode;
use crate::tui::onboarding::{OnboardingStep, OnboardingWizard};
use crate::tui::render::render;

/// A size a real terminal actually uses, so the responsive layout branches are
/// exercised the way a user sees them.
const WIDTH: u16 = 150;
const HEIGHT: u16 = 45;

fn message(role: &str, content: String) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content,
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ui-dump");
    std::fs::create_dir_all(&dir).expect("create target/ui-dump");
    dir
}

/// Renderer-independent colour token, so the consumer never has to know the
/// ratatui palette layout.
fn colour(c: Color) -> String {
    match c {
        Color::Reset => "reset".to_string(),
        Color::Rgb(r, g, b) => format!("rgb:{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("idx:{i}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Write one frame: the grid as text, plus style runs a renderer can replay.
fn dump(name: &str, buf: &Buffer) {
    let dir = out_dir();
    let area = buf.area;
    let mut text = String::new();
    let mut spans = format!("SIZE {} {}\n", area.width, area.height);

    for y in 0..area.height {
        let mut row = String::new();
        let mut x = 0u16;
        while x < area.width {
            let first = &buf[(x, y)];
            let (fg, bg, modifier) = (first.fg, first.bg, first.modifier);
            let start = x;
            let mut run = String::new();
            while x < area.width {
                let cell = &buf[(x, y)];
                if cell.fg != fg || cell.bg != bg || cell.modifier != modifier {
                    break;
                }
                run.push_str(cell.symbol());
                x += 1;
            }
            // Modifier's Debug can carry spaces ("BOLD | ITALIC"); the text is
            // last on the line so its own spaces are harmless.
            let mods = format!("{modifier:?}").replace(' ', "");
            spans.push_str(&format!(
                "SPAN {y} {start} {} {} {} {mods} {run}\n",
                x - start,
                colour(fg),
                colour(bg)
            ));
            row.push_str(&run);
        }
        text.push_str(row.trim_end());
        text.push('\n');
    }

    std::fs::write(dir.join(format!("{name}.txt")), text).expect("write grid txt");
    std::fs::write(dir.join(format!("{name}.spans")), spans).expect("write span file");
    println!("[ui-dump] {name} {WIDTH}x{HEIGHT}");
}

/// A live `App` with `messages` chat rows and a few real sessions behind it, so
/// the list screens have something to show instead of an empty state.
async fn app_with_chat(messages: usize) -> App {
    let db = Database::connect_in_memory()
        .await
        .expect("connect_in_memory");
    db.run_migrations().await.expect("migrations");
    let context = ServiceContext::new(db.pool().clone());

    // Real rows through the real service: `load_sessions()` reads these back.
    let sessions = SessionService::new(context.clone());
    for title in [
        "Windows lock: creation-time proof",
        "Clipboard backends on Windows",
        "Scheduled Task install/start/stop",
    ] {
        let _ = sessions.create_session(Some(title.to_string())).await;
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let service = Arc::new(AgentService::new_for_test(provider, context.clone()).await);
    #[cfg(feature = "whatsapp")]
    let mut app = App::new(
        service,
        context,
        Arc::new(crate::channels::whatsapp::WhatsAppState::new()),
    );
    #[cfg(not(feature = "whatsapp"))]
    let mut app = App::new(service, context);

    for i in 0..messages {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        app.messages
            .push(message(role, format!("message number {i} in the history")));
    }
    app
}

/// Draw the current state of `app` and persist it under `name`.
async fn shot(name: &str, app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
    terminal.draw(|f| render(f, app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    dump(name, &buf);
}

async fn shot_mode(name: &str, mode: AppMode) {
    let mut app = app_with_chat(6).await;
    app.switch_mode(mode).await.expect("switch_mode");
    shot(name, &mut app).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_01_chat() {
    let mut app = app_with_chat(8).await;
    app.input_buffer = "why did the Windows job go red?".to_string();
    shot("01-chat", &mut app).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_02_chat_slash_autocomplete() {
    let mut app = app_with_chat(4).await;
    app.input_buffer = "/he".to_string();
    app.slash_suggestions_active = true;
    shot("02-chat-slash-autocomplete", &mut app).await;
}

/// Every wizard step a first-run Windows user walks through.
#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_03_onboarding_steps() {
    let steps = [
        OnboardingStep::ModeSelect,
        OnboardingStep::Workspace,
        OnboardingStep::ProviderAuth,
        OnboardingStep::Channels,
        OnboardingStep::TelegramSetup,
        OnboardingStep::DiscordSetup,
        OnboardingStep::WhatsAppSetup,
        OnboardingStep::SlackSetup,
        OnboardingStep::TrelloSetup,
        OnboardingStep::VoiceSetup,
        OnboardingStep::ImageSetup,
        OnboardingStep::Daemon,
        OnboardingStep::HealthCheck,
        OnboardingStep::BrainSetup,
        OnboardingStep::Complete,
    ];
    for (i, step) in steps.into_iter().enumerate() {
        let mut app = app_with_chat(0).await;
        let mut wizard = OnboardingWizard::new();
        wizard.step = step;
        app.onboarding = Some(wizard);
        app.mode = AppMode::Onboarding;
        let name = format!("03-onboarding-{:02}-{step:?}", i + 1);
        shot(&name, &mut app).await;
    }
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_04_sessions() {
    shot_mode("04-sessions", AppMode::Sessions).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_05_help() {
    shot_mode("05-help", AppMode::Help).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_06_mission_control() {
    shot_mode("06-mission-control", AppMode::MissionControl).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_07_skills() {
    shot_mode("07-skills", AppMode::SkillsList).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_08_profiles() {
    shot_mode("08-profiles", AppMode::Profiles).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_09_projects() {
    shot_mode("09-projects", AppMode::Projects).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_10_session_files() {
    shot_mode("10-session-files", AppMode::SessionFiles).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_11_settings() {
    shot_mode("11-settings", AppMode::Settings).await;
}

#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_12_usage_dashboard() {
    let mut app = app_with_chat(6).await;
    app.open_usage_dashboard().await;
    app.switch_mode(AppMode::UsageDashboard)
        .await
        .expect("switch_mode");
    shot("12-usage-dashboard", &mut app).await;
}

/// The dump is only useful if it is not silently empty: every file must carry
/// the requested size and at least one non-blank row.
#[tokio::test]
#[ignore = "UI dump: explicit only, run with --ignored"]
async fn dump_13_chat_is_not_blank() {
    let mut app = app_with_chat(8).await;
    app.input_buffer = "assert the dump actually has pixels".to_string();
    shot("13-chat-fidelity-check", &mut app).await;

    let dir = out_dir();
    let text = std::fs::read_to_string(dir.join("13-chat-fidelity-check.txt"))
        .expect("grid file must exist after the dump");
    assert!(
        text.lines().count() == HEIGHT as usize,
        "grid must have {HEIGHT} rows, got {}",
        text.lines().count()
    );
    assert!(
        text.lines().any(|l| !l.trim().is_empty()),
        "a blank grid means the render produced nothing: {text:?}"
    );
    assert!(
        text.contains("message number 0"),
        "the seeded chat must appear in the frame"
    );
}
