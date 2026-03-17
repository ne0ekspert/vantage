use std::collections::HashMap;
use std::env;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde_json::Value;

use crate::domain::GeoPoint;

#[derive(Clone, Copy, Debug)]
pub struct GeoBounds {
    pub lamin: f64,
    pub lomin: f64,
    pub lamax: f64,
    pub lomax: f64,
}

impl GeoBounds {
    pub fn normalized(self) -> Self {
        Self {
            lamin: self.lamin.min(self.lamax).clamp(-85.0, 85.0),
            lomin: self.lomin.min(self.lomax).clamp(-180.0, 180.0),
            lamax: self.lamin.max(self.lamax).clamp(-85.0, 85.0),
            lomax: self.lomin.max(self.lomax).clamp(-180.0, 180.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrafficFilterMode {
    CommercialOnly,
    AllAircraft,
}

#[derive(Clone, Debug)]
pub struct TrafficSettings {
    pub enabled: bool,
    pub refresh_interval_secs: u64,
    pub filter_mode: TrafficFilterMode,
    pub show_labels: bool,
    pub client_id: String,
    pub client_secret: String,
}

impl Default for TrafficSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            refresh_interval_secs: 15,
            filter_mode: TrafficFilterMode::CommercialOnly,
            show_labels: true,
            client_id: String::new(),
            client_secret: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AircraftState {
    pub icao24: String,
    pub callsign: Option<String>,
    pub position: GeoPoint,
    pub baro_altitude_m: Option<f32>,
    pub geo_altitude_m: Option<f32>,
    pub velocity_mps: Option<f32>,
    pub heading_deg: Option<f32>,
    pub on_ground: bool,
    pub category: Option<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct TrafficOverlay {
    pub aircraft: Vec<AircraftState>,
    pub trails: HashMap<String, Vec<GeoPoint>>,
    pub show_labels: bool,
}

#[derive(Clone, Debug)]
struct TrafficSnapshot {
    aircraft: Vec<AircraftState>,
    fetched_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
struct TrafficFetchResult {
    snapshot: TrafficSnapshot,
}

#[derive(Clone, Debug)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

pub struct TrafficManager {
    pub settings: TrafficSettings,
    overlay: TrafficOverlay,
    pending: bool,
    last_request_started: Option<Instant>,
    last_success: Option<DateTime<Utc>>,
    pub status_message: String,
    tx: Sender<Result<TrafficFetchResult, String>>,
    rx: Receiver<Result<TrafficFetchResult, String>>,
    token_cache: Option<CachedToken>,
}

impl Default for TrafficManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: TrafficSettings::default(),
            overlay: TrafficOverlay::default(),
            pending: false,
            last_request_started: None,
            last_success: None,
            status_message: "Live traffic disabled".into(),
            tx,
            rx,
            token_cache: None,
        }
    }
}

impl TrafficManager {
    pub fn aircraft_count(&self) -> usize {
        self.overlay.aircraft.len()
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn overlay(&mut self) -> Option<&TrafficOverlay> {
        self.overlay.show_labels = self.settings.show_labels;
        self.settings.enabled.then_some(&self.overlay)
    }

    pub fn aircraft(&self, icao24: &str) -> Option<&AircraftState> {
        self.overlay
            .aircraft
            .iter()
            .find(|aircraft| aircraft.icao24 == icao24)
    }

    pub fn drain_results(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            match result {
                Ok(fetch) => {
                    self.last_success = Some(fetch.snapshot.fetched_at);
                    self.merge_snapshot(fetch.snapshot);
                    self.status_message = format!(
                        "Live traffic: {} aircraft updated {}",
                        self.overlay.aircraft.len(),
                        self.last_success
                            .map(|time| time.format("%H:%M:%S UTC").to_string())
                            .unwrap_or_else(|| "now".into())
                    );
                    changed = true;
                }
                Err(error) => {
                    self.status_message = format!("Live traffic fetch failed: {error}");
                }
            }
        }
        changed
    }

    pub fn maybe_refresh(&mut self, bounds: GeoBounds, force: bool) {
        if !self.settings.enabled || self.pending {
            return;
        }

        let due = self
            .last_request_started
            .map(|instant| {
                instant.elapsed() >= Duration::from_secs(self.settings.refresh_interval_secs.max(5))
            })
            .unwrap_or(true);
        if !force && !due {
            return;
        }

        let bounds = bounds.normalized();
        let auth = self.prepare_auth_payload();
        let tx = self.tx.clone();
        let filter_mode = self.settings.filter_mode;
        self.pending = true;
        self.last_request_started = Some(Instant::now());
        self.status_message = "Refreshing live traffic…".into();

        thread::spawn(move || {
            let result = fetch_snapshot(bounds, auth, filter_mode)
                .map(|snapshot| TrafficFetchResult { snapshot });
            let _ = tx.send(result);
        });
    }

    fn prepare_auth_payload(&mut self) -> Option<AuthPayload> {
        let client_id = if self.settings.client_id.trim().is_empty() {
            env::var("VANTAGE_OPENSKY_CLIENT_ID").ok()?
        } else {
            self.settings.client_id.trim().to_owned()
        };
        let client_secret = if self.settings.client_secret.trim().is_empty() {
            env::var("VANTAGE_OPENSKY_CLIENT_SECRET").ok()?
        } else {
            self.settings.client_secret.trim().to_owned()
        };

        if let Some(cached) = &self.token_cache {
            if cached.expires_at > Instant::now() + Duration::from_secs(30) {
                return Some(AuthPayload {
                    access_token: cached.access_token.clone(),
                    client_id,
                    client_secret,
                });
            }
        }

        match fetch_access_token(&client_id, &client_secret) {
            Ok((access_token, expires_in)) => {
                self.token_cache = Some(CachedToken {
                    access_token: access_token.clone(),
                    expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(30)),
                });
                Some(AuthPayload {
                    access_token,
                    client_id,
                    client_secret,
                })
            }
            Err(error) => {
                self.status_message =
                    format!("OpenSky auth failed, falling back to anonymous: {error}");
                None
            }
        }
    }

    fn merge_snapshot(&mut self, snapshot: TrafficSnapshot) {
        let mut next_trails = self.overlay.trails.clone();
        for aircraft in &snapshot.aircraft {
            let trail = next_trails.entry(aircraft.icao24.clone()).or_default();
            trail.push(aircraft.position);
            if trail.len() > 16 {
                trail.remove(0);
            }
        }
        next_trails.retain(|icao24, _| {
            snapshot
                .aircraft
                .iter()
                .any(|aircraft| &aircraft.icao24 == icao24)
        });

        self.overlay = TrafficOverlay {
            aircraft: snapshot.aircraft,
            trails: next_trails,
            show_labels: self.settings.show_labels,
        };
    }
}

#[derive(Clone, Debug)]
struct AuthPayload {
    access_token: String,
    #[allow(dead_code)]
    client_id: String,
    #[allow(dead_code)]
    client_secret: String,
}

fn fetch_snapshot(
    bounds: GeoBounds,
    auth: Option<AuthPayload>,
    filter_mode: TrafficFilterMode,
) -> Result<TrafficSnapshot, String> {
    let mut request = opensky_http_client()
        .get("https://opensky-network.org/api/states/all")
        .query(&[
            ("lamin", bounds.lamin.to_string()),
            ("lomin", bounds.lomin.to_string()),
            ("lamax", bounds.lamax.to_string()),
            ("lomax", bounds.lomax.to_string()),
            ("extended", "1".to_string()),
        ]);

    if let Some(auth) = auth {
        request = request.header(AUTHORIZATION, format!("Bearer {}", auth.access_token));
    }

    let response = request.send().map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenSky states request failed with {}",
            response.status()
        ));
    }

    let fetched_at = Utc::now();
    let body: Value = serde_json::from_str(&response.text().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let states = body
        .get("states")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "OpenSky response missing states".to_string())?;

    let mut aircraft = Vec::new();
    for state in states {
        let Some(row) = state.as_array() else {
            continue;
        };
        if let Some(aircraft_state) = parse_state_vector(row) {
            if include_aircraft(&aircraft_state, filter_mode) {
                aircraft.push(aircraft_state);
            }
        }
    }

    Ok(TrafficSnapshot {
        aircraft,
        fetched_at,
    })
}

