use std::env;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use chrono::{NaiveDateTime, Utc};
use reqwest::blocking::Client;
use sgp4::Elements;

use crate::traffic::GeoBounds;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SatelliteSource {
    CelesTrak,
    SpaceTrack,
}

impl SatelliteSource {
    pub fn layer_name(self) -> &'static str {
        match self {
            Self::CelesTrak => "CelesTrak Satellites",
            Self::SpaceTrack => "Space-Track Satellites",
        }
    }

    pub fn metadata_key(self) -> &'static str {
        match self {
            Self::CelesTrak => "celestrak",
            Self::SpaceTrack => "space_track",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CelesTrakSettings {
    pub group: String,
}

impl Default for CelesTrakSettings {
    fn default() -> Self {
        Self {
            group: "stations".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpaceTrackSettings {
    pub identity: String,
    pub password: String,
}

impl Default for SpaceTrackSettings {
    fn default() -> Self {
        Self {
            identity: String::new(),
            password: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SatellitePosition {
    pub name: Option<String>,
    pub norad_id: u64,
    pub object_id: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub altitude_km: f32,
    pub epoch: String,
}

#[derive(Clone, Debug)]
pub struct SatelliteQueryResult {
    pub source: SatelliteSource,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub satellites: Vec<SatellitePosition>,
}

pub struct CelesTrakManager {
    pub settings: CelesTrakSettings,
    pending: bool,
    pub status_message: String,
    tx: Sender<Result<SatelliteQueryResult, String>>,
    rx: Receiver<Result<SatelliteQueryResult, String>>,
}

pub struct SpaceTrackManager {
    pub settings: SpaceTrackSettings,
    pending: bool,
    pub status_message: String,
    tx: Sender<Result<SatelliteQueryResult, String>>,
    rx: Receiver<Result<SatelliteQueryResult, String>>,
}

impl Default for CelesTrakManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: CelesTrakSettings::default(),
            pending: false,
            status_message: "CelesTrak layer idle".into(),
            tx,
            rx,
        }
    }
}

impl Default for SpaceTrackManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            settings: SpaceTrackSettings::default(),
            pending: false,
            status_message: "Space-Track layer idle".into(),
            tx,
            rx,
        }
    }
}

impl CelesTrakManager {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request_import(&mut self, bounds: GeoBounds) -> Result<(), String> {
        if self.pending {
            return Err("CelesTrak import already running".into());
        }

        let group = if self.settings.group.trim().is_empty() {
            env::var("VANTAGE_CELESTRAK_GROUP").unwrap_or_else(|_| "stations".into())
        } else {
            self.settings.group.trim().to_owned()
        };
        let tx = self.tx.clone();
        self.pending = true;
        self.status_message = "Querying CelesTrak…".into();

        thread::spawn(move || {
            let result = fetch_celestrak(bounds, &group);
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub fn drain_results(&mut self) -> Option<Result<SatelliteQueryResult, String>> {
        let mut latest = None;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            latest = Some(result);
        }
        if let Some(result) = &latest {
            match result {
                Ok(query) => {
                    self.status_message =
                        format!("CelesTrak: {} satellites fetched", query.satellites.len());
                }
                Err(error) => {
                    self.status_message = format!("CelesTrak fetch failed: {error}");
                }
            }
        }
        latest
    }
}

impl SpaceTrackManager {
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    pub fn request_import(&mut self, bounds: GeoBounds) -> Result<(), String> {
        if self.pending {
            return Err("Space-Track import already running".into());
        }

        let identity = if self.settings.identity.trim().is_empty() {
            env::var("VANTAGE_SPACETRACK_IDENTITY")
                .map_err(|_| "Missing Space-Track identity".to_string())?
        } else {
            self.settings.identity.trim().to_owned()
        };
        let password = if self.settings.password.trim().is_empty() {
            env::var("VANTAGE_SPACETRACK_PASSWORD")
                .map_err(|_| "Missing Space-Track password".to_string())?
        } else {
            self.settings.password.trim().to_owned()
        };
        let tx = self.tx.clone();
        self.pending = true;
        self.status_message = "Querying Space-Track…".into();

        thread::spawn(move || {
            let result = fetch_spacetrack(bounds, &identity, &password);
            let _ = tx.send(result);
        });

        Ok(())
    }

    pub fn drain_results(&mut self) -> Option<Result<SatelliteQueryResult, String>> {
        let mut latest = None;
        while let Ok(result) = self.rx.try_recv() {
            self.pending = false;
            latest = Some(result);
        }
        if let Some(result) = &latest {
            match result {
                Ok(query) => {
                    self.status_message =
                        format!("Space-Track: {} satellites fetched", query.satellites.len());
                }
                Err(error) => {
                    self.status_message = format!("Space-Track fetch failed: {error}");
                }
            }
        }
        latest
    }
}

fn fetch_celestrak(bounds: GeoBounds, group: &str) -> Result<SatelliteQueryResult, String> {
    let response = http_client()
        .get("https://celestrak.org/NORAD/elements/gp.php")
        .query(&[("GROUP", group), ("FORMAT", "json")])
        .send()
        .map_err(|error| format!("CelesTrak request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Failed to read CelesTrak response: {error}"))?;
    if !status.is_success() {
        return Err(format!("CelesTrak request failed with {status}"));
    }
    let elements: Vec<Elements> = serde_json::from_str(&body)
        .map_err(|error| format!("Failed to decode CelesTrak response: {error}"))?;
    Ok(SatelliteQueryResult {
        source: SatelliteSource::CelesTrak,
        fetched_at: Utc::now(),
        satellites: propagate_visible(elements, bounds),
    })
}

fn fetch_spacetrack(
    bounds: GeoBounds,
    identity: &str,
    password: &str,
) -> Result<SatelliteQueryResult, String> {
    let query = "https://www.space-track.org/basicspacedata/query/class/gp/decay_date/null-val/epoch/%3Enow-30/orderby/norad_cat_id/limit/1000/format/json";
    let response = http_client()
        .post("https://www.space-track.org/ajaxauth/login")
        .form(&[
            ("identity", identity),
            ("password", password),
            ("query", query),
        ])
        .send()
        .map_err(|error| format!("Space-Track request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("Failed to read Space-Track response: {error}"))?;
    if !status.is_success() {
        return Err(format!("Space-Track request failed with {status}"));
    }
    let elements: Vec<Elements> = serde_json::from_str(&body)
        .map_err(|error| format!("Failed to decode Space-Track response: {error}"))?;
    Ok(SatelliteQueryResult {
        source: SatelliteSource::SpaceTrack,
        fetched_at: Utc::now(),
        satellites: propagate_visible(elements, bounds),
    })
}

fn propagate_visible(elements: Vec<Elements>, bounds: GeoBounds) -> Vec<SatellitePosition> {
    let now = Utc::now().naive_utc();
    let sidereal = sgp4::iau_epoch_to_sidereal_time(sgp4::julian_years_since_j2000(&now));
    let bounds = bounds.normalized();

    elements
        .into_iter()
        .filter_map(|element| propagate_subpoint(&element, &now, sidereal))
        .filter(|satellite| {
            satellite.lat >= bounds.lamin
                && satellite.lat <= bounds.lamax
                && satellite.lon >= bounds.lomin
                && satellite.lon <= bounds.lomax
        })
        .collect()
}

fn propagate_subpoint(
    element: &Elements,
    now: &NaiveDateTime,
    sidereal: f64,
) -> Option<SatellitePosition> {
    let constants = sgp4::Constants::from_elements(element).ok()?;
    let minutes = element.datetime_to_minutes_since_epoch(now).ok()?;
    let prediction = constants.propagate(minutes).ok()?;
    let (lat, lon, altitude_km) = eci_to_subpoint(prediction.position, sidereal);
    Some(SatellitePosition {
        name: element.object_name.clone(),
        norad_id: element.norad_id,
        object_id: element.international_designator.clone(),
        lat,
        lon,
        altitude_km: altitude_km as f32,
        epoch: element.datetime.to_string(),
    })
}

fn eci_to_subpoint(position_km: [f64; 3], sidereal: f64) -> (f64, f64, f64) {
    let (sin_theta, cos_theta) = sidereal.sin_cos();
    let x = cos_theta * position_km[0] + sin_theta * position_km[1];
    let y = -sin_theta * position_km[0] + cos_theta * position_km[1];
    let z = position_km[2];
    let radius = (x * x + y * y + z * z).sqrt();
    let lon = y.atan2(x).to_degrees();
    let lat = z.atan2((x * x + y * y).sqrt()).to_degrees();
    let altitude_km = radius - 6378.137;
    (lat, normalize_lon(lon), altitude_km)
}

fn normalize_lon(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 {
        180.0
    } else {
        wrapped
    }
}

fn http_client() -> Client {
    Client::builder()
        .user_agent(concat!("vantage/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("Satellite HTTP client should build")
}

#[cfg(test)]
mod tests {
    use super::{eci_to_subpoint, normalize_lon, SatelliteSource};

    #[test]
    fn normalizes_longitude_to_standard_range() {
        assert_eq!(normalize_lon(190.0), -170.0);
        assert_eq!(normalize_lon(-190.0), 170.0);
    }

    #[test]
    fn converts_simple_eci_point_to_subpoint() {
        let (lat, lon, altitude) = eci_to_subpoint([6378.137 + 500.0, 0.0, 0.0], 0.0);
        assert!(lat.abs() < 1e-6);
        assert!(lon.abs() < 1e-6);
        assert!((altitude - 500.0).abs() < 1e-3);
    }

    #[test]
    fn satellite_source_layer_names_are_stable() {
        assert_eq!(
            SatelliteSource::CelesTrak.layer_name(),
            "CelesTrak Satellites"
        );
        assert_eq!(
            SatelliteSource::SpaceTrack.layer_name(),
            "Space-Track Satellites"
        );
    }
}
