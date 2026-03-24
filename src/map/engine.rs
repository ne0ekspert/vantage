use egui::{vec2, Color32, Pos2, Rect, Sense, Stroke, Ui};
use glam::{Vec2, Vec3};

use crate::commands::WorkspaceCommand;
use crate::domain::{GeoPoint, Geometry, Workspace};
use crate::evidence::{
    evidence_image_line_segments, evidence_perspective_corners, is_evidence_feature,
};
use crate::interactions::{
    DragTarget, EditMode, InteractionState, PendingGeometryEdit, VertexSelection,
};
use crate::map::osm::OsmTileProvider;
use crate::map::scene::{MapProjector, TilePlacement};
use crate::map::wgpu::{SceneFrame, WgpuMapRenderer};
use crate::timeline::feature_is_active;
use crate::traffic::{AircraftState, GeoBounds, TrafficOverlay};

pub struct MapUiOutput {
    pub edited: bool,
    pub selected_feature_id: Option<String>,
    pub selected_aircraft: Option<AircraftState>,
    pub status: Option<String>,
    pub command: Option<WorkspaceCommand>,
    pub query_bounds: GeoBounds,
}

pub trait MapEngine {
    fn ui(
        &mut self,
        ui: &mut Ui,
        workspace: &mut Workspace,
        interactions: &mut InteractionState,
        traffic: Option<&TrafficOverlay>,
    ) -> MapUiOutput;
}

pub struct Wgpu3dMapEngine {
    pub provider: OsmTileProvider,
    renderer: WgpuMapRenderer,
}

impl Wgpu3dMapEngine {
    pub fn new(provider: OsmTileProvider, render_state: &eframe::egui_wgpu::RenderState) -> Self {
        Self {
            renderer: WgpuMapRenderer::new(render_state, provider.clone(), 1),
            provider,
        }
    }

