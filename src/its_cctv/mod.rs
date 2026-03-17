use std::env;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::traffic::GeoBounds;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItsRoadType {
    Expressway,
    NationalRoad,
}

impl Default for ItsRoadType {
    fn default() -> Self {
        Self::NationalRoad
    }
}

impl ItsRoadType {
    pub fn as_api_value(self) -> &'static str {
        match self {
            Self::Expressway => "ex",
            Self::NationalRoad => "its",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ItsCctvSettings {
    pub api_key: String,
    pub road_type: ItsRoadType,
}

impl Default for ItsCctvSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            road_type: ItsRoadType::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItsCctvCamera {
    pub name: String,
    pub stream_url: String,
    pub lat: f64,
    pub lon: f64,
    pub format: Option<String>,
    pub cctv_type: Option<String>,
    pub resolution: Option<String>,
    pub road_section_id: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ItsCctvQueryResult {
    pub road_type: ItsRoadType,
    pub fetched_at: DateTime<Utc>,
    pub cameras: Vec<ItsCctvCamera>,
}

pub struct ItsCctvManager {
    pub settings: ItsCctvSettings,
    pending: bool,
    pub status_message: String,
    tx: Sender<Result<ItsCctvQueryResult, String>>,
    rx: Receiver<Result<ItsCctvQueryResult, String>>,
}

impl Default for ItsCctvManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: ItsCctvSettings::default(),
            pending: false,
            status_message: "ITS CCTV layer idle".into(),
            tx,
            rx,
        }
    }
}

impl ItsCctvManager {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request_import(&mut self, bounds: GeoBounds) -> Result<(), String> {
        if self.pending {
            return Err("ITS CCTV import already running".into());
        }

        let api_key = if self.settings.api_key.trim().is_empty() {
            env::var("VANTAGE_ITS_API_KEY").map_err(|_| "Missing ITS API key".to_string())?
        } else {
            self.settings.api_key.trim().to_owned()
        };
        let road_type = self.settings.road_type;
        let tx = self.tx.clone();
        self.pending = true;
        self.status_message = "Querying ITS CCTV…".into();

