use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, CACHE_CONTROL, ETAG, EXPIRES,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT,
};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct OsmTileProvider {
    pub cache_root: PathBuf,
    pub tile_size_px: usize,
    pub tile_url_template: String,
}

#[derive(Debug, Error)]
pub enum TileError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unexpected tile response: {0}")]
    Unexpected(String),
}

impl OsmTileProvider {
    pub fn new(cache_root: impl AsRef<Path>) -> Result<Self, TileError> {
        fs::create_dir_all(cache_root.as_ref())?;
        Ok(Self {
            cache_root: cache_root.as_ref().to_path_buf(),
            tile_size_px: 256,
            tile_url_template: env::var("VANTAGE_TILE_URL_TEMPLATE")
                .ok()
                .filter(|value| {
                    value.contains("{z}") && value.contains("{x}") && value.contains("{y}")
                })
                .unwrap_or_else(|| "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into()),
        })
    }

    pub fn cache_path(&self, z: u32, x: i32, y: i32) -> PathBuf {
        self.cache_root
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.png"))
    }

    pub fn ensure_tile_cached(&self, z: u32, x: i32, y: i32) -> Result<PathBuf, TileError> {
        let wrapped_x = wrap_tile_x(x, z);
        let max_index = (1_i32 << z) - 1;
        if y < 0 || y > max_index {
            return Err(TileError::Unexpected("tile out of range".into()));
        }

        let path = self.cache_path(z, wrapped_x, y);
        if path.exists() {
            return Ok(path);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let url = self
            .tile_url_template
            .replace("{z}", &z.to_string())
            .replace("{x}", &wrapped_x.to_string())
            .replace("{y}", &y.to_string());
        let mut metadata_store =
            TileMetadataStore::open(self.cache_root.join("tile-cache.sqlite"))?;
        let existing_metadata = metadata_store.get(&url)?;
        let now = Utc::now();

        if path.exists()
            && existing_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.expires_at > now)
        {
            return Ok(path);
        }

        let mut request = tile_http_client().get(&url);
        if let Some(metadata) = &existing_metadata {
            if let Some(etag) = &metadata.etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = &metadata.last_modified {
                request = request.header(IF_MODIFIED_SINCE, last_modified);
            }
        }

        let response = request.send()?.error_for_status()?;
        let headers = response.headers().clone();
        let status = response.status();

        if status == reqwest::StatusCode::NOT_MODIFIED {
            let refreshed_metadata =
                TileMetadata::from_headers(&url, &headers, existing_metadata.as_ref(), now);
            metadata_store.upsert(&refreshed_metadata)?;
            return Ok(path);
        }

        let bytes = response.bytes()?;
        fs::write(&path, &bytes)?;
        let metadata = TileMetadata::from_headers(&url, &headers, existing_metadata.as_ref(), now);
        metadata_store.upsert(&metadata)?;
        Ok(path)
    }
}

pub fn wrap_tile_x(x: i32, z: u32) -> i32 {
    let modulus = 1_i32 << z;
    ((x % modulus) + modulus) % modulus
}

fn tile_http_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("image/png,image/*;q=0.8,*/*;q=0.1"),
    );
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.8"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Vantage/0.1 (dragunov7072@gmail.com)"),
    );

    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .build()
        .expect("tile HTTP client should build")
}

#[derive(Clone, Debug)]
struct TileMetadata {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    expires_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TileMetadata {
    fn from_headers(
        url: &str,
        headers: &HeaderMap,
        previous: Option<&TileMetadata>,
        now: DateTime<Utc>,
    ) -> Self {
        let etag =
            header_value(headers, ETAG).or_else(|| previous.and_then(|value| value.etag.clone()));
        let last_modified = header_value(headers, LAST_MODIFIED)
            .or_else(|| previous.and_then(|value| value.last_modified.clone()));
        let expires_at = expires_at_from_headers(headers, now).unwrap_or_else(|| {
            previous
                .map(|value| value.expires_at)
                .unwrap_or(now + chrono::Duration::days(7))
        });

        Self {
            url: url.to_owned(),
            etag,
            last_modified,
            expires_at,
            updated_at: now,
        }
    }
}

struct TileMetadataStore {
    connection: Connection,
}

impl TileMetadataStore {
    fn open(path: PathBuf) -> Result<Self, TileError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS tile_metadata (
                url TEXT PRIMARY KEY,
                etag TEXT,
                last_modified TEXT,
                expires_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            ",
        )?;
        Ok(Self { connection })
    }

    fn get(&mut self, url: &str) -> Result<Option<TileMetadata>, TileError> {
        self.connection
            .query_row(
                "SELECT url, etag, last_modified, expires_at, updated_at FROM tile_metadata WHERE url = ?1",
                params![url],
                |row| {
                    let expires_at_raw: String = row.get(3)?;
                    let updated_at_raw: String = row.get(4)?;
                    Ok(TileMetadata {
                        url: row.get(0)?,
                        etag: row.get(1)?,
                        last_modified: row.get(2)?,
                        expires_at: parse_rfc3339_utc(&expires_at_raw).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        updated_at: parse_rfc3339_utc(&updated_at_raw).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    })
                },
            )
            .optional()
            .map_err(TileError::from)
    }

    fn upsert(&mut self, metadata: &TileMetadata) -> Result<(), TileError> {
        self.connection.execute(
            "
            INSERT INTO tile_metadata (url, etag, last_modified, expires_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(url) DO UPDATE SET
                etag = excluded.etag,
                last_modified = excluded.last_modified,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at
            ",
            params![
                metadata.url,
                metadata.etag,
                metadata.last_modified,
                metadata.expires_at.to_rfc3339(),
                metadata.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

fn expires_at_from_headers(headers: &HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(cache_control) = header_value(headers, CACHE_CONTROL) {
        for directive in cache_control.split(',').map(|value| value.trim()) {
            if let Some(max_age) = directive.strip_prefix("max-age=") {
                if let Ok(seconds) = max_age.parse::<i64>() {
                    return Some(now + chrono::Duration::seconds(seconds.max(0)));
                }
            }
            if directive.eq_ignore_ascii_case("no-cache") {
                return Some(now);
            }
        }
    }

    header_value(headers, EXPIRES).and_then(|value| {
        DateTime::parse_from_rfc2822(&value)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    })
}

fn header_value(headers: &HeaderMap, key: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned())
}

fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|parsed| parsed.with_timezone(&Utc))
}
