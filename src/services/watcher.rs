use crate::{routes, state::AppState};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub async fn watch_libraries(state: Arc<AppState>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();
    let mut watcher = match notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::error!(%error, "could not start library watcher");
            return;
        }
    };
    for path in &state.settings.library.paths {
        if let Err(error) = watcher.watch(Path::new(path), RecursiveMode::Recursive) {
            tracing::warn!(%error, path, "could not watch library path");
        }
    }

    while let Some(event) = rx.recv().await {
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
        tokio::time::sleep(Duration::from_millis(700)).await;
        while rx.try_recv().is_ok() {}
        if let Err(error) = routes::scan_startup(&state).await {
            tracing::warn!(%error, "library rescan after filesystem change failed");
        }
    }
}
