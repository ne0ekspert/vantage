use chrono::{DateTime, Utc};
use egui::Color32;
use serde_json::json;

use super::merge_imported_features;
use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};
use crate::satellites::{SatellitePosition, SatelliteQueryResult, SatelliteSource};

#[derive(Debug)]
pub struct SatelliteImportResult {
    pub layer_id: String,
    pub added_feature_count: usize,
}

pub fn apply_satellites(
    workspace: &mut Workspace,
    query: SatelliteQueryResult,
) -> SatelliteImportResult {
    let layer_id = ensure_satellite_layer(workspace, query.source);

    let fetched_at = query.fetched_at;
    let source = query.source;
    let selected_feature_id = merge_imported_features(
        workspace,
        &layer_id,
        query.satellites.into_iter().map(|satellite| {
            let key = satellite_record_key(source, &satellite);
            let feature = satellite_feature(&layer_id, source, satellite, fetched_at, &key);
            (key, feature)
        }),
        satellite_feature_key,
    );

    if selected_feature_id.is_some() {
        workspace.app_state.ui.selected_feature_id = selected_feature_id;
    }

    workspace.recalculate_timeline_bounds();

    SatelliteImportResult {
        layer_id,
        added_feature_count: satellite_feature_count(workspace, source),
    }
}

pub fn clear_satellite_layer(workspace: &mut Workspace, source: SatelliteSource) -> usize {
    let Some(layer_id) = satellite_layer_id(workspace, source).map(str::to_owned) else {
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

pub fn satellite_feature_count(workspace: &Workspace, source: SatelliteSource) -> usize {
    let Some(layer_id) = satellite_layer_id(workspace, source) else {
        return 0;
    };
    workspace
        .features
        .iter()
        .filter(|feature| feature.layer_id == layer_id)
        .count()
}

fn ensure_satellite_layer(workspace: &mut Workspace, source: SatelliteSource) -> String {
    if let Some(existing) = workspace
        .layers
        .iter()
        .find(|layer| is_satellite_layer(layer, source))
        .map(|layer| layer.id.clone())
    {
        return existing;
    }

    let mut layer = Layer::new(
        source.layer_name(),
        LayerType::Marker,
        workspace.layers.len() as i32 * 10 + 10,
    );
    layer.style_json = json!({
        "source": source.metadata_key(),
        "source_kind": "api",
        "geometry_kind": "satellite_subpoint",
    });
    let layer_id = layer.id.clone();
    workspace.layers.push(layer);
    layer_id
}

fn satellite_layer_id(workspace: &Workspace, source: SatelliteSource) -> Option<&str> {
    workspace
        .layers
        .iter()
        .find(|layer| is_satellite_layer(layer, source))
        .map(|layer| layer.id.as_str())
}

fn is_satellite_layer(layer: &Layer, source: SatelliteSource) -> bool {
    layer
        .style_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some(source.metadata_key())
        || layer.name == source.layer_name()
}

fn satellite_feature(
    layer_id: &str,
    source: SatelliteSource,
    satellite: SatellitePosition,
    fetched_at: DateTime<Utc>,
    record_key: &str,
) -> Feature {
    let color = match source {
        SatelliteSource::CelesTrak => (
            Color32::from_rgb(250, 245, 255),
            Color32::from_rgb(168, 85, 247),
        ),
        SatelliteSource::SpaceTrack => (
            Color32::from_rgb(236, 253, 245),
            Color32::from_rgb(5, 150, 105),
        ),
    };

    let mut feature = Feature::new(
        layer_id.to_owned(),
        FeatureType::Marker,
        satellite
            .name
            .clone()
            .unwrap_or_else(|| format!("NORAD {}", satellite.norad_id)),
        Geometry::Point(GeoPoint {
            lat: satellite.lat,
            lon: satellite.lon,
            altitude_m: Some(satellite.altitude_km * 1000.0),
        }),
        FeatureStyle::marker(color.0, color.1, 8.0),
    );
    feature.metadata_json = json!({
        "source": source.metadata_key(),
        "provider_record_key": record_key,
        "name": satellite.name,
        "norad_cat_id": satellite.norad_id,
        "object_id": satellite.object_id,
        "latitude": satellite.lat,
        "longitude": satellite.lon,
        "altitude_km": satellite.altitude_km,
        "epoch": satellite.epoch,
        "imported_at": fetched_at.to_rfc3339(),
    });
    feature
}

fn satellite_record_key(source: SatelliteSource, satellite: &SatellitePosition) -> String {
    format!("{}:{}", source.metadata_key(), satellite.norad_id)
}

fn satellite_feature_key(feature: &Feature) -> Option<String> {
    feature
        .metadata_json
        .get("provider_record_key")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            let source = feature
                .metadata_json
                .get("source")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let norad_id = feature
                .metadata_json
                .get("norad_cat_id")
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_i64().map(|number| number.max(0) as u64))
                })?;
            Some(format!("{source}:{norad_id}"))
        })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{apply_satellites, satellite_feature_count};
    use crate::domain::sample_workspace;
    use crate::satellites::{SatellitePosition, SatelliteQueryResult, SatelliteSource};

    #[test]
    fn apply_satellites_preserves_existing_features_and_updates_matches() {
        let mut workspace = sample_workspace();
        let first = apply_satellites(
            &mut workspace,
            SatelliteQueryResult {
                source: SatelliteSource::CelesTrak,
                fetched_at: Utc::now(),
                satellites: vec![SatellitePosition {
                    name: Some("ISS".into()),
                    norad_id: 25544,
                    object_id: Some("1998-067A".into()),
                    lat: 37.5,
                    lon: 126.9,
                    altitude_km: 420.0,
                    epoch: "2026-03-18T00:00:00".into(),
                }],
            },
        );
        let second = apply_satellites(
            &mut workspace,
            SatelliteQueryResult {
                source: SatelliteSource::CelesTrak,
                fetched_at: Utc::now(),
                satellites: vec![
                    SatellitePosition {
                        name: Some("ISS Updated".into()),
                        norad_id: 25544,
                        object_id: Some("1998-067A".into()),
                        lat: 37.7,
                        lon: 127.1,
                        altitude_km: 421.0,
                        epoch: "2026-03-18T01:00:00".into(),
                    },
                    SatellitePosition {
                        name: Some("Hubble".into()),
                        norad_id: 20580,
                        object_id: Some("1990-037B".into()),
                        lat: 38.0,
                        lon: 127.0,
                        altitude_km: 540.0,
                        epoch: "2026-03-18T00:00:00".into(),
                    },
                ],
            },
        );

        assert_eq!(first.layer_id, second.layer_id);
        assert_eq!(
            satellite_feature_count(&workspace, SatelliteSource::CelesTrak),
            2
        );
        let updated = workspace
            .features
            .iter()
            .find(|feature| {
                feature.layer_id == first.layer_id
                    && feature.metadata_json["norad_cat_id"].as_u64() == Some(25544)
            })
            .expect("updated satellite should exist");
        assert_eq!(updated.name, "ISS Updated");
        assert_eq!(
            updated.metadata_json["epoch"].as_str(),
            Some("2026-03-18T01:00:00")
        );
    }
}
