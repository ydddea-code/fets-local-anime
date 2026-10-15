//! Filesystem watcher for the configured library folders. Events are
//! debounced and trigger a background rescan; when the user changes folder
//! settings, the watcher re-registers its roots without restarting the app.

use crate::{routes, state::AppState};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub async fn watch_libraries(state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::error!(%error, "could not start library watcher");
            return;
        }
    };
    let mut registered: Vec<std::path::PathBuf> = Vec::new();
    register_roots(&state, &mut watcher, &mut registered);

    loop {
        // Pick up folder add/remove and the watch on/off toggle live.
        register_roots(&state, &mut watcher, &mut registered);
        let event = tokio::time::timeout(Duration::from_millis(3000), rx.recv()).await;
        let Ok(maybe_event) = event else { continue };
        let Some(event) = maybe_event else { break };
        match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) => {}
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(%error, "library watcher reported an error");
                continue;
            }
        }
        // Debounce bursts (e.g. Explorer finishing a copy).
        tokio::time::sleep(Duration::from_millis(700)).await;
        while rx.try_recv().is_ok() {}
        if let Err(error) = routes::scan_startup(&state).await {
            tracing::warn!(%error, "library rescan after filesystem change failed");
        }
    }
}

fn register_roots(
    state: &Arc<AppState>,
    watcher: &mut RecommendedWatcher,
    registered: &mut Vec<std::path::PathBuf>,
) {
    let desired: Vec<std::path::PathBuf> = {
        let settings = state.settings.read().expect("settings lock");
        if !settings.library.watch {
            Vec::new()
        } else {
            settings
                .library
                .paths
                .iter()
                .map(|p| Path::new(p.as_str()).to_path_buf())
                .collect()
        }
    };
    registered.retain(|root| {
        if desired.contains(root) {
            true
        } else {
            let _ = watcher.unwatch(root);
            false
        }
    });
    for root in desired {
        if registered.contains(&root) {
            continue;
        }
        match watcher.watch(&root, RecursiveMode::Recursive) {
            Ok(()) => {
                registered.push(root);
            }
            Err(error) => {
                tracing::warn!(%error, path = %root.display(), "could not watch library path");
            }
        }
    }
    *state.watch_roots.lock().expect("watch roots lock") = registered.clone();
}
