use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static SEASON_EPISODE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,2})\s*E(\d{1,3}(?:\.\d+)?)\b").ok());
static EPISODE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\s-\s|\b)(\d{1,4}(?:\.\d+)?)(?:v\d+)?(?:\s|$)").ok());
static TAGS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]|\([^)]*\)").ok());

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEpisode {
    pub title: String,
    pub season: u32,
    pub number: f64,
}

pub fn parse_filename(path: &Path) -> Option<ParsedEpisode> {
    let file = path.file_stem()?.to_string_lossy();
    let cleaned = TAGS.as_ref()?.replace_all(&file, " ");
    let (season, number, end) = if let Some(c) = SEASON_EPISODE.as_ref()?.captures(&cleaned) {
        (c[1].parse().ok()?, c[2].parse().ok()?, c.get(0)?.start())
    } else {
        let caps = EPISODE.as_ref()?.captures(&cleaned)?;
        let m = caps.get(1)?;
        let number = m.as_str().parse().ok()?;
        let season = path
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .find_map(|s| {
                s.to_ascii_lowercase()
                    .strip_prefix("season ")
                    .and_then(|n| n.trim().parse().ok())
            })
            .unwrap_or(1);
        (season, number, m.start())
    };
    let mut title = cleaned[..end]
        .trim()
        .trim_end_matches(['-', '_', ' '])
        .replace('_', " ");
    title = title.trim().to_string();
    if title.is_empty() {
        title = path.parent()?.file_name()?.to_string_lossy().to_string();
    }
    Some(ParsedEpisode {
        title,
        season,
        number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_names() {
        let cases = [
            (
                "[SubsPlease] Frieren - 12 (1080p) [A1B2C3D4].mkv",
                "Frieren",
                1,
                12.0,
            ),
            ("Frieren S01E12.mkv", "Frieren", 1, 12.0),
            ("Frieren - 12v2.mkv", "Frieren", 1, 12.0),
            (
                "Jujutsu Kaisen - S02E05 - Title.mp4",
                "Jujutsu Kaisen",
                2,
                5.0,
            ),
            ("One Piece - 1085.mkv", "One Piece", 1, 1085.0),
            ("Show - 12.5.mkv", "Show", 1, 12.5),
        ];
        for (name, title, season, number) in cases {
            let parsed = parse_filename(Path::new(name)).unwrap();
            assert_eq!(parsed.title, title);
            assert_eq!(parsed.season, season);
            assert_eq!(parsed.number, number);
        }
    }

    #[test]
    fn season_is_read_from_parent_folder() {
        let parsed = parse_filename(Path::new("Anime Name/Season 2/Anime Name - 03.mkv")).unwrap();
        assert_eq!(parsed.season, 2);
        assert_eq!(parsed.number, 3.0);
    }
}
