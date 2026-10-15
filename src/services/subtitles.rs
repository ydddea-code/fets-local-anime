use regex::Regex;
use std::sync::LazyLock;

static ASS_TIME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(\d+):(\d{2}):(\d{2})[.,](\d{2})$").ok());
static ASS_TAGS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\{[^}]*\}").ok());

pub fn to_webvtt(format: &str, input: &str) -> Result<String, String> {
    match format.to_ascii_lowercase().as_str() {
        "vtt" | "webvtt" => Ok(if input.trim_start().starts_with("WEBVTT") {
            input.to_owned()
        } else {
            format!("WEBVTT\n\n{input}")
        }),
        "srt" => Ok(srt_to_vtt(input)),
        "ass" | "ssa" => ass_to_vtt(input),
        _ => Err("unsupported subtitle format".into()),
    }
}

fn srt_to_vtt(input: &str) -> String {
    let mut body = Vec::new();
    for line in input.replace('\u{feff}', "").lines() {
        if line.contains("-->") {
            body.push(line.replace(',', "."));
        } else {
            body.push(line.to_owned());
        }
    }
    format!("WEBVTT\n\n{}", body.join("\n").trim())
}

fn ass_to_vtt(input: &str) -> Result<String, String> {
    let mut result = String::from("WEBVTT\n\n");
    let mut cue = 1;
    for line in input.lines().map(str::trim) {
        let Some(dialogue) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<_> = dialogue.splitn(10, ',').collect();
        if fields.len() < 10 {
            continue;
        }
        let start = ass_time_to_vtt(fields[1].trim())?;
        let end = ass_time_to_vtt(fields[2].trim())?;
        let text = fields[9].replace("\\N", "\n").replace("\\n", "\n");
        let text = ASS_TAGS
            .as_ref()
            .ok_or("subtitle matcher is unavailable")?
            .replace_all(&text, "");
        result.push_str(&format!("{cue}\n{start} --> {end}\n{text}\n\n"));
        cue += 1;
    }
    Ok(result)
}

fn ass_time_to_vtt(value: &str) -> Result<String, String> {
    let captures = ASS_TIME
        .as_ref()
        .ok_or("timestamp matcher is unavailable")?
        .captures(value)
        .ok_or_else(|| format!("invalid ASS timestamp: {value}"))?;
    let hours: u32 = captures[1].parse().map_err(|_| "invalid ASS hour value")?;
    Ok(format!(
        "{hours:02}:{}:{}.{}0",
        &captures[2], &captures[3], &captures[4]
    ))
}

#[cfg(test)]
mod tests {
    use super::to_webvtt;

    #[test]
    fn converts_srt_timestamps_and_header() {
        let result = to_webvtt("srt", "1\n00:00:01,250 --> 00:00:02,500\nHalo, semua\n").unwrap();
        assert!(result.starts_with("WEBVTT\n\n"));
        assert!(result.contains("00:00:01.250 --> 00:00:02.500"));
        assert!(result.contains("Halo, semua"));
    }

    #[test]
    fn converts_ass_dialogue_and_drops_styling() {
        let source =
            "[Events]\nDialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\i1}Halo\\Nsemua";
        let result = to_webvtt("ass", source).unwrap();
        assert!(result.contains("00:00:01.000 --> 00:00:02.500"));
        assert!(result.contains("Halo\nsemua"));
        assert!(!result.contains("\\i1"));
    }
}
