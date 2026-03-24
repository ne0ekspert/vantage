use egui::{Button, DragValue, RichText, TextEdit, Ui};
use serde_json::{Map, Value};

use crate::cctv_viewer::ItsCctvViewer;
use crate::domain::{FeatureType, GeoPoint, Geometry, Workspace};
use crate::evidence::{
    clear_evidence_image_line_segments, ensure_evidence_perspective_corners,
    estimate_evidence_location, evidence_clue_text, evidence_image_line_segments,
    is_evidence_feature, pop_evidence_image_line_segment, reset_evidence_perspective_corners,
    set_evidence_clue_text, set_evidence_perspective_corners,
};
use crate::evidence_preview::EvidenceImagePreview;
use crate::interactions::InteractionState;
use crate::traffic::AircraftState;

pub fn show_inspector(
    ui: &mut Ui,
    workspace: &mut Workspace,
    interactions: &InteractionState,
    selected_aircraft: Option<&AircraftState>,
    cctv_viewer: &mut ItsCctvViewer,
    evidence_preview: &mut EvidenceImagePreview,
    status_message: &mut String,
) {
    ui.heading("Inspector");
    ui.separator();

    if let Some(aircraft) = selected_aircraft.or(interactions.selected_aircraft.as_ref()) {
        ui.label(RichText::new("Selected aircraft").strong());
        ui.monospace(aircraft.icao24.to_uppercase());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Callsign");
            ui.monospace(
                aircraft
                    .callsign
                    .as_deref()
                    .unwrap_or("unknown")
                    .trim()
                    .to_string(),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Type");
            ui.monospace("live_aircraft");
        });
        ui.horizontal(|ui| {
            ui.label("Position");
            ui.monospace(format!(
                "{:.4}, {:.4}",
                aircraft.position.lat, aircraft.position.lon
            ));
        });
        if let Some(altitude) = aircraft.geo_altitude_m.or(aircraft.baro_altitude_m) {
            ui.horizontal(|ui| {
                ui.label("Altitude");
                ui.monospace(format!("{altitude:.0} m"));
            });
        }
        if let Some(speed) = aircraft.velocity_mps {
            ui.horizontal(|ui| {
                ui.label("Speed");
                ui.monospace(format!("{speed:.1} m/s"));
            });
        }
        if let Some(heading) = aircraft.heading_deg {
            ui.horizontal(|ui| {
                ui.label("Heading");
                ui.monospace(format!("{heading:.0} deg"));
            });
        }
        if let Some(category) = aircraft.category {
            ui.horizontal(|ui| {
                ui.label("Category");
                ui.monospace(category.to_string());
            });
        }
        return;
    }

    let selected_feature_id = interactions
        .selected_feature_id
        .clone()
        .or_else(|| workspace.app_state.ui.selected_feature_id.clone());

    if let Some(feature_id) = selected_feature_id {
        let layer_name = workspace
            .feature(&feature_id)
            .and_then(|feature| workspace.layer(&feature.layer_id))
            .map(|layer| layer.name.clone())
            .unwrap_or_else(|| "Unknown layer".into());

        let mut recenter_camera_to = None;
        if let Some(feature) = workspace.feature_mut(&feature_id) {
            ui.label(RichText::new("Selected feature").strong());
            ui.monospace(&feature.id);
            ui.add_space(8.0);
            ui.label("Name");
            ui.text_edit_singleline(&mut feature.name);
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("Type");
                ui.monospace(match feature.feature_type {
                    FeatureType::Marker => "marker",
                    FeatureType::Path => "path",
                    FeatureType::Polygon => "polygon",
                    FeatureType::ImageOverlay => "image_overlay",
                });
            });

            ui.horizontal(|ui| {
                ui.label("Layer");
                ui.monospace(&layer_name);
            });

            if let Some(start) = feature.time_start {
                ui.horizontal(|ui| {
                    ui.label("Time");
                    ui.monospace(start.format("%Y-%m-%d %H:%M:%S UTC").to_string());
                });
            }

            cctv_viewer.show_ui(ui, &feature.id);
            show_evidence_ui(ui, feature, evidence_preview, status_message);

            ui.add_space(8.0);
            ui.label(RichText::new("Geometry").strong());
            match &mut feature.geometry {
                Geometry::Point(point) => {
                    if ui.button("Set map position to marker").clicked() {
                        recenter_camera_to = Some(*point);
                    }
                    edit_point(ui, point);
                }
                Geometry::Path(points) => {
                    ui.label(format!("{} control points", points.len()));
                    if let Some(first) = points.first() {
                        ui.monospace(format!("Start {:.4}, {:.4}", first.lat, first.lon));
                    }
                    if let Some(last) = points.last() {
                        ui.monospace(format!("End {:.4}, {:.4}", last.lat, last.lon));
                    }
                }
                Geometry::Polygon(points) => {
                    ui.label(format!("{} polygon vertices", points.len()));
                    if let Some(point) = points.first() {
                        ui.monospace(format!("Anchor {:.4}, {:.4}", point.lat, point.lon));
                    }
                }
                Geometry::ImageOverlay(overlay) => {
                    ui.label(format!("Source {}", overlay.source_label));
                    ui.label(format!("{} corners", overlay.corners.len()));
                }
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Style").strong());
            ui.horizontal(|ui| {
                ui.label("Opacity");
                let fill = feature.style.fill_color();
                ui.colored_label(fill, "Fill");
                let stroke = feature.style.stroke_color();
                ui.colored_label(stroke, "Stroke");
            });
            ui.add(
                DragValue::new(&mut feature.style.stroke_width)
                    .speed(0.1)
                    .prefix("Stroke "),
            );
            ui.add(
                DragValue::new(&mut feature.style.marker_size)
                    .speed(0.1)
                    .prefix("Marker "),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Metadata").strong());
            show_metadata_editor(ui, &mut feature.metadata_json);
        }
        if let Some(point) = recenter_camera_to {
            workspace.app_state.camera.center = point;
            *status_message = format!(
                "Centered map on marker at {:.4}, {:.4}",
                point.lat, point.lon
            );
        }
    } else {
        ui.label("Select a feature in the map or layer list.");
    }
}

