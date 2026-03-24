use std::{fs::File, io::BufReader, path::Path};

use chrono::Utc;
use egui::Color32;
use exif::{In, Reader, Tag, Value};
use image::ImageReader;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use thiserror::Error;

use crate::domain::{
    Feature, FeatureStyle, FeatureType, GeoPoint, Geometry, Layer, LayerType, Workspace,
};

const EVIDENCE_LAYER_NAME: &str = "Evidence";
const DEFAULT_PERSPECTIVE_WIDTH_M: f64 = 80.0;
const DEFAULT_PERSPECTIVE_HEIGHT_M: f64 = 60.0;

#[derive(Debug)]
pub struct EvidenceImportResult {
    pub layer_id: String,
    pub feature_name: String,
}

#[derive(Debug)]
pub struct EvidenceEstimateResult {
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct EvidenceImageLineSegment {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

#[derive(Debug)]
struct EvidenceExifLocation {
    point: GeoPoint,
    display_name: String,
}

#[derive(Debug, Error)]
pub enum EvidenceImportError {
    #[error("image file not found: {0}")]
    MissingFile(String),
    #[error("image could not be decoded: {0}")]
    InvalidImage(String),
}

#[derive(Debug, Deserialize)]
struct NominatimCandidate {
    lat: String,
    lon: String,
    display_name: String,
}

pub fn import_evidence_file(
    path: impl AsRef<Path>,
    workspace: &mut Workspace,
) -> Result<EvidenceImportResult, EvidenceImportError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(EvidenceImportError::MissingFile(
            path.to_string_lossy().to_string(),
        ));
    }
    ensure_supported_evidence_image(path)?;

    let layer_id = ensure_evidence_layer(workspace);
    let feature_name = format!(
        "Evidence {}",
        path.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("image")
    );
    let mut feature = Feature::new(
        layer_id.clone(),
        FeatureType::Marker,
        feature_name.clone(),
        Geometry::Point(workspace.app_state.camera.center),
        FeatureStyle::marker(
            Color32::from_rgb(251, 191, 36),
            Color32::from_rgb(217, 119, 6),
            9.0,
        ),
    );
    feature.time_start = Some(Utc::now());
    feature.metadata_json = json!({
        "source": "evidence",
        "evidence_type": "image",
        "image_path": path.to_string_lossy().to_string(),
        "clue_text": "",
        "perspective_corners": default_evidence_perspective_corners_from_point(workspace.app_state.camera.center),
        "projected_lines": [],
        "estimated_display_name": null,
        "estimated_query": null,
        "estimated_at": null,
    });
    let feature_id = feature.id.clone();
    workspace.features.push(feature);
    workspace.app_state.ui.selected_feature_id = Some(feature_id.clone());
    workspace.recalculate_timeline_bounds();

    Ok(EvidenceImportResult {
        layer_id,
        feature_name,
    })
}

pub fn estimate_evidence_location(feature: &mut Feature) -> Result<EvidenceEstimateResult, String> {
    if let Some(exif_location) = extract_evidence_exif_location(feature) {
        feature.geometry = Geometry::Point(exif_location.point);
        if evidence_image_line_segments(feature).is_empty() {
            let _ = reset_evidence_perspective_corners(feature);
        }
        feature.metadata_json["estimated_display_name"] = exif_location.display_name.clone().into();
        feature.metadata_json["estimated_query"] = "EXIF GPS".into();
        feature.metadata_json["estimated_at"] = Utc::now().to_rfc3339().into();

        return Ok(EvidenceEstimateResult {
            display_name: exif_location.display_name,
        });
    }

    let clue = evidence_clue_text(feature)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "No EXIF GPS data found. Enter a visible sign, street name, or place clue first"
                .to_string()
        })?
        .to_owned();

    let candidate = http_client()
        .get("https://nominatim.openstreetmap.org/search")
        .query(&[("q", clue.as_str()), ("format", "jsonv2"), ("limit", "1")])
        .send()
        .map_err(|error| format!("Geocoding request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Geocoding request failed: {error}"))?
        .text()
        .map_err(|error| format!("Failed to read geocoding response: {error}"))
        .and_then(|body| {
            serde_json::from_str::<Vec<NominatimCandidate>>(&body)
                .map_err(|error| format!("Failed to decode geocoding response: {error}"))
        })?
        .into_iter()
        .next()
        .ok_or_else(|| format!("No location match found for \"{clue}\""))?;

    let point = GeoPoint {
        lat: candidate
            .lat
            .parse()
            .map_err(|error| format!("Invalid latitude from geocoder: {error}"))?,
        lon: candidate
            .lon
            .parse()
            .map_err(|error| format!("Invalid longitude from geocoder: {error}"))?,
        altitude_m: Some(0.0),
    };

    feature.geometry = Geometry::Point(point);
    if evidence_image_line_segments(feature).is_empty() {
        let _ = reset_evidence_perspective_corners(feature);
    }
    feature.metadata_json["estimated_display_name"] = candidate.display_name.clone().into();
    feature.metadata_json["estimated_query"] = clue.into();
    feature.metadata_json["estimated_at"] = Utc::now().to_rfc3339().into();

    Ok(EvidenceEstimateResult {
        display_name: candidate.display_name,
    })
}

