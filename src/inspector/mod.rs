use egui::{DragValue, RichText, Ui};

use crate::domain::{FeatureType, GeoPoint, Geometry, Workspace};
use crate::interactions::InteractionState;
use crate::traffic::AircraftState;

pub fn show_inspector(
    ui: &mut Ui,
    workspace: &mut Workspace,
    interactions: &InteractionState,
    selected_aircraft: Option<&AircraftState>,
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

            ui.add_space(8.0);
            ui.label(RichText::new("Geometry").strong());
            match &mut feature.geometry {
                Geometry::Point(point) => edit_point(ui, point),
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
            let mut metadata_text =
                serde_json::to_string_pretty(&feature.metadata_json).unwrap_or_default();
            ui.code_editor(&mut metadata_text);
        }
    } else {
        ui.label("Select a feature in the map or layer list.");
    }
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