    fn build_scene(
        &mut self,
        projector: &MapProjector,
        workspace: &Workspace,
        interactions: &InteractionState,
        visible_tiles: &[TilePlacement],
        traffic: Option<&TrafficOverlay>,
    ) -> SceneFrame {
        let mut scene = SceneFrame {
            view_proj: projector.view_proj(),
            ..Default::default()
        };

        for tile in visible_tiles.iter().filter(|tile| tile.cache_path.exists()) {
            scene.push_tile(tile);
        }

        let current_time = workspace.app_state.timeline.current_time;
        let hide_inactive = workspace.app_state.timeline.show_only_active;
        let mut features = workspace.features.iter().collect::<Vec<_>>();
        features.sort_by_key(|feature| {
            workspace
                .layer(&feature.layer_id)
                .map(|layer| layer.z_index)
                .unwrap_or_default()
        });

        for feature in features {
            let layer = match workspace.layer(&feature.layer_id) {
                Some(layer) if layer.visible => layer,
                _ => continue,
            };

            let active = feature_is_active(feature, current_time);
            if hide_inactive && !active {
                continue;
            }

            let selected = interactions.selected_feature_id.as_deref() == Some(feature.id.as_str());
            let opacity_factor = if active { 1.0 } else { 0.22 };
            let opacity = layer.opacity.clamp(0.0, 1.0) * opacity_factor;
            let stroke = color_to_linear(
                feature.style.stroke_color(),
                opacity.max(if selected { 0.86 } else { 0.18 }),
            );
            let fill = color_to_linear(feature.style.fill_color(), opacity);

            match &feature.geometry {
                Geometry::Point(point) => {
                    scene.push_marker(
                        projector.geo_to_world(*point),
                        feature.style.marker_size * 2.6 + if selected { 5.0 } else { 0.0 },
                        fill,
                    );
                }
                Geometry::Path(points) => {
                    let world_points = points
                        .iter()
                        .copied()
                        .map(|point| projector.geo_to_world(point))
                        .collect::<Vec<_>>();
                    scene.push_polyline(
                        &world_points,
                        feature.style.stroke_width * 2.4 + if selected { 1.5 } else { 0.0 },
                        stroke,
                    );
                }
                Geometry::Polygon(points) => {
                    let world_points = points
                        .iter()
                        .copied()
                        .map(|point| projector.geo_to_world(point))
                        .collect::<Vec<_>>();
                    scene.push_triangle_fan(&world_points, fill);
                    scene.push_polyline(
                        &closed_world_points_vec(&world_points),
                        feature.style.stroke_width * 2.0,
                        stroke,
                    );
                }
                Geometry::ImageOverlay(overlay) => {
                    let corners = overlay.corners.map(|corner| projector.geo_to_world(corner));
                    scene.push_colored_quad(corners, fill);
                    scene.push_polyline(
                        &closed_world_points(&corners),
                        2.0 + if selected { 1.0 } else { 0.0 },
                        stroke,
                    );
                }
            }

            if is_evidence_feature(feature) {
                let evidence_segments = projected_evidence_segments(projector, feature);
                if !evidence_segments.is_empty() {
                    let line_color = if selected {
                        [0.98, 0.89, 0.30, 0.98]
                    } else {
                        [0.98, 0.74, 0.18, 0.9]
                    };
                    for segment in evidence_segments {
                        scene.push_polyline(&segment, if selected { 4.8 } else { 3.4 }, line_color);
                    }
                    if let Some(corners) = evidence_perspective_world_corners(projector, feature) {
                        scene.push_polyline(
                            &closed_world_points(&corners),
                            if selected { 2.5 } else { 1.8 },
                            [line_color[0], line_color[1], line_color[2], 0.34],
                        );
                    }
                }
            }
        }

        if let Some(traffic) = traffic {
            for aircraft in &traffic.aircraft {
                let position = projector.geo_to_world(aircraft.position);
                let mut ground_position = projector.geo_to_world(GeoPoint {
                    altitude_m: Some(0.0),
                    ..aircraft.position
                });
                ground_position.y += 1.0;
                let color = [0.24, 0.58, 0.95, 0.98];
                scene.push_vertical_stem(ground_position, position, 2.4, [0.24, 0.58, 0.95, 0.42]);
                scene.push_ground_disc(ground_position, 10.0, [0.24, 0.58, 0.95, 0.28], 20);
                if let Some(heading_deg) = aircraft.heading_deg {
                    scene.push_aircraft(position, 18.0, heading_deg.to_radians(), color);
                } else {
                    scene.push_marker(position, 14.0, color);
                }

                if let Some(trail) = traffic.trails.get(&aircraft.icao24) {
                    let world_points = trail
                        .iter()
                        .copied()
                        .map(|point| projector.geo_to_world(point))
                        .collect::<Vec<_>>();
                    scene.push_polyline(&world_points, 3.0, [0.24, 0.58, 0.95, 0.45]);
                }
            }
        }

        scene
    }

    fn draw_overlay(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        projector: &MapProjector,
        workspace: &Workspace,
        interactions: &InteractionState,
        traffic: Option<&TrafficOverlay>,
    ) {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, Color32::from_rgba_premultiplied(51, 65, 85, 120)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.left_top() + vec2(14.0, 14.0),
            egui::Align2::LEFT_TOP,
            "3D map prototype",
            egui::TextStyle::Body.resolve(&painter.ctx().style()),
            Color32::from_rgb(180, 208, 230),
        );

        let current_time = workspace.app_state.timeline.current_time;
        let hide_inactive = workspace.app_state.timeline.show_only_active;
        for feature in &workspace.features {
            if !workspace
                .layer(&feature.layer_id)
                .map(|layer| layer.visible)
                .unwrap_or(false)
            {
                continue;
            }
            let active = feature_is_active(feature, current_time);
            if hide_inactive && !active {
                continue;
            }
            let Some(anchor) = feature_anchor_screen(projector, feature) else {
                continue;
            };
            painter.text(
                anchor + vec2(10.0, -8.0),
                egui::Align2::LEFT_CENTER,
                &feature.name,
                egui::TextStyle::Small.resolve(&painter.ctx().style()),
                if active {
                    Color32::from_rgb(232, 241, 248)
                } else {
                    Color32::from_gray(130)
                },
            );
        }

