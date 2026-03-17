use chrono::{DateTime, Utc};
use egui::Color32;
use serde_json::json;

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
    workspace
        .features
        .retain(|feature| feature.layer_id != layer_id);

    let fetched_at = query.fetched_at;
    let bounds = query.bounds;
    let mut selected_feature_id = None;

    for network in query.networks {
        let feature = wigle_feature(&layer_id, network, bounds, fetched_at);
        if selected_feature_id.is_none() {
            selected_feature_id = Some(feature.id.clone());
        }
        workspace.features.push(feature);
    }

    workspace.app_state.ui.selected_feature_id = selected_feature_id;
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{apply_wigle_networks, wigle_feature_count};
    use crate::domain::sample_workspace;
    use crate::traffic::GeoBounds;
    use crate::wigle::{WigleNetworkRecord, WigleQueryResult};

    #[test]
    fn apply_wigle_networks_reuses_single_layer_and_replaces_features() {
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
                networks: vec![WigleNetworkRecord {
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
                }],
            },
        );

        assert_eq!(first.layer_id, second.layer_id);
        assert_eq!(wigle_feature_count(&workspace), 1);
    }
}