fn show_metadata_editor(ui: &mut Ui, metadata: &mut Value) {
    if !metadata.is_object() {
        *metadata = match metadata.take() {
            Value::Null => Value::Object(Map::new()),
            other => {
                let mut map = Map::new();
                map.insert("value".into(), other);
                Value::Object(map)
            }
        };
    }

    let mut rows = metadata
        .as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .map(|(key, value)| (key.clone(), metadata_value_to_text(value)))
        .collect::<Vec<_>>();
    rows.push((String::new(), String::new()));

    let total_width = ui.available_width();
    let spacing = ui.spacing().item_spacing.x;
    let remove_width = 72.0;
    let key_width = (total_width * 0.32).clamp(110.0, 180.0);
    let value_width = (total_width - key_width - remove_width - spacing * 2.0).max(80.0);

    let mut pending_remove = None;
    for (index, (key, value)) in rows.iter_mut().enumerate() {
        ui.scope(|ui| {
            ui.set_width(total_width);
            ui.set_max_width(total_width);
            ui.horizontal(|ui| {
                ui.set_width(total_width);
                ui.set_max_width(total_width);
                ui.set_min_width(total_width);

                ui.add_sized(
                    [key_width, 24.0],
                    TextEdit::singleline(key).hint_text("key"),
                );
                ui.add_sized(
                    [value_width, 24.0],
                    TextEdit::singleline(value).hint_text("value"),
                );

                if key.trim().is_empty() {
                    ui.allocate_space(egui::vec2(remove_width, 24.0));
                } else if ui
                    .add_sized([remove_width, 24.0], Button::new("Remove"))
                    .clicked()
                {
                    pending_remove = Some(index);
                }
            });
        });
    }

    let mut next = Map::new();
    for (index, (key, value)) in rows.into_iter().enumerate() {
        if pending_remove == Some(index) {
            continue;
        }
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        next.insert(key.to_owned(), metadata_text_to_value(&value));
    }

    *metadata = Value::Object(next);
    ui.small("Values accept plain text or JSON literals like numbers, booleans, arrays, and null.");
}