        if let Some(traffic) = traffic {
            for aircraft in &traffic.aircraft {
                if let Some(anchor) = projector.geo_to_screen(aircraft.position) {
                    let label = if let Some(callsign) = &aircraft.callsign {
                        let altitude = aircraft
                            .geo_altitude_m
                            .or(aircraft.baro_altitude_m)
                            .unwrap_or_default();
                        format!("{callsign}  {:>5.0}m", altitude)
                    } else {
                        aircraft.icao24.to_uppercase()
                    };
                    painter.circle_filled(anchor, 3.5, Color32::from_rgb(59, 130, 246));
                    if traffic.show_labels {
                        painter.text(
                            anchor + vec2(10.0, 10.0),
                            egui::Align2::LEFT_TOP,
                            label,
                            egui::TextStyle::Small.resolve(&painter.ctx().style()),
                            Color32::from_rgb(147, 197, 253),
                        );
                    }
                }
            }
        }

        if interactions.edit_mode == EditMode::EditGeometry {
            if let Some(feature_id) = interactions.selected_feature_id.as_deref() {
                if let Some(feature) = workspace.feature(feature_id) {
                    for (vertex_index, point) in editable_points(feature) {
                        if let Some(handle_center) = projector.geo_to_screen(point) {
                            let selected = interactions.selected_vertex.as_ref()
                                == Some(&VertexSelection {
                                    feature_id: feature.id.clone(),
                                    vertex_index,
                                });
                            painter.circle_filled(
                                handle_center,
                                if selected { 7.5 } else { 6.0 },
                                if selected {
                                    Color32::from_rgb(250, 204, 21)
                                } else {
                                    Color32::from_rgb(241, 245, 249)
                                },
                            );
                            painter.circle_stroke(
                                handle_center,
                                if selected { 7.5 } else { 6.0 },
                                Stroke::new(2.0, Color32::from_rgb(15, 23, 42)),
                            );
                        }
                    }
                }
            }
        }
    }

    fn feature_hit_test(
        &self,
        projector: &MapProjector,
        workspace: &Workspace,
        pointer: Pos2,
    ) -> Option<String> {
        let current_time = workspace.app_state.timeline.current_time;
        let hide_inactive = workspace.app_state.timeline.show_only_active;
        let mut candidates = workspace.features.iter().collect::<Vec<_>>();
        candidates.sort_by_key(|feature| {
            workspace
                .layer(&feature.layer_id)
                .map(|layer| layer.z_index)
                .unwrap_or_default()
        });
        candidates.reverse();

        for feature in candidates {
            if !workspace
                .layer(&feature.layer_id)
                .map(|layer| layer.visible)
                .unwrap_or(false)
            {
                continue;
            }
            if hide_inactive && !feature_is_active(feature, current_time) {
                continue;
            }

            match &feature.geometry {
                Geometry::Point(point) => {
                    if projector.geo_to_screen(*point).is_some_and(|screen| {
                        screen.distance(pointer) <= feature.style.marker_size + 12.0
                    }) {
                        return Some(feature.id.clone());
                    }
                }
                Geometry::Path(points) => {
                    let projected = points
                        .iter()
                        .filter_map(|point| projector.geo_to_screen(*point))
                        .collect::<Vec<_>>();
                    for segment in projected.windows(2) {
                        if distance_to_segment(pointer, segment[0], segment[1]) <= 9.0 {
                            return Some(feature.id.clone());
                        }
                    }
                }
                Geometry::Polygon(points) => {
                    let projected = points
                        .iter()
                        .filter_map(|point| projector.geo_to_screen(*point))
                        .collect::<Vec<_>>();
                    if point_in_polygon(pointer, &projected) {
                        return Some(feature.id.clone());
                    }
                }
                Geometry::ImageOverlay(overlay) => {
                    let projected = overlay
                        .corners
                        .iter()
                        .filter_map(|point| projector.geo_to_screen(*point))
                        .collect::<Vec<_>>();
                    if point_in_polygon(pointer, &projected) {
                        return Some(feature.id.clone());
                    }
                }
            }
        }
        None
    }

    fn vertex_hit_test(
        &self,
        projector: &MapProjector,
        workspace: &Workspace,
        feature_id: &str,
        pointer: Pos2,
    ) -> Option<VertexSelection> {
        let feature = workspace.feature(feature_id)?;
        for (vertex_index, point) in editable_points(feature) {
            if projector
                .geo_to_screen(point)
                .is_some_and(|screen| screen.distance(pointer) <= 10.0)
            {
                return Some(VertexSelection {
                    feature_id: feature_id.to_owned(),
                    vertex_index,
                });
            }
        }
        None
    }

    fn insert_vertex_command(
        &self,
        projector: &MapProjector,
        workspace: &Workspace,
        feature_id: &str,
        pointer: Pos2,
    ) -> Option<WorkspaceCommand> {
        let feature = workspace.feature(feature_id)?;
        let new_point = projector.screen_to_geo(pointer);
        let before = feature.geometry.clone();
        let mut after = before.clone();
        let segment_index = nearest_segment_index(projector, feature, pointer)?;

        match &mut after {
            Geometry::Path(points) | Geometry::Polygon(points) => {
                points.insert(segment_index + 1, new_point);
            }
            Geometry::Point(_) | Geometry::ImageOverlay(_) => return None,
        }

        Some(WorkspaceCommand::UpdateGeometry {
            feature_id: feature_id.to_owned(),
            before,
            after,
        })
    }

    fn aircraft_hit_test(
        &self,
        projector: &MapProjector,
        traffic: Option<&TrafficOverlay>,
        pointer: Pos2,
    ) -> Option<AircraftState> {
        let traffic = traffic?;
        let mut nearest: Option<(AircraftState, f32)> = None;
        for aircraft in &traffic.aircraft {
            if let Some(screen) = projector.geo_to_screen(aircraft.position) {
                let distance = screen.distance(pointer);
                if distance <= 10.0 {
                    match &nearest {
                        Some((_, best_distance)) if *best_distance <= distance => {}
                        _ => nearest = Some((aircraft.clone(), distance)),
                    }
                }
            }
        }
        nearest.map(|value| value.0)
    }
}

