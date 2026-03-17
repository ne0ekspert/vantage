use std::env;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};

use crate::traffic::GeoBounds;

#[derive(Clone, Debug)]
pub struct WigleSettings {
    pub api_name: String,
    pub api_token: String,
    pub results_per_page: u16,
}

impl Default for WigleSettings {
    fn default() -> Self {
        Self {
            api_name: String::new(),
            api_token: String::new(),
            results_per_page: 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WigleNetworkRecord {
    pub ssid: Option<String>,
    pub netid: String,
    pub network_type: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub channel: Option<String>,
    pub encryption: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub lastupdt: Option<String>,
    pub freenet: Option<bool>,
    pub paynet: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct WigleQueryResult {
    pub bounds: GeoBounds,
    pub fetched_at: DateTime<Utc>,
    pub networks: Vec<WigleNetworkRecord>,
}

#[derive(Clone, Debug, Deserialize)]
struct WigleSearchResponse {
    success: bool,
    message: Option<String>,
    results: Vec<WigleSearchNetwork>,
}

#[derive(Clone, Debug, Deserialize)]
struct WigleSearchNetwork {
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    ssid: Option<String>,
    netid: String,
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_optional_stringish"
    )]
    network_type: Option<String>,
    trilat: Option<f64>,
    trilong: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    channel: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    encryption: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    city: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    region: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    country: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_stringish")]
    lastupdt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_boolish")]
    freenet: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_boolish")]
    paynet: Option<bool>,
}

pub struct WigleManager {
    pub settings: WigleSettings,
    pending: bool,
    pub status_message: String,
    tx: Sender<Result<WigleQueryResult, String>>,
    rx: Receiver<Result<WigleQueryResult, String>>,
}

impl Default for WigleManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: WigleSettings::default(),
            pending: false,
            status_message: "WiGLE layer idle".into(),
            tx,
            rx,
        }
    }
}

impl WigleManager {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request_import(&mut self, bounds: GeoBounds) -> Result<(), String> {
        if self.pending {
            return Err("WiGLE import already running".into());
        }

        let api_name = if self.settings.api_name.trim().is_empty() {
            env::var("VANTAGE_WIGLE_API_NAME").map_err(|_| "Missing WiGLE API name".to_string())?
        } else {
            self.settings.api_name.trim().to_owned()
        };
        let api_token = if self.settings.api_token.trim().is_empty() {
            env::var("VANTAGE_WIGLE_API_TOKEN")
                .map_err(|_| "Missing WiGLE API token".to_string())?
        } else {
            self.settings.api_token.trim().to_owned()
        };

        let tx = self.tx.clone();
        let results_per_page = self.settings.results_per_page.clamp(10, 100);
        self.pending = true;
        self.status_message = "Querying WiGLE…".into();

        thread::spawn(move || {
            let result = fetch_networks(bounds, &api_name, &api_token, results_per_page);
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub fn drain_results(&mut self) -> Option<Result<WigleQueryResult, String>> {
        let mut latest = None;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            latest = Some(result);
        }

        if let Some(result) = &latest {
            match result {
                Ok(query) => {
                    self.status_message =
                        format!("WiGLE: {} networks fetched", query.networks.len());
                }
                Err(error) => {
                    self.status_message = format!("WiGLE fetch failed: {error}");
                }
            }
        }

        latest
    }
}

fn fetch_networks(
    bounds: GeoBounds,
    api_name: &str,
    api_token: &str,
    results_per_page: u16,
) -> Result<WigleQueryResult, String> {
    let bounds = bounds.normalized();
    let response = http_client()
        .get("https://api.wigle.net/api/v2/network/search")
        .basic_auth(api_name, Some(api_token))
        .query(&[
            ("latrange1", bounds.lamin.to_string()),
            ("latrange2", bounds.lamax.to_string()),
            ("longrange1", bounds.lomin.to_string()),
            ("longrange2", bounds.lomax.to_string()),
            ("resultsPerPage", results_per_page.to_string()),
        ])
        .send()
        .map_err(|error| format!("WiGLE request failed: {error}"))?;

    let status = response.status();
    let body_text = response
        .text()
        .map_err(|error| format!("Failed to read WiGLE response: {error}"))?;
    let body: WigleSearchResponse = serde_json::from_str(&body_text)
        .map_err(|error| format!("Failed to decode WiGLE response: {error}"))?;

    if !status.is_success() || !body.success {
        return Err(body
            .message
            .unwrap_or_else(|| format!("WiGLE request failed with {status}")));
    }

    let networks = body
        .results
        .into_iter()
        .filter_map(|record| {
            Some(WigleNetworkRecord {
                ssid: record.ssid,
                netid: record.netid,
                network_type: record.network_type,
                lat: record.trilat?,
                lon: record.trilong?,
                channel: record.channel,
                encryption: record.encryption,
                city: record.city,
                region: record.region,
                country: record.country,
                lastupdt: record.lastupdt,
                freenet: record.freenet,
                paynet: record.paynet,
            })
        })
        .collect();

    Ok(WigleQueryResult {
        bounds,
        fetched_at: Utc::now(),
        networks,
    })
}

fn http_client() -> Client {
    Client::builder()
        .user_agent(concat!("vantage/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("WiGLE HTTP client should build")
}

fn deserialize_optional_boolish<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        Number(i64),
        Text(String),
    }

    let value = Option::<Boolish>::deserialize(deserializer)?;
    Ok(match value {
        None => None,
        Some(Boolish::Bool(value)) => Some(value),
        Some(Boolish::Number(value)) => Some(value != 0),
        Some(Boolish::Text(value)) => {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "" | "?" | "unknown" | "null" => None,
                "1" | "true" | "t" | "yes" | "y" => Some(true),
                "0" | "false" | "f" | "no" | "n" => Some(false),
                _ => None,
            }
        }
    })
}

fn deserialize_optional_stringish<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stringish {
        Text(String),
        Integer(i64),
        Float(f64),
        Bool(bool),
    }

    let value = Option::<Stringish>::deserialize(deserializer)?;
    Ok(match value {
        None => None,
        Some(Stringish::Text(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Some(Stringish::Integer(value)) => Some(value.to_string()),
        Some(Stringish::Float(value)) => Some(value.to_string()),
        Some(Stringish::Bool(value)) => Some(value.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::WigleSearchNetwork;

    #[test]
    fn wigle_boolish_fields_accept_question_mark_strings() {
        let parsed: WigleSearchNetwork = serde_json::from_str(
            r#"{
                "netid":"aa:bb:cc:dd:ee:ff",
                "ssid":"Cafe",
                "type":"WIFI",
                "trilat":37.5,
                "trilong":126.9,
                "freenet":"?",
                "paynet":"false"
            }"#,
        )
        .expect("WiGLE response fragment should deserialize");

        assert_eq!(parsed.freenet, None);
        assert_eq!(parsed.paynet, Some(false));
    }

    #[test]
    fn wigle_stringish_fields_accept_integer_values() {
        let parsed: WigleSearchNetwork = serde_json::from_str(
            r#"{
                "netid":"aa:bb:cc:dd:ee:ff",
                "ssid":"Cafe",
                "type":"WIFI",
                "trilat":37.5,
                "trilong":126.9,
                "channel":1,
                "region":11,
                "country":"KR"
            }"#,
        )
        .expect("WiGLE response fragment with integer string fields should deserialize");

        assert_eq!(parsed.channel.as_deref(), Some("1"));
        assert_eq!(parsed.region.as_deref(), Some("11"));
    }
}
