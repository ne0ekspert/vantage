use chrono::{DateTime, Duration, Utc};
use egui::Color32;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub description: String,
    pub layers: Vec<Layer>,
    pub features: Vec<Feature>,
    pub events: Vec<Event>,
    pub app_state: PersistedAppState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedAppState {
    pub camera: MapCamera,
    pub timeline: TimelineState,
    pub ui: UiState,
    #[serde(default)]
    pub services: ServiceSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiState {
    pub inspector_open: bool,
    pub layer_panel_open: bool,
    pub selected_feature_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceSettings {
    pub opensky_client_id: String,
    pub opensky_client_secret: String,
    pub wigle_api_name: String,
    pub wigle_api_token: String,
    pub its_api_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineState {
    pub current_time: DateTime<Utc>,
    pub range_start: DateTime<Utc>,
    pub range_end: DateTime<Utc>,
    pub playing: bool,
    pub playback_speed: f32,
    #[serde(default)]
    pub show_only_active: bool,
    #[serde(default = "default_playback_fps_cap")]
    pub playback_fps_cap: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapCamera {
    pub center: GeoPoint,
    pub zoom: f32,
    pub tilt_degrees: f32,
    pub bearing_degrees: f32,
    pub altitude_m: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub layer_type: LayerType,
    pub visible: bool,
    pub z_index: i32,
    pub opacity: f32,
    pub style_json: Value,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LayerType {
    Marker,
    Path,
    Polygon,
    ImageOverlay,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub layer_id: String,
    pub feature_type: FeatureType,
    pub name: String,
    pub geometry: Geometry,
    pub style: FeatureStyle,
    pub metadata_json: Value,
    pub time_start: Option<DateTime<Utc>>,
    pub time_end: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FeatureType {
    Marker,
    Path,
    Polygon,
    ImageOverlay,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Geometry {
    Point(GeoPoint),
    Path(Vec<GeoPoint>),
    Polygon(Vec<GeoPoint>),
    ImageOverlay(ImageOverlayGeometry),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageOverlayGeometry {
    pub corners: [GeoPoint; 4],
    pub source_label: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
    pub altitude_m: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureStyle {
    pub stroke_rgba: [u8; 4],
    pub fill_rgba: [u8; 4],
    pub stroke_width: f32,
    pub marker_size: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub feature_id: Option<String>,
    pub title: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub event_type: String,
    pub metadata_json: Value,
}

impl Workspace {
    pub fn feature_mut(&mut self, feature_id: &str) -> Option<&mut Feature> {
        self.features
            .iter_mut()
            .find(|feature| feature.id == feature_id)
    }

    pub fn feature(&self, feature_id: &str) -> Option<&Feature> {
        self.features
            .iter()
            .find(|feature| feature.id == feature_id)
    }

    pub fn layer(&self, layer_id: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == layer_id)
    }

    pub fn recalculate_timeline_bounds(&mut self) {
        let mut starts = self
            .events
            .iter()
            .map(|event| event.start_time)
            .chain(
                self.features
                    .iter()
                    .filter_map(|feature| feature.time_start),
            )
            .collect::<Vec<_>>();

        if starts.is_empty() {
            let now = Utc::now();
            self.app_state.timeline.range_start = now - Duration::hours(1);
            self.app_state.timeline.range_end = now + Duration::hours(4);
            self.app_state.timeline.current_time = now;
            return;
        }

        starts.sort();
        let start = *starts.first().unwrap();
        let mut end_candidates = self
            .events
            .iter()
            .filter_map(|event| event.end_time)
            .chain(self.features.iter().filter_map(|feature| feature.time_end))
            .collect::<Vec<_>>();
        end_candidates.sort();
        let end = end_candidates
            .last()
            .copied()
            .unwrap_or(start + Duration::hours(6));

        self.app_state.timeline.range_start = start;
        self.app_state.timeline.range_end = end.max(start + Duration::minutes(30));
        self.app_state.timeline.current_time = self.app_state.timeline.current_time.clamp(
            self.app_state.timeline.range_start,
            self.app_state.timeline.range_end,
        );
    }
}

impl Default for PersistedAppState {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            camera: MapCamera {
                center: GeoPoint {
                    lat: 37.5665,
                    lon: 126.9780,
                    altitude_m: Some(3500.0),
                },
                zoom: 10.0,
                tilt_degrees: 22.0,
                bearing_degrees: -12.0,
                altitude_m: 3500.0,
            },
            timeline: TimelineState {
                current_time: now,
                range_start: now - Duration::hours(1),
                range_end: now + Duration::hours(4),
                playing: false,
                playback_speed: 30.0,
                show_only_active: false,
                playback_fps_cap: default_playback_fps_cap(),
            },
            ui: UiState {
                inspector_open: true,
                layer_panel_open: true,
                selected_feature_id: None,
            },
            services: ServiceSettings::default(),
        }
    }
}

fn default_playback_fps_cap() -> u32 {
    60
}

impl FeatureStyle {
    pub fn marker(stroke: Color32, fill: Color32, marker_size: f32) -> Self {
        Self {
            stroke_rgba: stroke.to_array(),
            fill_rgba: fill.to_array(),
            stroke_width: 2.0,
            marker_size,
        }
    }

    pub fn line(color: Color32, width: f32) -> Self {
        Self {
            stroke_rgba: color.to_array(),
            fill_rgba: Color32::TRANSPARENT.to_array(),
            stroke_width: width,
            marker_size: 7.0,
        }
    }

    pub fn polygon(stroke: Color32, fill: Color32) -> Self {
        Self {
            stroke_rgba: stroke.to_array(),
            fill_rgba: fill.to_array(),
            stroke_width: 2.0,
            marker_size: 7.0,
        }
    }

    pub fn stroke_color(&self) -> Color32 {
        Color32::from_rgba_premultiplied(
            self.stroke_rgba[0],
            self.stroke_rgba[1],
            self.stroke_rgba[2],
            self.stroke_rgba[3],
        )
    }

    pub fn fill_color(&self) -> Color32 {
        Color32::from_rgba_premultiplied(
            self.fill_rgba[0],
            self.fill_rgba[1],
            self.fill_rgba[2],
            self.fill_rgba[3],
        )
    }
}

impl Layer {
    pub fn new(name: impl Into<String>, layer_type: LayerType, z_index: i32) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            layer_type,
            visible: true,
            z_index,
            opacity: 1.0,
            style_json: json!({}),
        }
    }
}

impl Feature {
    pub fn new(
        layer_id: impl Into<String>,
        feature_type: FeatureType,
        name: impl Into<String>,
        geometry: Geometry,
        style: FeatureStyle,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            layer_id: layer_id.into(),
            feature_type,
            name: name.into(),
            geometry,
            style,
            metadata_json: json!({}),
            time_start: None,
            time_end: None,
        }
    }
}

impl Event {
    pub fn new(
        feature_id: Option<String>,
        title: impl Into<String>,
        start_time: DateTime<Utc>,
        end_time: Option<DateTime<Utc>>,
        event_type: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            feature_id,
            title: title.into(),
            start_time,
            end_time,
            event_type: event_type.into(),
            metadata_json: json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::PersistedAppState;

    #[test]
    fn persisted_app_state_accepts_legacy_service_payloads() {
        let app_state: PersistedAppState = serde_json::from_value(json!({
            "camera": {
                "center": { "lat": 37.5, "lon": 126.9, "altitude_m": 3500.0 },
                "zoom": 10.0,
                "tilt_degrees": 22.0,
                "bearing_degrees": -12.0,
                "altitude_m": 3500.0
            },
            "timeline": {
                "current_time": "2026-03-17T12:00:00Z",
                "range_start": "2026-03-17T11:00:00Z",
                "range_end": "2026-03-17T14:00:00Z",
                "playing": false,
                "playback_speed": 30.0,
                "show_only_active": false,
                "playback_fps_cap": 60
            },
            "ui": {
                "inspector_open": true,
                "layer_panel_open": true,
                "selected_feature_id": null
            },
            "services": {
                "opensky_client_id": "legacy-id",
                "opensky_client_secret": "legacy-secret"
            }
        }))
        .expect("legacy payload should deserialize");

        assert_eq!(app_state.services.opensky_client_id, "legacy-id");
        assert_eq!(app_state.services.opensky_client_secret, "legacy-secret");
        assert_eq!(app_state.services.wigle_api_name, "");
        assert_eq!(app_state.services.wigle_api_token, "");
        assert_eq!(app_state.services.its_api_key, "");
    }
}