impl MapEngine for Wgpu3dMapEngine {
    fn ui(
        &mut self,
        ui: &mut Ui,
        workspace: &mut Workspace,
        interactions: &mut InteractionState,
        traffic: Option<&TrafficOverlay>,
    ) -> MapUiOutput {
        let desired_size = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let projector = MapProjector::new(rect, workspace, self.provider.tile_size_px as f32);
        let query_bounds = projector.visible_geo_bounds();
        let visible_tiles = projector.visible_tiles(workspace, &self.provider.cache_root);
        self.renderer.request_tiles(&visible_tiles);
        let newly_loaded_tiles = self.renderer.drain_tile_events();
        let scene = self.build_scene(&projector, workspace, interactions, &visible_tiles, traffic);
        painter.add(self.renderer.paint_callback(rect, scene));
        self.draw_overlay(&painter, rect, &projector, workspace, interactions, traffic);

        let mut edited = newly_loaded_tiles > 0;
        let mut status = None;
        let mut selected_feature_id = interactions.selected_feature_id.clone();
        let mut selected_aircraft = interactions.selected_aircraft.clone();
        let mut command = None;
        let pointer_delta = ui.input(|input| input.pointer.delta());
        let shift_held = ui.input(|input| input.modifiers.shift);

        if response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let clicked_aircraft = if interactions.edit_mode != EditMode::EditGeometry {
                    self.aircraft_hit_test(&projector, traffic, pointer)
                } else {
                    None
                };

                if interactions.edit_mode != EditMode::EditGeometry {
                    if let Some(aircraft) = clicked_aircraft.clone() {
                        interactions.select_aircraft(Some(aircraft.clone()));
                        workspace.app_state.ui.selected_feature_id = None;
                        selected_feature_id = None;
                        selected_aircraft = Some(aircraft);
                    }
                }

                if interactions.edit_mode == EditMode::EditGeometry {
                    if let Some(feature_id) = interactions.selected_feature_id.clone() {
                        if shift_held {
                            command = self.insert_vertex_command(
                                &projector,
                                workspace,
                                &feature_id,
                                pointer,
                            );
                            if let Some(WorkspaceCommand::UpdateGeometry { after, .. }) = &command {
                                if let Some(feature) = workspace.feature_mut(&feature_id) {
                                    feature.geometry = after.clone();
                                    edited = true;
                                    status = Some("Inserted vertex on selected geometry".into());
                                }
                            }
                        } else if let Some(vertex) =
                            self.vertex_hit_test(&projector, workspace, &feature_id, pointer)
                        {
                            interactions.selected_vertex = Some(vertex.clone());
                            selected_feature_id = Some(feature_id);
                        }
                    }
                }

                if command.is_none() && clicked_aircraft.is_none() {
                    selected_feature_id = self.feature_hit_test(&projector, workspace, pointer);
                    interactions.select(selected_feature_id.clone());
                    workspace.app_state.ui.selected_feature_id = selected_feature_id.clone();
                    selected_aircraft = None;
                    if let Some(feature_id) = selected_feature_id.as_deref() {
                        if interactions.edit_mode == EditMode::EditGeometry {
                            interactions.selected_vertex =
                                self.vertex_hit_test(&projector, workspace, feature_id, pointer);
                        }
                    } else {
                        interactions.selected_vertex = None;
                        selected_aircraft = None;
                    }
                } else if clicked_aircraft.is_some() {
                    workspace.app_state.ui.selected_feature_id = None;
                    interactions.selected_vertex = None;
                }
            }
        }

        if response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                if interactions.edit_mode == EditMode::EditGeometry {
                    if let Some(feature_id) = interactions.selected_feature_id.clone() {
                        if let Some(vertex) =
                            self.vertex_hit_test(&projector, workspace, &feature_id, pointer)
                        {
                            let before = workspace
                                .feature(&feature_id)
                                .map(|feature| feature.geometry.clone());
                            interactions.dragging_target = Some(DragTarget::Vertex(vertex.clone()));
                            interactions.selected_vertex = Some(vertex);
                            if let Some(before) = before {
                                interactions.pending_geometry_edit =
                                    Some(PendingGeometryEdit { feature_id, before });
                            }
                        }
                    }
                } else if let Some(feature_id) =
                    self.feature_hit_test(&projector, workspace, pointer)
                {
                    if matches!(
                        workspace
                            .feature(&feature_id)
                            .map(|feature| &feature.geometry),
                        Some(Geometry::Point(_))
                    ) {
                        let before = workspace
                            .feature(&feature_id)
                            .map(|feature| feature.geometry.clone());
                        interactions.dragging_target =
                            Some(DragTarget::Feature(feature_id.clone()));
                        if let Some(before) = before {
                            interactions.pending_geometry_edit =
                                Some(PendingGeometryEdit { feature_id, before });
                        }
                    }
                }
            }
        }

        if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                match interactions.dragging_target.clone() {
                    Some(DragTarget::Feature(feature_id)) => {
                        let next_point = projector.screen_to_geo(pointer);
                        if let Some(feature) = workspace.feature_mut(&feature_id) {
                            if let Geometry::Point(point) = &mut feature.geometry {
                                point.lat = next_point.lat;
                                point.lon = next_point.lon;
                                point.altitude_m = next_point.altitude_m;
                                edited = true;
                                status = Some(format!(
                                    "Moved marker to {:.4}, {:.4}",
                                    point.lat, point.lon
                                ));
                            }
                        }
                    }
                    Some(DragTarget::Vertex(vertex)) => {
                        let next_point = projector.screen_to_geo(pointer);
                        if let Some(feature) = workspace.feature_mut(&vertex.feature_id) {
                            if let Some(point) =
                                mutable_vertex_point(&mut feature.geometry, vertex.vertex_index)
                            {
                                point.lat = next_point.lat;
                                point.lon = next_point.lon;
                                point.altitude_m = next_point.altitude_m;
                                edited = true;
                                status = Some(format!(
                                    "Edited vertex {} to {:.4}, {:.4}",
                                    vertex.vertex_index + 1,
                                    point.lat,
                                    point.lon
                                ));
                            }
                        }
                    }
                    None => {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let previous = pointer - pointer_delta;
                            let prev_geo = projector.screen_to_geo(previous);
                            let next_geo = projector.screen_to_geo(pointer);
                            workspace.app_state.camera.center.lat += prev_geo.lat - next_geo.lat;
                            workspace.app_state.camera.center.lon += prev_geo.lon - next_geo.lon;
                            edited = true;
                        }
                    }
                }
            }
        }

        if response.drag_stopped() {
            if let Some(pending) = interactions.pending_geometry_edit.take() {
                if let Some(feature) = workspace.feature(&pending.feature_id) {
                    if pending.before != feature.geometry {
                        command = Some(WorkspaceCommand::UpdateGeometry {
                            feature_id: pending.feature_id.clone(),
                            before: pending.before,
                            after: feature.geometry.clone(),
                        });
                    }
                }
            }
            interactions.dragging_target = None;
        }

        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                workspace.app_state.camera.zoom =
                    (workspace.app_state.camera.zoom + scroll * 0.002).clamp(1.0, 17.0);
                edited = true;
            }
        }

        MapUiOutput {
            edited,
            selected_feature_id,
            selected_aircraft,
            status,
            command,
            query_bounds,
        }
    }
}

