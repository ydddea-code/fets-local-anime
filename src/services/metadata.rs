use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

const ENDPOINT: &str = "https://graphql.anilist.co";
const QUERY: &str = "query ($search: String, $type: MediaType) { Media(search: $search, type: $type) { id title { romaji english native } description(asHtml: false) seasonYear status coverImage { large } genres averageScore } }";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AniListEntry {
    pub id: i64,
    pub romaji_title: Option<String>,
    pub english_title: Option<String>,
    pub native_title: Option<String>,
    pub description: Option<String>,
    pub year: Option<i64>,
    pub status: Option<String>,
    pub cover_url: Option<String>,
    pub genres: Vec<String>,
    pub score: Option<i64>,
}

#[derive(Clone)]
pub struct MetadataClient {
    client: reqwest::Client,
    rate_gate: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl MetadataClient {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("AnimeStream local media library")
                .build()?,
            rate_gate: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn lookup(
        &self,
        pool: &SqlitePool,
        title: &str,
        ttl_hours: u64,
    ) -> Result<Option<(AniListEntry, bool)>, String> {
        let cache_key = title.trim().to_lowercase();
        if let Some(payload) = sqlx::query_scalar::<_, String>(
            "SELECT payload_json FROM metadata_cache WHERE cache_key=? AND expires_at>unixepoch()",
        )
        .bind(&cache_key)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        {
            let entry = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
            return Ok(Some((entry, true)));
        }

        let entry = self.fetch(title).await?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let payload = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        let ttl_seconds = ttl_hours.clamp(1, 24 * 30) as i64 * 3600;
        sqlx::query("INSERT INTO metadata_cache(cache_key,payload_json,expires_at) VALUES(?,?,unixepoch()+?) ON CONFLICT(cache_key) DO UPDATE SET payload_json=excluded.payload_json,expires_at=excluded.expires_at")
            .bind(cache_key).bind(payload).bind(ttl_seconds).execute(pool).await.map_err(|e| e.to_string())?;
        Ok(Some((entry, false)))
    }

    async fn fetch(&self, title: &str) -> Result<Option<AniListEntry>, String> {
        let mut last_error = String::new();
        for attempt in 0..3 {
            {
                let mut previous = self.rate_gate.lock().await;
                if let Some(last) = *previous {
                    let interval = Duration::from_millis(2200);
                    if last.elapsed() < interval {
                        tokio::time::sleep(interval - last.elapsed()).await;
                    }
                }
                *previous = Some(tokio::time::Instant::now());
            }
            let response = self.client.post(ENDPOINT)
                .json(&serde_json::json!({"query": QUERY, "variables": {"search": title, "type": "ANIME"}}))
                .send().await.map_err(|e| format!("AniList request failed: {e}"))?;
            if response.status().as_u16() == 429 {
                let delay = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(5)
                    .min(60);
                last_error = format!("AniList rate limited the request; retry after {delay}s");
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
                break;
            }
            if response.status().is_server_error() {
                last_error = format!("AniList returned {}", response.status());
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                    continue;
                }
                break;
            }
            let response = response
                .error_for_status()
                .map_err(|e| format!("AniList request failed: {e}"))?;
            let json: Value = response
                .json()
                .await
                .map_err(|e| format!("AniList returned invalid JSON: {e}"))?;
            return parse_entry(&json);
        }
        Err(last_error)
    }
}

fn parse_entry(response: &Value) -> Result<Option<AniListEntry>, String> {
    let Some(media) = response
        .pointer("/data/Media")
        .filter(|media| !media.is_null())
    else {
        return Ok(None);
    };
    let title = media.get("title").ok_or("AniList response had no title")?;
    let text = |key: &str| title.get(key).and_then(Value::as_str).map(str::to_owned);
    let status = media
        .get("status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "RELEASING" => "airing".to_owned(),
            "FINISHED" => "finished".to_owned(),
            _ => "unknown".to_owned(),
        });
    Ok(Some(AniListEntry {
        id: media
            .get("id")
            .and_then(Value::as_i64)
            .ok_or("AniList response had no media ID")?,
        romaji_title: text("romaji"),
        english_title: text("english"),
        native_title: text("native"),
        description: media
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        year: media.get("seasonYear").and_then(Value::as_i64),
        status,
        cover_url: media
            .pointer("/coverImage/large")
            .and_then(Value::as_str)
            .map(str::to_owned),
        genres: media
            .get("genres")
            .and_then(Value::as_array)
            .map(|genres| {
                genres
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        score: media.get("averageScore").and_then(Value::as_i64),
    }))
}

#[cfg(test)]
mod tests {
    use super::parse_entry;
    use serde_json::json;

    #[test]
    fn parses_optional_media_fields_and_status() {
        let data = json!({"data":{"Media":{"id":42,"title":{"romaji":"Example","english":"Example Show","native":"例"},"description":"Summary","seasonYear":2024,"status":"RELEASING","coverImage":{"large":"https://img.example/poster.jpg"},"genres":["Action","Drama"],"averageScore":81}}});
        let entry = parse_entry(&data).unwrap().unwrap();
        assert_eq!(entry.id, 42);
        assert_eq!(entry.english_title.as_deref(), Some("Example Show"));
        assert_eq!(entry.status.as_deref(), Some("airing"));
        assert_eq!(entry.genres, vec!["Action", "Drama"]);
    }

    #[test]
    fn returns_none_when_search_has_no_match() {
        assert!(parse_entry(&json!({"data":{"Media":null}}))
            .unwrap()
            .is_none());
    }
}
