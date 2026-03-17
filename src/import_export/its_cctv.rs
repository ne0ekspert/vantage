use chrono::{DateTime, Utc};
use egui::Color32;
use serde_json::json;

use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};
use crate::its_cctv::{ItsCctvCamera, ItsCctvQueryResult, ItsRoadType};

const ITS_CCTV_LAYER_NAME: &str = "ITS CCTV HLS";

#[derive(Debug)]
pub struct ItsCctvImportResult {
    pub layer_id: String,
    pub added_feature_count: usize,
}

pub fn apply_its_cctv(workspace: &mut Workspace, query: ItsCctvQueryResult) -> ItsCctvImportResult {
    let layer_id = ensure_its_cctv_layer(workspace);
    workspace
        .features
        .retain(|feature| feature.layer_id != layer_id);

    let fetched_at = query.fetched_at;
    let road_type = query.road_type;
    let mut selected_feature_id = None;
    for camera in query.cameras {
        let feature = its_cctv_feature(&layer_id, camera, road_type, fetched_at);
        if selected_feature_id.is_none() {
            selected_feature_id = Some(feature.id.clone());
        }
        workspace.features.push(feature);
    }

    workspace.app_state.ui.selected_feature_id = selected_feature_id;
    workspace.recalculate_timeline_bounds();

    ItsCctvImportResult {
        layer_id,
        added_feature_count: its_cctv_feature_count(workspace),
    }
}

pub fn clear_its_cctv_layer(workspace: &mut Workspace) -> usize {
    let Some(layer_id) = its_cctv_layer_id(workspace).map(str::to_owned) else {
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

pub fn its_cctv_feature_count(workspace: &Workspace) -> usize {
    let Some(layer_id) = its_cctv_layer_id(workspace) else {
        return 0;
    };
    workspace
        .features
        .iter()
        .filter(|feature| feature.layer_id == layer_id)
        .count()
}

fn ensure_its_cctv_layer(workspace: &mut Workspace) -> String {
    if let Some(existing) = workspace
        .layers
        .iter()
        .find(|layer| is_its_cctv_layer(layer))
        .map(|layer| layer.id.clone())
    {
        return existing;
    }

    let mut layer = Layer::new(
        ITS_CCTV_LAYER_NAME,
        LayerType::Marker,
        workspace.layers.len() as i32 * 10 + 10,
    );
    layer.style_json = json!({
        "source": "its_cctv",
        "source_kind": "api",
        "media_kind": "hls",
    });
    let layer_id = layer.id.clone();
    workspace.layers.push(layer);
    layer_id
}

fn its_cctv_layer_id(workspace: &Workspace) -> Option<&str> {
    workspace
        .layers
        .iter()
        .find(|layer| is_its_cctv_layer(layer))
        .map(|layer| layer.id.as_str())
}

fn is_its_cctv_layer(layer: &Layer) -> bool {
    layer
        .style_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some("its_cctv")
        || layer.name == ITS_CCTV_LAYER_NAME
}

fn its_cctv_feature(
    layer_id: &str,
    camera: ItsCctvCamera,
    road_type: ItsRoadType,
    fetched_at: DateTime<Utc>,
) -> Feature {
    let mut feature = Feature::new(
        layer_id.to_owned(),
        FeatureType::Marker,
        camera.name.clone(),
        Geometry::Point(GeoPoint {
            lat: camera.lat,
            lon: camera.lon,
            altitude_m: Some(0.0),
        }),
        FeatureStyle::marker(
            Color32::from_rgb(254, 226, 226),
            Color32::from_rgb(220, 38, 38),
            8.5,
        ),
    );
    feature.metadata_json = json!({
        "source": "its_cctv",
        "road_type": road_type.as_api_value(),
        "name": camera.name,
        "stream_url": camera.stream_url,
        "coordx": camera.lon,
        "coordy": camera.lat,
        "cctvformat": camera.format,
        "cctvtype": camera.cctv_type,
        "resolution": camera.resolution,
        "roadsectionid": camera.road_section_id,
        "filecreatetime": camera.created_at,
        "imported_at": fetched_at.to_rfc3339(),
    });
    feature
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{apply_its_cctv, its_cctv_feature_count};
    use crate::domain::sample_workspace;
    use crate::its_cctv::{ItsCctvCamera, ItsCctvQueryResult, ItsRoadType};

    #[test]
    fn apply_its_cctv_reuses_single_layer_and_replaces_features() {
        let mut workspace = sample_workspace();
        let first = apply_its_cctv(
            &mut workspace,
            ItsCctvQueryResult {
                road_type: ItsRoadType::NationalRoad,
                fetched_at: Utc::now(),
                cameras: vec![ItsCctvCamera {
                    name: "Camera A".into(),
                    stream_url: "https://example.com/a.m3u8".into(),
                    lat: 37.5,
                    lon: 126.9,
                    format: Some("HLS".into()),
                    cctv_type: Some("4".into()),
                    resolution: Some("1280x720".into()),
                    road_section_id: Some("001".into()),
                    created_at: Some("2026-03-17 10:00:00".into()),
                }],
            },
        );

        let second = apply_its_cctv(
            &mut workspace,
            ItsCctvQueryResult {
                road_type: ItsRoadType::Expressway,
                fetched_at: Utc::now(),
                cameras: vec![ItsCctvCamera {
                    name: "Camera B".into(),
                    stream_url: "https://example.com/b.m3u8".into(),
                    lat: 37.6,
                    lon: 127.0,
                    format: Some("HLS".into()),
                    cctv_type: Some("4".into()),
                    resolution: None,
                    road_section_id: None,
                    created_at: None,
                }],
            },
        );

        assert_eq!(first.layer_id, second.layer_id);
        assert_eq!(its_cctv_feature_count(&workspace), 1);
    }
}