fn metadata_value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn metadata_text_to_value(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::String(String::new());
    }

    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(text.to_owned()))
}

fn edit_point(ui: &mut Ui, point: &mut GeoPoint) {
    ui.horizontal(|ui| {
        ui.label("Latitude");
        ui.add(DragValue::new(&mut point.lat).speed(0.0005));
    });
    ui.horizontal(|ui| {
        ui.label("Longitude");
        ui.add(DragValue::new(&mut point.lon).speed(0.0005));
    });
    let altitude = point.altitude_m.get_or_insert(0.0);
    ui.horizontal(|ui| {
        ui.label("Altitude");
        ui.add(DragValue::new(altitude).speed(1.0).suffix(" m"));
    });
}

fn show_evidence_ui(
    ui: &mut Ui,
    feature: &mut crate::domain::Feature,
    evidence_preview: &mut EvidenceImagePreview,
    status_message: &mut String,
) {
    if !is_evidence_feature(feature) {
        return;
    }

    ui.add_space(8.0);
    ui.label(RichText::new("Evidence").strong());

    evidence_preview.show_ui(ui, feature);

    let line_count = evidence_image_line_segments(feature).len();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(line_count > 0, Button::new("Undo last line"))
            .clicked()
        {
            pop_evidence_image_line_segment(feature);
        }
        if ui
            .add_enabled(line_count > 0, Button::new("Clear lines"))
            .clicked()
        {
            clear_evidence_image_line_segments(feature);
        }
    });
    ui.small(format!(
        "{line_count} projected line{}",
        if line_count == 1 { "" } else { "s" }
    ));

    let mut clue_text = evidence_clue_text(feature).unwrap_or_default().to_owned();
    ui.label("Visible clue");
    if ui.text_edit_singleline(&mut clue_text).changed() {
        set_evidence_clue_text(feature, &clue_text);
    }
    ui.small("Tries EXIF GPS first. If none is present, enter a visible sign, street, storefront, or landmark text from the image.");

    if ui.button("Estimate location").clicked() {
        match estimate_evidence_location(feature) {
            Ok(result) => {
                *status_message = format!("Estimated evidence location: {}", result.display_name);
            }
            Err(error) => {
                *status_message = format!("Evidence estimation failed: {error}");
            }
        }
    }

    if let Some(display_name) = feature
        .metadata_json
        .get("estimated_display_name")
        .and_then(|value| value.as_str())
    {
        ui.label("Estimated match");
        ui.small(display_name);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Perspective quad");
        if ui.button("Reset from marker").clicked() {
            reset_evidence_perspective_corners(feature);
        }
    });
    ui.small(
        "Image corners map to the ground in this order: top-left, top-right, bottom-right, bottom-left.",
    );

    if let Some(mut corners) = ensure_evidence_perspective_corners(feature) {
        let original = corners;
        for (label, corner) in ["Top-left", "Top-right", "Bottom-right", "Bottom-left"]
            .into_iter()
            .zip(corners.iter_mut())
        {
            ui.collapsing(label, |ui| edit_point(ui, corner));
        }
        if corners != original {
            set_evidence_perspective_corners(feature, corners);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{metadata_text_to_value, metadata_value_to_text};

    #[test]
    fn metadata_text_parser_accepts_json_literals_and_plain_text() {
        assert_eq!(metadata_text_to_value("42"), json!(42));
        assert_eq!(metadata_text_to_value("true"), json!(true));
        assert_eq!(metadata_text_to_value("hello"), json!("hello"));
    }

    #[test]
    fn metadata_value_formatter_preserves_strings_and_serializes_structures() {
        assert_eq!(metadata_value_to_text(&json!("hello")), "hello");
        assert_eq!(metadata_value_to_text(&json!(42)), "42");
        assert_eq!(metadata_value_to_text(&json!({"a": 1})), "{\"a\":1}");
    }
}
