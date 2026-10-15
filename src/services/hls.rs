use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};

struct Session {
    directory: PathBuf,
    child: Child,
    last_access: Instant,
    _permit: OwnedSemaphorePermit,
}

pub struct HlsManager {
    root: PathBuf,
    max_sessions: Arc<Semaphore>,
    sessions: Mutex<HashMap<i64, Session>>,
    segment_seconds: u32,
}

impl HlsManager {
    pub fn new(root: PathBuf, max_sessions: usize, segment_seconds: u32) -> Self {
        Self {
            root,
            max_sessions: Arc::new(Semaphore::new(max_sessions.max(1))),
            sessions: Mutex::new(HashMap::new()),
            segment_seconds: segment_seconds.max(1),
        }
    }

    pub async fn ensure_session(
        &self,
        episode_id: i64,
        input: &Path,
        ffmpeg: &str,
    ) -> Result<PathBuf, String> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&episode_id) {
            session.last_access = Instant::now();
            if session.directory.join("index.m3u8").is_file() {
                return Ok(session.directory.clone());
            }
            let directory = session.directory.clone();
            let finished_badly = session
                .child
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some_and(|status| !status.success());
            if !finished_badly {
                drop(sessions);
                return self.wait_or_stop(episode_id, directory).await;
            }
            sessions.remove(&episode_id);
        }

        let permit = self
            .max_sessions
            .clone()
            .try_acquire_owned()
            .map_err(|_| "the HLS transcode limit has been reached".to_owned())?;
        let directory = self.root.join(episode_id.to_string());
        if tokio::fs::try_exists(&directory)
            .await
            .map_err(|e| e.to_string())?
        {
            tokio::fs::remove_dir_all(&directory)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        let playlist = directory.join("index.m3u8");
        let segments = directory.join("seg_%05d.ts");
        let child = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(input)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-f",
                "hls",
                "-hls_time",
            ])
            .arg(self.segment_seconds.to_string())
            .args(["-hls_playlist_type", "event", "-hls_segment_filename"])
            .arg(segments)
            .arg(playlist)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("could not start ffmpeg: {e}"))?;
        sessions.insert(
            episode_id,
            Session {
                directory: directory.clone(),
                child,
                last_access: Instant::now(),
                _permit: permit,
            },
        );
        drop(sessions);

        self.wait_or_stop(episode_id, directory).await
    }

    async fn wait_or_stop(&self, episode_id: i64, directory: PathBuf) -> Result<PathBuf, String> {
        match wait_for_playlist(directory).await {
            Ok(path) => Ok(path),
            Err(error) => {
                let session = self.sessions.lock().await.remove(&episode_id);
                if let Some(mut session) = session {
                    let _ = session.child.kill().await;
                    let _ = tokio::fs::remove_dir_all(session.directory).await;
                }
                Err(error)
            }
        }
    }

    pub async fn cleanup_idle(&self, idle_for: Duration) {
        let mut sessions = self.sessions.lock().await;
        let expired: Vec<_> = sessions
            .iter()
            .filter_map(|(id, session)| (session.last_access.elapsed() >= idle_for).then_some(*id))
            .collect();
        for id in expired {
            if let Some(mut session) = sessions.remove(&id) {
                let _ = session.child.kill().await;
                let _ = tokio::fs::remove_dir_all(session.directory).await;
            }
        }
    }
}

async fn wait_for_playlist(directory: PathBuf) -> Result<PathBuf, String> {
    for _ in 0..100 {
        if directory.join("index.m3u8").is_file() {
            return Ok(directory);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("ffmpeg did not create an HLS playlist in time".into())
}

pub fn supports_direct_play(
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    container: Option<&str>,
) -> bool {
    let video_ok = video_codec
        .is_some_and(|codec| matches!(codec.to_ascii_lowercase().as_str(), "h264" | "avc1"));
    let audio_ok = audio_codec
        .is_none_or(|codec| matches!(codec.to_ascii_lowercase().as_str(), "aac" | "mp3"));
    let container_ok = container.is_some_and(|name| {
        name.split(',')
            .any(|part| matches!(part.trim(), "mov" | "mp4" | "m4a"))
    });
    video_ok && audio_ok && container_ok
}

pub fn valid_segment_name(name: &str) -> bool {
    name.strip_prefix("seg_")
        .and_then(|rest| rest.strip_suffix(".ts"))
        .is_some_and(|digits| digits.len() == 5 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{supports_direct_play, valid_segment_name};

    #[test]
    fn direct_play_requires_browser_friendly_codecs_and_mp4() {
        assert!(supports_direct_play(
            Some("h264"),
            Some("aac"),
            Some("mov,mp4,m4a,3gp,3g2,mj2")
        ));
        assert!(!supports_direct_play(
            Some("hevc"),
            Some("aac"),
            Some("mp4")
        ));
        assert!(!supports_direct_play(
            Some("h264"),
            Some("flac"),
            Some("mp4")
        ));
        assert!(!supports_direct_play(
            Some("h264"),
            Some("aac"),
            Some("matroska,webm")
        ));
    }

    #[test]
    fn segment_route_only_accepts_expected_generated_names() {
        assert!(valid_segment_name("seg_00001.ts"));
        assert!(!valid_segment_name("../../app.db"));
        assert!(!valid_segment_name("seg_1.ts"));
        assert!(!valid_segment_name("seg_00001.m3u8"));
    }
}