pub fn is_evidence_feature(feature: &Feature) -> bool {
    feature
        .metadata_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some("evidence")
}

pub fn evidence_clue_text(feature: &Feature) -> Option<&str> {
    feature
        .metadata_json
        .get("clue_text")
        .and_then(|value| value.as_str())
}

pub fn set_evidence_clue_text(feature: &mut Feature, clue_text: &str) {
    if !feature.metadata_json.is_object() {
        feature.metadata_json = json!({});
    }
    feature.metadata_json["clue_text"] = clue_text.to_owned().into();
}

pub fn evidence_image_path(feature: &Feature) -> Option<&str> {
    feature
        .metadata_json
        .get("image_path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn evidence_image_line_segments(feature: &Feature) -> Vec<EvidenceImageLineSegment> {
    feature
        .metadata_json
        .get("projected_lines")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

pub fn set_evidence_image_line_segments(
    feature: &mut Feature,
    segments: &[EvidenceImageLineSegment],
) {
    ensure_evidence_metadata_object(feature);
    feature.metadata_json["projected_lines"] =
        serde_json::to_value(segments).unwrap_or_else(|_| JsonValue::Array(Vec::new()));
}

pub fn pop_evidence_image_line_segment(feature: &mut Feature) -> bool {
    let mut segments = evidence_image_line_segments(feature);
    let removed = segments.pop().is_some();
    if removed {
        set_evidence_image_line_segments(feature, &segments);
    }
    removed
}

pub fn clear_evidence_image_line_segments(feature: &mut Feature) -> bool {
    let had_lines = !evidence_image_line_segments(feature).is_empty();
    if had_lines {
        set_evidence_image_line_segments(feature, &[]);
    }
    had_lines
}

pub fn evidence_perspective_corners(feature: &Feature) -> Option<[GeoPoint; 4]> {
    feature
        .metadata_json
        .get("perspective_corners")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub fn set_evidence_perspective_corners(feature: &mut Feature, corners: [GeoPoint; 4]) {
    ensure_evidence_metadata_object(feature);
    feature.metadata_json["perspective_corners"] =
        serde_json::to_value(corners).expect("evidence perspective corners should serialize");
}

pub fn ensure_evidence_perspective_corners(feature: &mut Feature) -> Option<[GeoPoint; 4]> {
    if let Some(corners) = evidence_perspective_corners(feature) {
        return Some(corners);
    }
    let corners = default_evidence_perspective_corners(feature)?;
    set_evidence_perspective_corners(feature, corners);
    Some(corners)
}

pub fn reset_evidence_perspective_corners(feature: &mut Feature) -> Option<[GeoPoint; 4]> {
    let corners = default_evidence_perspective_corners(feature)?;
    set_evidence_perspective_corners(feature, corners);
    Some(corners)
}

fn ensure_evidence_layer(workspace: &mut Workspace) -> String {
    if let Some(existing) = workspace
        .layers
        .iter()
        .find(|layer| is_evidence_layer(layer))
        .map(|layer| layer.id.clone())
    {
        return existing;
    }

    let mut layer = Layer::new(
        EVIDENCE_LAYER_NAME,
        LayerType::Marker,
        workspace.layers.len() as i32 * 10 + 10,
    );
    layer.style_json = json!({
        "source": "evidence",
        "source_kind": "image"
    });
    let layer_id = layer.id.clone();
    workspace.layers.push(layer);
    layer_id
}

fn is_evidence_layer(layer: &Layer) -> bool {
    layer
        .style_json
        .get("source")
        .and_then(|value| value.as_str())
        == Some("evidence")
        || layer.name == EVIDENCE_LAYER_NAME
}

fn ensure_evidence_metadata_object(feature: &mut Feature) {
    if !feature.metadata_json.is_object() {
        feature.metadata_json = json!({});
    }
}

fn default_evidence_perspective_corners(feature: &Feature) -> Option<[GeoPoint; 4]> {
    match feature.geometry {
        Geometry::Point(point) => Some(default_evidence_perspective_corners_from_point(point)),
        _ => None,
    }
}

fn default_evidence_perspective_corners_from_point(center: GeoPoint) -> [GeoPoint; 4] {
    let half_width_m = DEFAULT_PERSPECTIVE_WIDTH_M * 0.5;
    let half_height_m = DEFAULT_PERSPECTIVE_HEIGHT_M * 0.5;
    [
        offset_geo_point(center, -half_width_m, half_height_m),
        offset_geo_point(center, half_width_m, half_height_m),
        offset_geo_point(center, half_width_m, -half_height_m),
        offset_geo_point(center, -half_width_m, -half_height_m),
    ]
}

fn offset_geo_point(center: GeoPoint, east_m: f64, north_m: f64) -> GeoPoint {
    let lat_offset = north_m / 111_320.0;
    let lon_scale = center.lat.to_radians().cos().abs().max(0.1);
    let lon_offset = east_m / (111_320.0 * lon_scale);
    GeoPoint {
        lat: center.lat + lat_offset,
        lon: center.lon + lon_offset,
        altitude_m: center.altitude_m.or(Some(0.0)),
    }
}

fn http_client() -> Client {
    Client::builder()
        .user_agent(concat!("vantage/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("evidence HTTP client should build")
}

fn extract_evidence_exif_location(feature: &Feature) -> Option<EvidenceExifLocation> {
    let path = Path::new(evidence_image_path(feature)?);
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut reader).ok()?;

    let lat = exif_gps_coordinate(
        exif.get_field(Tag::GPSLatitude, In::PRIMARY)?.value.clone(),
        exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY)?
            .value
            .clone(),
    )?;
    let lon = exif_gps_coordinate(
        exif.get_field(Tag::GPSLongitude, In::PRIMARY)?
            .value
            .clone(),
        exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY)?
            .value
            .clone(),
    )?;
    let altitude_m = exif_gps_altitude(
        exif.get_field(Tag::GPSAltitude, In::PRIMARY)
            .map(|field| field.value.clone()),
        exif.get_field(Tag::GPSAltitudeRef, In::PRIMARY)
            .map(|field| field.value.clone()),
    );

    Some(EvidenceExifLocation {
        point: GeoPoint {
            lat,
            lon,
            altitude_m: altitude_m.map(|value| value as f32),
        },
        display_name: format!("EXIF GPS {:.5}, {:.5}", lat, lon),
    })
}

fn ensure_supported_evidence_image(path: &Path) -> Result<(), EvidenceImportError> {
    ImageReader::open(path)
        .map_err(|error| EvidenceImportError::InvalidImage(error.to_string()))?
        .with_guessed_format()
        .map_err(|error| EvidenceImportError::InvalidImage(error.to_string()))?
        .decode()
        .map_err(|error| EvidenceImportError::InvalidImage(error.to_string()))?;
    Ok(())
}

fn exif_gps_coordinate(value: Value, reference: Value) -> Option<f64> {
    let sign = match exif_ascii(reference)?.to_ascii_uppercase().as_str() {
        "N" | "E" => 1.0,
        "S" | "W" => -1.0,
        _ => return None,
    };

    let Value::Rational(parts) = value else {
        return None;
    };
    if parts.len() < 3 {
        return None;
    }

    let degrees = rational_to_f64(parts[0]);
    let minutes = rational_to_f64(parts[1]);
    let seconds = rational_to_f64(parts[2]);
    Some(sign * (degrees + minutes / 60.0 + seconds / 3600.0))
}

fn exif_gps_altitude(value: Option<Value>, reference: Option<Value>) -> Option<f64> {
    let Value::Rational(parts) = value? else {
        return None;
    };
    let altitude = rational_to_f64(*parts.first()?);
    let sign = match reference {
        Some(Value::Byte(values)) if values.first().copied() == Some(1) => -1.0,
        _ => 1.0,
    };
    Some(sign * altitude)
}

fn exif_ascii(value: Value) -> Option<String> {
    let Value::Ascii(values) = value else {
        return None;
    };
    let bytes = values.first()?;
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim_matches(char::from(0))
        .trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn rational_to_f64(value: exif::Rational) -> f64 {
    value.num as f64 / value.denom as f64
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use egui::Color32;
    use exif::Value;
    use image::{ImageBuffer, Rgba};

    use super::{
        clear_evidence_image_line_segments, ensure_evidence_perspective_corners,
        evidence_image_line_segments, exif_gps_altitude, exif_gps_coordinate, import_evidence_file,
        pop_evidence_image_line_segment, EvidenceImageLineSegment, EvidenceImportError, JsonValue,
    };
    use crate::domain::sample_workspace;

    #[test]
    fn import_evidence_reuses_single_layer() {
        let mut workspace = sample_workspace();
        let base = std::env::temp_dir().join(format!("vantage-evidence-{}", std::process::id()));
        fs::create_dir_all(&base).expect("temp dir should exist");
        let first_path = base.join("first.png");
        let second_path = base.join("second.png");
        write_test_image(&first_path, Color32::RED);
        write_test_image(&second_path, Color32::BLUE);

        let first = import_evidence_file(&first_path, &mut workspace).expect("first import");
        let second = import_evidence_file(&second_path, &mut workspace).expect("second import");

        assert_eq!(first.layer_id, second.layer_id);
    }

    #[test]
    fn import_evidence_rejects_non_image_files() {
        let mut workspace = sample_workspace();
        let initial_feature_count = workspace.features.len();
        let initial_layer_count = workspace.layers.len();
        let base =
            std::env::temp_dir().join(format!("vantage-evidence-invalid-{}", std::process::id()));
        fs::create_dir_all(&base).expect("temp dir should exist");
        let path = base.join("not-an-image.png");
        fs::write(&path, b"not really an image").expect("invalid file should write");

        let result = import_evidence_file(&path, &mut workspace);

        assert!(matches!(result, Err(EvidenceImportError::InvalidImage(_))));
        assert_eq!(workspace.features.len(), initial_feature_count);
        assert_eq!(workspace.layers.len(), initial_layer_count);
    }

    #[test]
    fn exif_coordinate_parser_supports_west_and_south_refs() {
        let lat = exif_gps_coordinate(
            Value::Rational(vec![
                exif::Rational { num: 37, denom: 1 },
                exif::Rational { num: 30, denom: 1 },
                exif::Rational { num: 0, denom: 1 },
            ]),
            Value::Ascii(vec![b"S".to_vec()]),
        )
        .expect("latitude should parse");
        let lon = exif_gps_coordinate(
            Value::Rational(vec![
                exif::Rational { num: 127, denom: 1 },
                exif::Rational { num: 45, denom: 1 },
                exif::Rational { num: 0, denom: 1 },
            ]),
            Value::Ascii(vec![b"W".to_vec()]),
        )
        .expect("longitude should parse");

        assert!((lat + 37.5).abs() < 1e-6);
        assert!((lon + 127.75).abs() < 1e-6);
    }

    #[test]
    fn exif_altitude_parser_respects_below_sea_level_flag() {
        let altitude = exif_gps_altitude(
            Some(Value::Rational(vec![exif::Rational { num: 42, denom: 1 }])),
            Some(Value::Byte(vec![1])),
        )
        .expect("altitude should parse");

        assert_eq!(altitude, -42.0);
    }

    #[test]
    fn evidence_line_segments_round_trip_in_metadata() {
        let mut workspace = sample_workspace();
        let feature = workspace
            .features
            .iter_mut()
            .find(|feature| matches!(feature.geometry, crate::domain::Geometry::Point(_)))
            .expect("sample marker should exist");

        let line = EvidenceImageLineSegment {
            start: [0.1, 0.2],
            end: [0.7, 0.8],
        };
        super::set_evidence_image_line_segments(feature, &[line]);

        assert_eq!(evidence_image_line_segments(feature), vec![line]);
        assert!(pop_evidence_image_line_segment(feature));
        assert!(evidence_image_line_segments(feature).is_empty());
        assert!(!clear_evidence_image_line_segments(feature));
    }

    #[test]
    fn ensure_perspective_corners_populates_defaults_for_point_feature() {
        let mut workspace = sample_workspace();
        let feature = workspace
            .features
            .iter_mut()
            .find(|feature| matches!(feature.geometry, crate::domain::Geometry::Point(_)))
            .expect("sample marker should exist");

        feature.metadata_json["perspective_corners"] = JsonValue::Null;
        let corners = ensure_evidence_perspective_corners(feature).expect("default corners");

        assert_eq!(corners.len(), 4);
        assert!(feature.metadata_json.get("perspective_corners").is_some());
    }

    fn write_test_image(path: &Path, color: Color32) {
        let image =
            ImageBuffer::from_pixel(2, 2, Rgba([color.r(), color.g(), color.b(), color.a()]));
        image.save(path).expect("test image should save");
    }
}
