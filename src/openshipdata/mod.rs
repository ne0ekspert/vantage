use std::env;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::traffic::GeoBounds;

#[derive(Clone, Debug)]
pub struct OpenShipDataSettings {
    pub api_key: String,
}

impl Default for OpenShipDataSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenShipDataShip {
    pub name: Option<String>,
    pub mmsi: Option<String>,
    pub imo: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub destination: Option<String>,
    pub ship_type: Option<String>,
    pub speed_knots: Option<f64>,
    pub heading_deg: Option<f64>,
    pub eta: Option<String>,
    pub provider: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OpenShipDataQueryResult {
    pub fetched_at: DateTime<Utc>,
    pub ships: Vec<OpenShipDataShip>,
}

pub struct OpenShipDataManager {
    pub settings: OpenShipDataSettings,
    pending: bool,
    pub status_message: String,
    tx: Sender<Result<OpenShipDataQueryResult, String>>,
    rx: Receiver<Result<OpenShipDataQueryResult, String>>,
}

impl Default for OpenShipDataManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: OpenShipDataSettings::default(),
            pending: false,
            status_message: "OpenShipData layer idle".into(),
            tx,
            rx,
        }
    }
}

impl OpenShipDataManager {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request_import(&mut self, bounds: GeoBounds) -> Result<(), String> {
        if self.pending {
            return Err("OpenShipData import already running".into());
        }

        let api_key = if self.settings.api_key.trim().is_empty() {
            env::var("VANTAGE_OPENSHIPDATA_API_KEY")
                .map_err(|_| "Missing OpenShipData API key".to_string())?
        } else {
            self.settings.api_key.trim().to_owned()
        };

        let tx = self.tx.clone();
        self.pending = true;
        self.status_message = "Querying OpenShipData…".into();

        thread::spawn(move || {
            let result = fetch_positions(bounds, &api_key);
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub fn drain_results(&mut self) -> Option<Result<OpenShipDataQueryResult, String>> {
        let mut latest = None;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            latest = Some(result);
        }

        if let Some(result) = &latest {
            match result {
                Ok(query) => {
                    self.status_message =
                        format!("OpenShipData: {} ships fetched", query.ships.len());
                }
                Err(error) => {
                    self.status_message = format!("OpenShipData fetch failed: {error}");
                }
            }
        }

        latest
    }
}

fn fetch_positions(bounds: GeoBounds, api_key: &str) -> Result<OpenShipDataQueryResult, String> {
    let bounds = bounds.normalized();
    let area = format!(
        "{},{},{},{}",
        bounds.lomin, bounds.lamin, bounds.lomax, bounds.lamax
    );
    let response = http_client()
        .get("https://api.openshipdata.com/v2/positions.json")
        .query(&[
            ("area", area),
            ("source", "all".to_owned()),
            ("key", api_key.to_owned()),
        ])
        .send()
        .map_err(|error| format!("OpenShipData request failed: {error}"))?;

    let status = response.status();
    let body_text = response
        .text()
        .map_err(|error| format!("Failed to read OpenShipData response: {error}"))?;
    let body: Value = serde_json::from_str(&body_text)
        .map_err(|error| format!("Failed to decode OpenShipData response: {error}"))?;

    if !status.is_success() {
        return Err(format!("OpenShipData request failed with {status}"));
    }
    if let Some(error) = extract_response_error(&body) {
        return Err(error);
    }

    let ships = extract_items(&body)
        .into_iter()
        .filter_map(parse_ship)
        .collect::<Vec<_>>();

    Ok(OpenShipDataQueryResult {
        fetched_at: Utc::now(),
        ships,
    })
}

fn extract_response_error(body: &Value) -> Option<String> {
    body.get("error")
        .and_then(value_as_stringish)
        .or_else(|| body.get("message").and_then(value_as_stringish))
        .filter(|message| !message.is_empty())
}

fn extract_items(body: &Value) -> Vec<&Value> {
    if let Some(items) = body.as_array() {
        return items.iter().collect();
    }

    let candidates = [
        body.get("positions"),
        body.get("data"),
        body.get("results"),
        body.get("items"),
    ];

    for candidate in candidates.into_iter().flatten() {
        if let Some(items) = candidate.as_array() {
            return items.iter().collect();
        }
    }

    Vec::new()
}

fn parse_ship(item: &Value) -> Option<OpenShipDataShip> {
    let (lon, lat) = find_point(item)?;

    Some(OpenShipDataShip {
        name: find_string(item, &["boatName", "name", "vesselName"]),
        mmsi: find_string(item, &["mmsi", "MMSI"]),
        imo: find_string(item, &["imo", "IMO"]),
        lat,
        lon,
        destination: find_string(item, &["destination"]),
        ship_type: find_string(item, &["boatType", "shipType"]),
        speed_knots: find_f64(item, &["speed", "speedKnots", "sog"]),
        heading_deg: find_f64(item, &["heading", "course", "cog"]),
        eta: find_string(item, &["eta"]),
        provider: find_string(item, &["source", "provider"]),
    })
}

fn find_point(item: &Value) -> Option<(f64, f64)> {
    if let Some(point) = item.get("point") {
        if let Some(array) = point.as_array() {
            let lon = array.first().and_then(value_as_f64)?;
            let lat = array.get(1).and_then(value_as_f64)?;
            return Some((lon, lat));
        }
        if let Some(coords) = point.get("coordinates").and_then(Value::as_array) {
            let lon = coords.first().and_then(value_as_f64)?;
            let lat = coords.get(1).and_then(value_as_f64)?;
            return Some((lon, lat));
        }
    }

    let lon = find_f64(item, &["lon", "lng", "longitude"])?;
    let lat = find_f64(item, &["lat", "latitude"])?;
    Some((lon, lat))
}

fn find_string(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(value_as_stringish))
}

fn find_f64(item: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(value_as_f64))
}

fn value_as_stringish(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn http_client() -> Client {
    Client::builder()
        .user_agent(concat!("vantage/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("OpenShipData HTTP client should build")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{extract_items, parse_ship};

    #[test]
    fn extracts_ship_from_array_response() {
        let payload = json!([{
            "boatName": "Test Ship",
            "mmsi": "123456789",
            "point": [126.97, 37.56],
            "speed": 12.4,
            "heading": 180
        }]);

        let items = extract_items(&payload);
        let ship = parse_ship(items[0]).expect("ship should parse");

        assert_eq!(ship.name.as_deref(), Some("Test Ship"));
        assert_eq!(ship.lon, 126.97);
        assert_eq!(ship.lat, 37.56);
    }

    #[test]
    fn extracts_ship_from_positions_wrapper() {
        let payload = json!({
            "positions": [{
                "name": "Wrapper Ship",
                "longitude": "127.00",
                "latitude": "37.60"
            }]
        });

        let items = extract_items(&payload);
        let ship = parse_ship(items[0]).expect("ship should parse");

        assert_eq!(ship.name.as_deref(), Some("Wrapper Ship"));
        assert_eq!(ship.lon, 127.0);
        assert_eq!(ship.lat, 37.6);
    }
}
