use chrono::{DateTime, Utc};
use egui::Color32;
use serde_json::json;

use super::merge_imported_features;
use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};
use crate::traffic::GeoBounds;
use crate::wigle::{WigleNetworkRecord, WigleQueryResult};

const WIGLE_LAYER_NAME: &str = "WiGLE Networks";

#[derive(Debug)]
pub struct WigleImportResult {
    pub layer_id: String,
    pub added_feature_count: usize,
}

pub fn apply_wigle_networks(
    workspace: &mut Workspace,
    query: WigleQueryResult,
) -> WigleImportResult {
    let layer_id = ensure_wigle_layer(workspace);

    let fetched_at = query.fetched_at;
    let bounds = query.bounds;
    let selected_feature_id = merge_imported_features(
        workspace,
        &layer_id,
        query.networks.into_iter().map(|network| {
            let key = wigle_record_key(&network);
            let feature = wigle_feature(&layer_id, network, bounds, fetched_at, &key);
            (key, feature)
        }),
        wigle_feature_key,
    );

    if selected_feature_id.is_some() {
        workspace.app_state.ui.selected_feature_id = selected_feature_id;
    }

    workspace.recalculate_timeline_bounds();

    WigleImportResult {
        layer_id,
        added_feature_count: wigle_feature_count(workspace),
    }
}