fn feature_anchor_screen(
    projector: &MapProjector,
    feature: &crate::domain::Feature,
) -> Option<Pos2> {
    match &feature.geometry {
        Geometry::Point(point) => projector.geo_to_screen(*point),
        Geometry::Path(points) | Geometry::Polygon(points) => points
            .first()
            .copied()
            .and_then(|point| projector.geo_to_screen(point)),
        Geometry::ImageOverlay(overlay) => projector.geo_to_screen(overlay.corners[0]),
    }
}

fn evidence_perspective_world_corners(
    projector: &MapProjector,
    feature: &crate::domain::Feature,
) -> Option<[Vec3; 4]> {
    let corners = evidence_perspective_corners(feature)?;
    Some(corners.map(|corner| projector.geo_to_world(corner)))
}

fn projected_evidence_segments(
    projector: &MapProjector,
    feature: &crate::domain::Feature,
) -> Vec<[Vec3; 2]> {
    let segments = evidence_image_line_segments(feature);
    if segments.is_empty() {
        return Vec::new();
    }

    let Some(quad) = evidence_perspective_world_corners(projector, feature) else {
        return Vec::new();
    };

    segments
        .into_iter()
        .map(|segment| {
            [
                projective_quad_point(quad, segment.start),
                projective_quad_point(quad, segment.end),
            ]
        })
        .collect()
}