        thread::spawn(move || {
            let result = fetch_cctv(bounds, &api_key, road_type);
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub fn drain_results(&mut self) -> Option<Result<ItsCctvQueryResult, String>> {
        let mut latest = None;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            latest = Some(result);
        }

        if let Some(result) = &latest {
            match result {
                Ok(query) => {
                    self.status_message =
                        format!("ITS CCTV: {} cameras fetched", query.cameras.len());
                }
                Err(error) => {
                    self.status_message = format!("ITS CCTV fetch failed: {error}");
                }
            }
        }

        latest
    }
}

fn fetch_cctv(
    bounds: GeoBounds,
    api_key: &str,
    road_type: ItsRoadType,
) -> Result<ItsCctvQueryResult, String> {
    let bounds = bounds.normalized();
    let response = http_client()
        .get("https://openapi.its.go.kr:9443/cctvInfo")
        .query(&[
            ("apiKey", api_key.to_owned()),
            ("type", road_type.as_api_value().to_owned()),
            ("cctvType", "4".to_owned()),
            ("minX", bounds.lomin.to_string()),
            ("maxX", bounds.lomax.to_string()),
            ("minY", bounds.lamin.to_string()),
            ("maxY", bounds.lamax.to_string()),
            ("getType", "json".to_owned()),
        ])
        .send()
        .map_err(|error| format!("ITS CCTV request failed: {error}"))?;

    let status = response.status();
    let body_text = response
        .text()
        .map_err(|error| format!("Failed to read ITS CCTV response: {error}"))?;
    let body: Value = serde_json::from_str(&body_text)
        .map_err(|error| format!("Failed to decode ITS CCTV response: {error}"))?;

    if !status.is_success() {
        return Err(format!("ITS CCTV request failed with {status}"));
    }
    if let Some(error) = extract_response_error(&body) {
        return Err(error);
    }

    let cameras = extract_items(&body)
        .into_iter()
        .filter_map(parse_camera)
        .collect::<Vec<_>>();

    Ok(ItsCctvQueryResult {
        road_type,
        fetched_at: Utc::now(),
        cameras,
    })
}

fn extract_response_error(body: &Value) -> Option<String> {
    let code = body
        .pointer("/response/header/resultCode")
        .and_then(value_as_stringish)
        .or_else(|| {
            body.pointer("/response/resultCode")
                .and_then(value_as_stringish)
        });
    let message = body
        .pointer("/response/header/resultMsg")
        .and_then(value_as_stringish)
        .or_else(|| {
            body.pointer("/response/resultMsg")
                .and_then(value_as_stringish)
        });

    match code.as_deref() {
        Some("0") | Some("00") | Some("SUCCESS") | None => None,
        Some(code) => Some(message.unwrap_or_else(|| format!("ITS CCTV API returned {code}"))),
    }
}

fn extract_items(body: &Value) -> Vec<&Value> {
    let candidates = [
        body.pointer("/response/data"),
        body.pointer("/response/body/items"),
        body.pointer("/response/body/items/item"),
        body.pointer("/body/items"),
        body.pointer("/data"),
    ];

    for candidate in candidates.into_iter().flatten() {
        if let Some(items) = candidate.as_array() {
            return items.iter().collect();
        }
        if candidate.is_object() {
            return vec![candidate];
        }
    }

    Vec::new()
}

fn parse_camera(item: &Value) -> Option<ItsCctvCamera> {
    let name = find_string(item, &["cctvname", "name"])?;
    let stream_url = find_string(item, &["cctvurl", "stream_url", "url"])?;
    let lon = find_f64(item, &["coordx", "x", "lon", "longitude"])?;
    let lat = find_f64(item, &["coordy", "y", "lat", "latitude"])?;

    Some(ItsCctvCamera {
        name,
        stream_url,
        lat,
        lon,
        format: find_string(item, &["cctvformat", "format"]),
        cctv_type: find_string(item, &["cctvtype", "cctvType"]),
        resolution: find_string(item, &["cctvresolution", "resolution"]),
        road_section_id: find_string(item, &["roadsectionid", "routeNo"]),
        created_at: find_string(item, &["filecreatetime", "createdate", "created_at"]),
    })
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
        .danger_accept_invalid_certs(true)
        .build()
        .expect("ITS CCTV HTTP client should build")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{extract_items, parse_camera};

    #[test]
    fn extracts_camera_from_response_data_shape() {
        let payload = json!({
            "response": {
                "data": [{
                    "cctvname": "Seoul Camera",
                    "cctvurl": "https://example.com/live.m3u8",
                    "coordx": 126.97,
                    "coordy": 37.56,
                    "cctvformat": "HLS",
                    "cctvtype": 4
                }]
            }
        });

        let items = extract_items(&payload);
        let camera = parse_camera(items[0]).expect("camera should parse");

        assert_eq!(camera.name, "Seoul Camera");
        assert_eq!(camera.stream_url, "https://example.com/live.m3u8");
        assert_eq!(camera.lon, 126.97);
        assert_eq!(camera.lat, 37.56);
        assert_eq!(camera.cctv_type.as_deref(), Some("4"));
    }

    #[test]
    fn extracts_camera_from_body_items_shape() {
        let payload = json!({
            "response": {
                "body": {
                    "items": [{
                        "name": "Busan Camera",
                        "url": "https://example.com/busan.m3u8",
                        "x": "129.07",
                        "y": "35.18"
                    }]
                }
            }
        });

        let items = extract_items(&payload);
        let camera = parse_camera(items[0]).expect("camera should parse");

        assert_eq!(camera.name, "Busan Camera");
        assert_eq!(camera.stream_url, "https://example.com/busan.m3u8");
    }
}
