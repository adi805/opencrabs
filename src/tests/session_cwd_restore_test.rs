//! Restoring a session's persisted working directory (`/cd` in a channel).

use std::path::PathBuf;

use crate::brain::agent::service::AgentService;
use crate::brain::agent::service::session_cwd::restorable_cwd;
use crate::brain::provider::Provider;
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};
use crate::tests::agent_service_mocks::MockProvider;
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn unset_or_blank_persisted_value_restores_nothing() {
    assert_eq!(restorable_cwd(None), None);
    assert_eq!(restorable_cwd(Some("")), None);
    assert_eq!(restorable_cwd(Some("   ")), None);
}

#[test]
fn stale_path_restores_nothing() {
    assert_eq!(
        restorable_cwd(Some("/nonexistent/repo/moved/away")),
        None,
        "a directory that no longer exists must not be restored"
    );
}

#[test]
fn existing_directory_is_restored() {
    let dir = std::env::temp_dir();
    let restored = restorable_cwd(Some(dir.to_string_lossy().as_ref()));
    assert_eq!(
        restored,
        Some(dir),
        "an existing directory is restored as written, not canonicalized"
    );
}

#[test]
fn collapsed_home_path_is_expanded() {
    let home = dirs::home_dir().expect("home dir");
    let restored = restorable_cwd(Some("~")).expect("home must restore");
    assert_eq!(restored, home);
    assert_ne!(
        restored,
        PathBuf::from("~"),
        "the tilde must be expanded, not passed through as a literal directory name"
    );
}

// ---------------------------------------------------------------------------
// #1810 ordering regression: readers must not steal the restore's say.
// ---------------------------------------------------------------------------

async fn make_service_with_context() -> (AgentService, ServiceContext) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let svc = AgentService::new_for_test(provider, context.clone()).await;
    (svc, context)
}

/// THE #1810 regression. On the buggy tree the first
/// `get_working_directory_for_session` (what prompt/brain builds call) lazily
/// created the session's cwd handle from the launch directory, so
/// `session_working_dir_unset` flipped false before the tool-loop restore ever
/// ran and every channel session executed in the launch directory. Red on
/// b844b213e at the "reader must not seed" assertion.
#[tokio::test]
async fn readers_do_not_seed_and_persisted_restore_still_has_a_say() {
    let (svc, context) = make_service_with_context().await;
    let launch_dir = svc.get_working_directory();
    let session_dir =
        std::env::temp_dir().join(format!("opencrabs-1810-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&session_dir).expect("create session dir");

    let sessions = SessionService::new(context);
    let created = sessions.create_session(None).await.expect("create session");
    sessions
        .update_session_working_directory(
            created.id,
            Some(session_dir.to_string_lossy().into_owned()),
        )
        .await
        .expect("persist session working directory");

    // 1. No handle yet: the persisted directory still has a say.
    assert!(
        svc.session_working_dir_unset(created.id),
        "a fresh session must not have a cwd handle before any turn"
    );

    // 2. The early-toucher class that fired on the buggy tree (prompt/brain
    //    build reading the session cwd): a reader resolves the value WITHOUT
    //    creating the handle.
    let read = svc.get_working_directory_for_session(created.id);
    assert_eq!(
        read, launch_dir,
        "an untouched session still falls back to the global launch directory"
    );
    assert!(
        svc.session_working_dir_unset(created.id),
        "a reader must not seed the handle: the persisted-cwd restore would lose its say (#1810)"
    );

    // 3. The tool-loop entry step restores from the DB row.
    svc.restore_persisted_working_directory(created.id).await;
    assert_eq!(
        svc.get_working_directory_for_session(created.id),
        session_dir,
        "the persisted directory wins over the launch directory"
    );

    // 4. Global untouched: restoring one chat's cwd must not move the seed.
    assert_eq!(
        svc.get_working_directory(),
        launch_dir,
        "a channel session restore must never drag the global cwd"
    );
}
