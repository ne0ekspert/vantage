use chrono::{DateTime, Utc};
use egui::Color32;
use serde_json::json;

use super::merge_imported_features;
use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};
use crate::openshipdata::{OpenShipDataQueryResult, OpenShipDataShip};

const OPENSHIPDATA_LAYER_NAME: &str = "OpenShipData";

#[derive(Debug)]
pub struct OpenShipDataImportResult {
    pub layer_id: String,
    pub added_feature_count: usize,
}

pub fn apply_openshipdata(
    workspace: &mut Workspace,
    query: OpenShipDataQueryResult,
) -> OpenShipDataImportResult {
    let layer_id = ensure_openshipdata_layer(workspace);

    let fetched_at = query.fetched_at;
    let selected_feature_id = merge_imported_features(
        workspace,
        &layer_id,
        query.ships.into_iter().map(|ship| {
            let key = openshipdata_record_key(&ship);
            let feature = openshipdata_feature(&layer_id, ship, fetched_at, &key);
            (key, feature)
        }),
        openshipdata_feature_key,
    );

    if selected_feature_id.is_some() {
        workspace.app_state.ui.selected_feature_id = selected_feature_id;
    }

    workspace.recalculate_timeline_bounds();

    OpenShipDataImportResult {
        layer_id,
        added_feature_count: openshipdata_feature_count(workspace),
    }
}

pub fn clear_openshipdata_layer(workspace: &mut Workspace) -> usize {
    let Some(layer_id) = openshipdata_layer_id(workspace).map(str::to_owned) else {
        return 0;
    };

    let before = workspace.features.len();
    workspace
        .features
        .retain(|feature| feature.layer_id != layer_id);
    if workspace
        .app_state
        .ui
        .selected_feature_id
        .as_deref()
        .is_some_and(|feature_id| workspace.feature(feature_id).is_none())
    {
        workspace.app_state.ui.selected_feature_id = None;
    }
    before.saturating_sub(workspace.features.len())
}

pub fn openshipdata_feature_count(workspace: &Workspace) -> usize {
    let Some(layer_id) = openshipdata_layer_id(workspace) else {
        return 0;
    };
    workspace
        .features
        .iter()
        .filter(|feature| feature.layer_id == layer_id)
        .count()
}

fn ensure_openshipdata_layer(workspace: &mut Workspace) -> String {
    if let Some(existing) = workspace
        .layers
        .iter()
        .find(|layer| is_openshipdata_layer(layer))
        .map(|layer| layer.id.clone())
    {
        return existing;
    }

    let mut layer = Layer::new(
        OPENSHIPDATA_LAYER_NAME,
        LayerType::Marker,
        workspace.layers.len() as i32 * 10 + 10,
    );
    layer.style_json = json!({
        "source": "openshipdata",
        "source_kind": "api",
        "geometry_kind": "ship_position",
    });
    let layer_id = layer.id.clone();
    workspace.layers.push(layer);
    layer_id
}

fn openshipdata_layer_id(workspace: &Workspace) -> Option<&str> {
    workspace
        .layers
        .iter()
        .find(|layer| is_openshipdata_layer(layer))
        .map(|layer| layer.id.as_str())
}

fn is_openshipdata_layer(layer: &Layer) -> bool {
    layer
        .style_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some("openshipdata")
        || layer.name == OPENSHIPDATA_LAYER_NAME
}

fn openshipdata_feature(
    layer_id: &str,
    ship: OpenShipDataShip,
    fetched_at: DateTime<Utc>,
    record_key: &str,
) -> Feature {
    let name = ship
        .name
        .clone()
        .or_else(|| ship.mmsi.clone().map(|mmsi| format!("MMSI {mmsi}")))
        .or_else(|| ship.imo.clone().map(|imo| format!("IMO {imo}")))
        .unwrap_or_else(|| "Unknown vessel".into());

    let mut feature = Feature::new(
        layer_id.to_owned(),
        FeatureType::Marker,
        name,
        Geometry::Point(GeoPoint {
            lat: ship.lat,
            lon: ship.lon,
            altitude_m: Some(0.0),
        }),
        FeatureStyle::marker(
            Color32::from_rgb(224, 242, 254),
            Color32::from_rgb(2, 132, 199),
            8.5,
        ),
    );
    feature.metadata_json = json!({
        "source": "openshipdata",
        "provider_record_key": record_key,
        "name": ship.name,
        "mmsi": ship.mmsi,
        "imo": ship.imo,
        "destination": ship.destination,
        "ship_type": ship.ship_type,
        "speed_knots": ship.speed_knots,
        "heading_deg": ship.heading_deg,
        "eta": ship.eta,
        "provider": ship.provider,
        "imported_at": fetched_at.to_rfc3339(),
    });
    feature
}

