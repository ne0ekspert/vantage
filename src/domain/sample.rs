use chrono::{Duration, Utc};
use egui::Color32;
use serde_json::json;
use uuid::Uuid;

use super::types::*;

pub fn sample_workspace() -> Workspace {
    let now = Utc::now();
    let mut marker_layer = Layer::new("Observations", LayerType::Marker, 10);
    marker_layer.style_json = json!({ "icon": "dot", "intent": "poi" });

    let mut path_layer = Layer::new("Tracks", LayerType::Path, 20);
    path_layer.style_json = json!({ "mode": "air_corridor" });

    let mut polygon_layer = Layer::new("Areas", LayerType::Polygon, 30);
    polygon_layer.style_json = json!({ "fill": "suspicious_zone" });

    let mut image_layer = Layer::new("Overlays", LayerType::ImageOverlay, 40);
    image_layer.style_json = json!({ "source_kind": "still_image" });

    let marker_id = Uuid::new_v4().to_string();
    let path_id = Uuid::new_v4().to_string();
    let polygon_id = Uuid::new_v4().to_string();
    let overlay_id = Uuid::new_v4().to_string();

    let features = vec![
        Feature {
            id: marker_id.clone(),
            layer_id: marker_layer.id.clone(),
            feature_type: FeatureType::Marker,
            name: "Signal Source".into(),
            geometry: Geometry::Point(GeoPoint {
                lat: 37.5665,
                lon: 126.9780,
                altitude_m: Some(25.0),
            }),
            style: FeatureStyle::marker(
                Color32::from_rgb(255, 208, 0),
                Color32::from_rgb(242, 132, 34),
                10.0,
            ),
            metadata_json: json!({
                "confidence": "high",
                "category": "sensor"
            }),
            time_start: Some(now - Duration::minutes(45)),
            time_end: Some(now + Duration::minutes(20)),
        },
        Feature {
            id: path_id.clone(),
            layer_id: path_layer.id.clone(),
            feature_type: FeatureType::Path,
            name: "Flight Corridor".into(),
            geometry: Geometry::Path(vec![
                GeoPoint {
                    lat: 35.1796,
                    lon: 129.0756,
                    altitude_m: Some(2800.0),
                },
                GeoPoint {
                    lat: 36.3504,
                    lon: 127.3845,
                    altitude_m: Some(3400.0),
                },
                GeoPoint {
                    lat: 37.5665,
                    lon: 126.9780,
                    altitude_m: Some(4200.0),
                },
                GeoPoint {
                    lat: 39.0392,
                    lon: 125.7625,
                    altitude_m: Some(3900.0),
                },
            ]),
            style: FeatureStyle::line(Color32::from_rgb(56, 189, 248), 3.5),
            metadata_json: json!({
                "source": "sample",
                "classification": "route"
            }),
            time_start: Some(now - Duration::minutes(20)),
            time_end: Some(now + Duration::minutes(80)),
        },
        Feature {
            id: polygon_id.clone(),
            layer_id: polygon_layer.id.clone(),
            feature_type: FeatureType::Polygon,
            name: "Focus Area".into(),
            geometry: Geometry::Polygon(vec![
                GeoPoint {
                    lat: 37.83,
                    lon: 126.34,
                    altitude_m: Some(0.0),
                },
                GeoPoint {
                    lat: 38.21,
                    lon: 126.91,
                    altitude_m: Some(0.0),
                },
                GeoPoint {
                    lat: 37.75,
                    lon: 127.45,
                    altitude_m: Some(0.0),
                },
                GeoPoint {
                    lat: 37.34,
                    lon: 126.86,
                    altitude_m: Some(0.0),
                },
            ]),
            style: FeatureStyle::polygon(
                Color32::from_rgb(248, 113, 113),
                Color32::from_rgba_premultiplied(239, 68, 68, 44),
            ),
            metadata_json: json!({
                "priority": "review",
                "status": "open"
            }),
            time_start: Some(now - Duration::minutes(90)),
            time_end: Some(now + Duration::minutes(120)),
        },
        Feature {
            id: overlay_id.clone(),
            layer_id: image_layer.id.clone(),
            feature_type: FeatureType::ImageOverlay,
            name: "Drone Snapshot".into(),
            geometry: Geometry::ImageOverlay(ImageOverlayGeometry {
                corners: [
                    GeoPoint {
                        lat: 37.701,
                        lon: 126.872,
                        altitude_m: Some(0.0),
                    },
                    GeoPoint {
                        lat: 37.701,
                        lon: 127.038,
                        altitude_m: Some(0.0),
                    },
                    GeoPoint {
                        lat: 37.579,
                        lon: 127.038,
                        altitude_m: Some(0.0),
                    },
                    GeoPoint {
                        lat: 37.579,
                        lon: 126.872,
                        altitude_m: Some(0.0),
                    },
                ],
                source_label: "sample/drone-frame-001".into(),
            }),
            style: FeatureStyle::polygon(
                Color32::from_rgb(196, 181, 253),
                Color32::from_rgba_premultiplied(167, 139, 250, 30),
            ),
            metadata_json: json!({
                "source_kind": "still_image",
                "expansion_target": "video_overlay"
            }),
            time_start: Some(now - Duration::minutes(15)),
            time_end: Some(now + Duration::minutes(15)),
        },
    ];

    let events = vec![
        Event::new(
            Some(marker_id),
            "Sensor activated",
            now - Duration::minutes(45),
            Some(now - Duration::minutes(5)),
            "alert",
        ),
        Event::new(
            Some(path_id),
            "Route playback window",
            now - Duration::minutes(20),
            Some(now + Duration::minutes(80)),
            "playback",
        ),
        Event::new(
            Some(polygon_id),
            "Area of interest open",
            now - Duration::minutes(90),
            Some(now + Duration::minutes(120)),
            "region",
        ),
        Event::new(
            Some(overlay_id),
            "Imagery available",
            now - Duration::minutes(15),
            Some(now + Duration::minutes(15)),
            "imagery",
        ),
    ];

    let mut workspace = Workspace {
        id: Uuid::new_v4().to_string(),
        name: "Sample Investigation".into(),
        description:
            "Investigation-first workspace skeleton with layers, timeline, and SQLite persistence."
                .into(),
        layers: vec![marker_layer, path_layer, polygon_layer, image_layer],
        features,
        events,
        app_state: PersistedAppState::default(),
    };
    workspace.app_state.ui.selected_feature_id =
        workspace.features.first().map(|feature| feature.id.clone());
    workspace.recalculate_timeline_bounds();
    workspace
}
