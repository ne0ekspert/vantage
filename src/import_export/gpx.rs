use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use egui::Color32;
use gpx::read;
use thiserror::Error;

use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("gpx parse error: {0}")]
    Gpx(#[from] gpx::errors::GpxError),
}

#[derive(Debug)]
pub struct GpxImportResult {
    pub added_layer_id: String,
    pub added_feature_count: usize,
}

pub fn import_gpx_file(
    path: impl AsRef<Path>,
    workspace: &mut Workspace,
) -> Result<GpxImportResult, ImportError> {
    let file = File::open(path.as_ref())?;
    let reader = BufReader::new(file);
    let gpx = read(reader)?;

    let mut path_layer = Layer::new(
        format!(
            "GPX {}",
            path.as_ref()
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("import")
        ),
        LayerType::Path,
        workspace.layers.len() as i32 + 10,
    );
    path_layer.style_json = serde_json::json!({
        "source": "gpx"
    });
    let layer_id = path_layer.id.clone();
    workspace.layers.push(path_layer);

    let mut added = 0usize;

    for track in gpx.tracks {
        for (segment_index, segment) in track.segments.into_iter().enumerate() {
            let points = segment
                .points
                .into_iter()
                .map(|point| GeoPoint {
                    lat: point.point().y(),
                    lon: point.point().x(),
                    altitude_m: point.elevation.map(|value| value as f32),
                })
                .collect::<Vec<_>>();

            if points.len() >= 2 {
                let mut feature = Feature::new(
                    layer_id.clone(),
                    FeatureType::Path,
                    format!(
                        "{} / segment {}",
                        track.name.clone().unwrap_or_else(|| "Track".into()),
                        segment_index + 1
                    ),
                    Geometry::Path(points),
                    FeatureStyle::line(Color32::from_rgb(34, 197, 94), 2.5),
                );
                feature.metadata_json = serde_json::json!({
                    "source": "gpx_track"
                });
                workspace.features.push(feature);
                added += 1;
            }
        }
    }

    for waypoint in gpx.waypoints {
        let mut feature = Feature::new(
            layer_id.clone(),
            FeatureType::Marker,
            waypoint.name.clone().unwrap_or_else(|| "Waypoint".into()),
            Geometry::Point(GeoPoint {
                lat: waypoint.point().y(),
                lon: waypoint.point().x(),
                altitude_m: waypoint.elevation.map(|value| value as f32),
            }),
            FeatureStyle::marker(
                Color32::from_rgb(250, 204, 21),
                Color32::from_rgb(59, 130, 246),
                8.0,
            ),
        );
        feature.metadata_json = serde_json::json!({
            "source": "gpx_waypoint"
        });
        workspace.features.push(feature);
        added += 1;
    }

    workspace.recalculate_timeline_bounds();

    Ok(GpxImportResult {
        added_layer_id: layer_id,
        added_feature_count: added,
    })
}