fn projective_quad_point(quad: [Vec3; 4], uv: [f32; 2]) -> Vec3 {
    let projected = map_unit_square_to_quad(
        Vec2::new(quad[0].x, quad[0].z),
        Vec2::new(quad[1].x, quad[1].z),
        Vec2::new(quad[2].x, quad[2].z),
        Vec2::new(quad[3].x, quad[3].z),
        uv[0].clamp(0.0, 1.0),
        uv[1].clamp(0.0, 1.0),
    );
    let elevation = (quad[0].y + quad[1].y + quad[2].y + quad[3].y) * 0.25 + 1.0;
    Vec3::new(projected.x, elevation, projected.y)
}

fn map_unit_square_to_quad(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, u: f32, v: f32) -> Vec2 {
    let s = p0 - p1 + p2 - p3;
    if s.length_squared() <= 1e-6 {
        return p0 + (p1 - p0) * u + (p3 - p0) * v;
    }

    let delta1 = p1 - p2;
    let delta2 = p3 - p2;
    let denominator = delta1.x * delta2.y - delta2.x * delta1.y;
    if denominator.abs() <= 1e-6 {
        return p0 + (p1 - p0) * u + (p3 - p0) * v;
    }

    let g = (s.x * delta2.y - delta2.x * s.y) / denominator;
    let h = (delta1.x * s.y - s.x * delta1.y) / denominator;
    let a = p1.x - p0.x + g * p1.x;
    let b = p3.x - p0.x + h * p3.x;
    let c = p0.x;
    let d = p1.y - p0.y + g * p1.y;
    let e = p3.y - p0.y + h * p3.y;
    let f = p0.y;
    let divisor = (g * u + h * v + 1.0).max(1e-6);

    Vec2::new((a * u + b * v + c) / divisor, (d * u + e * v + f) / divisor)
}

fn closed_world_points<const N: usize>(points: &[Vec3; N]) -> Vec<Vec3> {
    let mut closed = points.to_vec();
    if let Some(first) = points.first() {
        closed.push(*first);
    }
    closed
}

fn closed_world_points_vec(points: &[Vec3]) -> Vec<Vec3> {
    let mut closed = points.to_vec();
    if let Some(first) = points.first() {
        closed.push(*first);
    }
    closed
}