fn include_aircraft(aircraft: &AircraftState, filter_mode: TrafficFilterMode) -> bool {
    if aircraft.on_ground {
        return false;
    }
    let altitude = aircraft
        .geo_altitude_m
        .or(aircraft.baro_altitude_m)
        .unwrap_or_default();
    let velocity = aircraft.velocity_mps.unwrap_or_default();
    if altitude < 300.0 || velocity < 55.0 {
        return false;
    }

    match filter_mode {
        TrafficFilterMode::AllAircraft => true,
        TrafficFilterMode::CommercialOnly => {
            let category_ok = aircraft
                .category
                .is_some_and(|category| matches!(category, 3 | 4 | 5 | 6));
            let callsign_ok = aircraft.callsign.as_ref().is_some_and(|callsign| {
                let trimmed = callsign.trim();
                let has_alpha = trimmed
                    .chars()
                    .take(3)
                    .all(|char| char.is_ascii_alphabetic());
                let has_digit = trimmed.chars().any(|char| char.is_ascii_digit());
                has_alpha && has_digit
            });
            category_ok || callsign_ok
        }
    }
}

fn parse_state_vector(row: &[Value]) -> Option<AircraftState> {
    let icao24 = row.get(0)?.as_str()?.trim().to_string();
    let callsign = row
        .get(1)
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let _origin_country = row
        .get(2)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let longitude = row.get(5)?.as_f64()?;
    let latitude = row.get(6)?.as_f64()?;
    let baro_altitude_m = row
        .get(7)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);
    let on_ground = row.get(8)?.as_bool()?;
    let velocity_mps = row
        .get(9)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);
    let heading_deg = row
        .get(10)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);
    let _vertical_rate_mps = row
        .get(11)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);
    let geo_altitude_m = row
        .get(13)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32);
    let _last_contact = row
        .get(4)
        .and_then(|value| value.as_i64())
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .unwrap_or_else(Utc::now);
    let category = row
        .get(17)
        .and_then(|value| value.as_i64())
        .map(|value| value as u8);

    Some(AircraftState {
        icao24,
        callsign,
        position: GeoPoint {
            lat: latitude,
            lon: longitude,
            altitude_m: geo_altitude_m.or(baro_altitude_m),
        },
        baro_altitude_m,
        geo_altitude_m,
        velocity_mps,
        heading_deg,
        on_ground,
        category,
    })
}

fn fetch_access_token(client_id: &str, client_secret: &str) -> Result<(String, u64), String> {
    let response = opensky_http_client()
        .post("https://auth.opensky-network.org/auth/realms/opensky-network/protocol/openid-connect/token")
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("token request failed with {}", response.status()));
    }
    let body: Value = serde_json::from_str(&response.text().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let access_token = body
        .get("access_token")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "token response missing access_token".to_string())?;
    let expires_in = body
        .get("expires_in")
        .and_then(|value| value.as_u64())
        .unwrap_or(1800);
    Ok((access_token.to_owned(), expires_in))
}

fn opensky_http_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Vantage/0.1 (dragunov7072@gmail.com)"),
    );
    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .build()
        .expect("OpenSky HTTP client should build")
}
