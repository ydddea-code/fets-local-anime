use serde_json::Value;
use std::path::Path;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct ProbeInfo {
    pub duration_sec: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub container: Option<String>,
    pub streams_json: String,
}

pub fn parse_probe_json(input: &str) -> Result<ProbeInfo, String> {
    let root: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let streams = root
        .get("streams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let format = root.get("format").cloned().unwrap_or(Value::Null);
    let duration_sec = format
        .get("duration")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let video_codec = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
        .and_then(|s| s.get("codec_name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let audio_codec = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"))
        .and_then(|s| s.get("codec_name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let container = format
        .get("format_name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ProbeInfo {
        duration_sec,
        video_codec,
        audio_codec,
        container,
        streams_json: serde_json::to_string(&streams).map_err(|e| e.to_string())?,
    })
}

pub async fn probe(executable: &str, path: &Path) -> Result<ProbeInfo, String> {
    let mut command = Command::new(executable);
    command
        .arg("-v").arg("error")
        .arg("-show_entries").arg("format=duration,format_name:stream=index,codec_type,codec_name,channels:stream_tags=language,title")
        .arg("-of").arg("json")
        .arg(path);
    // Never flash a console window while scanning (Windows).
    crate::desktop::no_window(&mut command);
    let output = command
        .output()
        .await
        .map_err(|e| format!("could not start ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    parse_probe_json(&String::from_utf8_lossy(&output.stdout))
}

pub async fn generate_thumbnail(
    executable: &str,
    input: &Path,
    output: &Path,
) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut command = Command::new(executable);
    command
        .args(["-hide_banner", "-loglevel", "error", "-ss", "30", "-i"])
        .arg(input)
        .args(["-frames:v", "1", "-vf", "scale=480:-2", "-y"])
        .arg(output);
    crate::desktop::no_window(&mut command);
    let result = command
        .output()
        .await
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_probe_json;

    #[test]
    fn parses_format_and_stream_codecs() {
        let info = parse_probe_json(r#"{"format":{"duration":"121.5","format_name":"matroska,webm"},"streams":[{"codec_type":"video","codec_name":"hevc"},{"codec_type":"audio","codec_name":"aac"}]}"#).unwrap();
        assert_eq!(info.duration_sec, Some(121.5));
        assert_eq!(info.video_codec.as_deref(), Some("hevc"));
        assert_eq!(info.audio_codec.as_deref(), Some("aac"));
        assert_eq!(info.container.as_deref(), Some("matroska,webm"));
    }
}