fn openshipdata_record_key(ship: &OpenShipDataShip) -> String {
    if let Some(mmsi) = ship
        .mmsi
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return format!("mmsi:{}", mmsi.to_ascii_lowercase());
    }
    if let Some(imo) = ship
        .imo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return format!("imo:{}", imo.to_ascii_lowercase());
    }
    if let Some(name) = ship
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let provider = ship
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        return format!(
            "provider:{}:name:{}",
            provider.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        );
    }

    format!("coord:{:.5}:{:.5}", ship.lat, ship.lon)
}

fn openshipdata_feature_key(feature: &Feature) -> Option<String> {
    feature
        .metadata_json
        .get("provider_record_key")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            feature
                .metadata_json
                .get("mmsi")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|mmsi| format!("mmsi:{}", mmsi.to_ascii_lowercase()))
        })
        .or_else(|| {
            feature
                .metadata_json
                .get("imo")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|imo| format!("imo:{}", imo.to_ascii_lowercase()))
        })
        .or_else(|| {
            let provider = feature
                .metadata_json
                .get("provider")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown");
            feature
                .metadata_json
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|name| {
                    format!(
                        "provider:{}:name:{}",
                        provider.to_ascii_lowercase(),
                        name.to_ascii_lowercase()
                    )
                })
        })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{apply_openshipdata, openshipdata_feature_count};
    use crate::domain::sample_workspace;
    use crate::openshipdata::{OpenShipDataQueryResult, OpenShipDataShip};

    #[test]
    fn apply_openshipdata_preserves_existing_features_and_updates_matches() {
        let mut workspace = sample_workspace();
        let first = apply_openshipdata(
            &mut workspace,
            OpenShipDataQueryResult {
                fetched_at: Utc::now(),
                ships: vec![OpenShipDataShip {
                    name: Some("Test Vessel".into()),
                    mmsi: Some("123456789".into()),
                    imo: None,
                    lat: 37.5,
                    lon: 126.9,
                    destination: None,
                    ship_type: Some("Cargo".into()),
                    speed_knots: Some(12.3),
                    heading_deg: Some(180.0),
                    eta: None,
                    provider: Some("all".into()),
                }],
            },
        );

        let second = apply_openshipdata(
            &mut workspace,
            OpenShipDataQueryResult {
                fetched_at: Utc::now(),
                ships: vec![
                    OpenShipDataShip {
                        name: Some("Updated Vessel".into()),
                        mmsi: Some("123456789".into()),
                        imo: None,
                        lat: 37.6,
                        lon: 127.0,
                        destination: Some("Busan".into()),
                        ship_type: Some("Cargo".into()),
                        speed_knots: Some(14.2),
                        heading_deg: Some(90.0),
                        eta: None,
                        provider: Some("all".into()),
                    },
                    OpenShipDataShip {
                        name: Some("New Vessel".into()),
                        mmsi: Some("987654321".into()),
                        imo: None,
                        lat: 37.7,
                        lon: 127.1,
                        destination: None,
                        ship_type: Some("Tanker".into()),
                        speed_knots: None,
                        heading_deg: None,
                        eta: None,
                        provider: Some("all".into()),
                    },
                ],
            },
        );

        assert_eq!(first.layer_id, second.layer_id);
        assert_eq!(openshipdata_feature_count(&workspace), 2);
        let updated = workspace
            .features
            .iter()
            .find(|feature| {
                feature.layer_id == first.layer_id
                    && feature.metadata_json["mmsi"].as_str() == Some("123456789")
            })
            .expect("updated ship should exist");
        assert_eq!(updated.name, "Updated Vessel");
        assert_eq!(updated.metadata_json["destination"].as_str(), Some("Busan"));
    }
}