pub fn clear_wigle_layer(workspace: &mut Workspace) -> usize {
    let Some(layer_id) = wigle_layer_id(workspace).map(str::to_owned) else {
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

pub fn wigle_feature_count(workspace: &Workspace) -> usize {
    let Some(layer_id) = wigle_layer_id(workspace) else {
        return 0;
    };
    workspace
        .features
        .iter()
        .filter(|feature| feature.layer_id == layer_id)
        .count()
}

fn ensure_wigle_layer(workspace: &mut Workspace) -> String {
    if let Some(existing) = workspace
        .layers
        .iter()
        .find(|layer| is_wigle_layer(layer))
        .map(|layer| layer.id.clone())
    {
        return existing;
    }

    let mut layer = Layer::new(
        WIGLE_LAYER_NAME,
        LayerType::Marker,
        workspace.layers.len() as i32 * 10 + 10,
    );
    layer.style_json = json!({
        "source": "wigle",
        "source_kind": "api",
        "geometry_kind": "network_observation",
    });
    let layer_id = layer.id.clone();
    workspace.layers.push(layer);
    layer_id
}

fn wigle_layer_id(workspace: &Workspace) -> Option<&str> {
    workspace
        .layers
        .iter()
        .find(|layer| is_wigle_layer(layer))
        .map(|layer| layer.id.as_str())
}

fn is_wigle_layer(layer: &Layer) -> bool {
    layer
        .style_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some("wigle")
        || layer.name == WIGLE_LAYER_NAME
}

fn wigle_feature(
    layer_id: &str,
    network: WigleNetworkRecord,
    bounds: GeoBounds,
    fetched_at: DateTime<Utc>,
    record_key: &str,
) -> Feature {
    let marker_style = match network.network_type.as_deref().map(str::to_ascii_uppercase) {
        Some(kind) if kind.contains("CELL") => FeatureStyle::marker(
            Color32::from_rgb(255, 237, 213),
            Color32::from_rgb(249, 115, 22),
            8.5,
        ),
        Some(kind) if kind.contains("WIFI") => FeatureStyle::marker(
            Color32::from_rgb(220, 252, 231),
            Color32::from_rgb(34, 197, 94),
            8.0,
        ),
        _ => FeatureStyle::marker(
            Color32::from_rgb(219, 234, 254),
            Color32::from_rgb(59, 130, 246),
            8.0,
        ),
    };

    let name = network
        .ssid
        .as_deref()
        .filter(|ssid| !ssid.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| network.netid.to_uppercase());
    let network_type = network
        .network_type
        .clone()
        .unwrap_or_else(|| "unknown".into());
    let last_seen = network.lastupdt.clone();

    let mut feature = Feature::new(
        layer_id.to_owned(),
        FeatureType::Marker,
        name,
        Geometry::Point(GeoPoint {
            lat: network.lat,
            lon: network.lon,
            altitude_m: Some(0.0),
        }),
        marker_style,
    );
    feature.metadata_json = json!({
        "source": "wigle",
        "provider_record_key": record_key,
        "network_type": network_type,
        "netid": network.netid,
        "ssid": network.ssid,
        "channel": network.channel,
        "encryption": network.encryption,
        "city": network.city,
        "region": network.region,
        "country": network.country,
        "freenet": network.freenet,
        "paynet": network.paynet,
        "lastupdt": last_seen,
        "imported_at": fetched_at.to_rfc3339(),
        "query_bounds": {
            "latrange1": bounds.lamin,
            "latrange2": bounds.lamax,
            "longrange1": bounds.lomin,
            "longrange2": bounds.lomax,
        }
    });
    feature
}

fn wigle_record_key(network: &WigleNetworkRecord) -> String {
    let network_type = network
        .network_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    format!(
        "{}:{}",
        network_type.to_ascii_lowercase(),
        network.netid.trim().to_ascii_lowercase()
    )
}

fn wigle_feature_key(feature: &Feature) -> Option<String> {
    feature
        .metadata_json
        .get("provider_record_key")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            let network_type = feature
                .metadata_json
                .get("network_type")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown");
            feature
                .metadata_json
                .get("netid")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|netid| {
                    format!(
                        "{}:{}",
                        network_type.to_ascii_lowercase(),
                        netid.to_ascii_lowercase()
                    )
                })
        })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{apply_wigle_networks, wigle_feature_count};
    use crate::domain::sample_workspace;
    use crate::traffic::GeoBounds;
    use crate::wigle::{WigleNetworkRecord, WigleQueryResult};

    #[test]
    fn apply_wigle_networks_preserves_existing_features_and_updates_matches() {
        let mut workspace = sample_workspace();
        let bounds = GeoBounds {
            lamin: 37.0,
            lomin: 126.0,
            lamax: 38.0,
            lomax: 127.0,
        };

        let first = apply_wigle_networks(
            &mut workspace,
            WigleQueryResult {
                bounds,
                fetched_at: Utc::now(),
                networks: vec![
                    WigleNetworkRecord {
                        ssid: Some("Cafe".into()),
                        netid: "aa:bb:cc:dd:ee:ff".into(),
                        network_type: Some("WIFI".into()),
                        lat: 37.5,
                        lon: 126.9,
                        channel: Some("6".into()),
                        encryption: Some("wpa2".into()),
                        city: Some("Seoul".into()),
                        region: Some("11".into()),
                        country: Some("KR".into()),
                        lastupdt: Some("2026-03-17 10:00:00".into()),
                        freenet: Some(false),
                        paynet: Some(false),
                    },
                    WigleNetworkRecord {
                        ssid: None,
                        netid: "11:22:33:44:55:66".into(),
                        network_type: Some("CELL".into()),
                        lat: 37.6,
                        lon: 126.8,
                        channel: None,
                        encryption: None,
                        city: Some("Seoul".into()),
                        region: Some("11".into()),
                        country: Some("KR".into()),
                        lastupdt: None,
                        freenet: None,
                        paynet: None,
                    },
                ],
            },
        );

        assert_eq!(wigle_feature_count(&workspace), 2);

        let second = apply_wigle_networks(
            &mut workspace,
            WigleQueryResult {
                bounds,
                fetched_at: Utc::now(),
                networks: vec![
                    WigleNetworkRecord {
                        ssid: Some("Cafe Updated".into()),
                        netid: "aa:bb:cc:dd:ee:ff".into(),
                        network_type: Some("WIFI".into()),
                        lat: 37.55,
                        lon: 126.95,
                        channel: Some("11".into()),
                        encryption: Some("wpa3".into()),
                        city: Some("Seoul".into()),
                        region: Some("11".into()),
                        country: Some("KR".into()),
                        lastupdt: Some("2026-03-18 10:00:00".into()),
                        freenet: Some(false),
                        paynet: Some(false),
                    },
                    WigleNetworkRecord {
                        ssid: Some("Library".into()),
                        netid: "77:88:99:aa:bb:cc".into(),
                        network_type: Some("WIFI".into()),
                        lat: 37.7,
                        lon: 126.7,
                        channel: Some("11".into()),
                        encryption: Some("open".into()),
                        city: Some("Seoul".into()),
                        region: Some("11".into()),
                        country: Some("KR".into()),
                        lastupdt: None,
                        freenet: Some(true),
                        paynet: Some(false),
                    },
                ],
            },
        );

        assert_eq!(first.layer_id, second.layer_id);
        assert_eq!(wigle_feature_count(&workspace), 3);
        let updated = workspace
            .features
            .iter()
            .find(|feature| {
                feature.layer_id == first.layer_id
                    && feature.metadata_json["netid"].as_str() == Some("aa:bb:cc:dd:ee:ff")
            })
            .expect("updated network should exist");
        assert_eq!(updated.name, "Cafe Updated");
        assert_eq!(updated.metadata_json["encryption"].as_str(), Some("wpa3"));
    }
}