fn color_to_linear(color: Color32, alpha_scale: f32) -> [f32; 4] {
    [
        color.r() as f32 / 255.0,
        color.g() as f32 / 255.0,
        color.b() as f32 / 255.0,
        (color.a() as f32 / 255.0 * alpha_scale).clamp(0.0, 1.0),
    ]
}

fn editable_points(feature: &crate::domain::Feature) -> Vec<(usize, GeoPoint)> {
    match &feature.geometry {
        Geometry::Point(point) => vec![(0, *point)],
        Geometry::Path(points) | Geometry::Polygon(points) => {
            points.iter().copied().enumerate().collect()
        }
        Geometry::ImageOverlay(overlay) => overlay.corners.iter().copied().enumerate().collect(),
    }
}

fn mutable_vertex_point(geometry: &mut Geometry, vertex_index: usize) -> Option<&mut GeoPoint> {
    match geometry {
        Geometry::Point(point) if vertex_index == 0 => Some(point),
        Geometry::Path(points) | Geometry::Polygon(points) => points.get_mut(vertex_index),
        Geometry::ImageOverlay(overlay) => overlay.corners.get_mut(vertex_index),
        Geometry::Point(_) => None,
    }
}

fn nearest_segment_index(
    projector: &MapProjector,
    feature: &crate::domain::Feature,
    pointer: Pos2,
) -> Option<usize> {
    let points = match &feature.geometry {
        Geometry::Path(points) | Geometry::Polygon(points) => points,
        Geometry::Point(_) | Geometry::ImageOverlay(_) => return None,
    };

    if points.len() < 2 {
        return None;
    }

    let projected = points
        .iter()
        .filter_map(|point| projector.geo_to_screen(*point))
        .collect::<Vec<_>>();
    let mut best = None;
    let mut best_distance = f32::INFINITY;
    for (index, segment) in projected.windows(2).enumerate() {
        let distance = distance_to_segment(pointer, segment[0], segment[1]);
        if distance < best_distance {
            best_distance = distance;
            best = Some(index);
        }
    }
    if matches!(&feature.geometry, Geometry::Polygon(_)) && projected.len() >= 3 {
        let last_distance = distance_to_segment(pointer, *projected.last().unwrap(), projected[0]);
        if last_distance < best_distance {
            best_distance = last_distance;
            best = Some(projected.len() - 1);
        }
    }
    if best_distance <= 18.0 {
        best
    } else {
        None
    }
}

fn distance_to_segment(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_sq();
    if length_sq <= f32::EPSILON {
        return start.distance(point);
    }
    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    let projection = start + segment * t;
    projection.distance(point)
}

fn point_in_polygon(point: Pos2, polygon: &[Pos2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let intersects = ((polygon[i].y > point.y) != (polygon[j].y > point.y))
            && (point.x
                < (polygon[j].x - polygon[i].x) * (point.y - polygon[i].y)
                    / (polygon[j].y - polygon[i].y).max(0.0001)
                    + polygon[i].x);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use glam::Vec2;

    use super::map_unit_square_to_quad;

    fn assert_close(actual: Vec2, expected: Vec2) {
        assert!(
            (actual - expected).length() <= 1e-4,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn projective_mapping_hits_quad_corners() {
        let quad = [
            Vec2::new(10.0, 20.0),
            Vec2::new(110.0, 30.0),
            Vec2::new(120.0, 130.0),
            Vec2::new(0.0, 120.0),
        ];

        assert_close(
            map_unit_square_to_quad(quad[0], quad[1], quad[2], quad[3], 0.0, 0.0),
            quad[0],
        );
        assert_close(
            map_unit_square_to_quad(quad[0], quad[1], quad[2], quad[3], 1.0, 0.0),
            quad[1],
        );
        assert_close(
            map_unit_square_to_quad(quad[0], quad[1], quad[2], quad[3], 1.0, 1.0),
            quad[2],
        );
        assert_close(
            map_unit_square_to_quad(quad[0], quad[1], quad[2], quad[3], 0.0, 1.0),
            quad[3],
        );
    }

    #[test]
    fn affine_quad_midpoint_stays_centered() {
        let mapped = map_unit_square_to_quad(
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 2.0),
            Vec2::new(0.0, 2.0),
            0.5,
            0.5,
        );
        assert_close(mapped, Vec2::new(2.0, 1.0));
    }
}
