use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use chrono::Utc;
use eframe::egui::{self, CentralPanel, Context, SidePanel, Slider, TopBottomPanel};
use eframe::{App, CreationContext, Frame};

use crate::commands::{CommandHistory, WorkspaceCommand};
use crate::domain::{sample_workspace, Geometry, LayerType, MapCamera, Workspace};
use crate::import_export::{
    apply_its_cctv, apply_wigle_networks, clear_its_cctv_layer, clear_wigle_layer, import_gpx_file,
    its_cctv_feature_count, wigle_feature_count,
};
use crate::inspector::show_inspector;
use crate::interactions::{EditMode, InteractionState, VertexSelection};
use crate::its_cctv::{ItsCctvManager, ItsRoadType};
use crate::map::{MapEngine, Wgpu3dMapEngine};
use crate::storage::SqliteWorkspaceStore;
use crate::timeline::{
    advance_playback, event_is_active, feature_is_active, scrub_fraction_to_time, time_to_fraction,
};
use crate::traffic::{GeoBounds, TrafficFilterMode, TrafficManager};
use crate::wigle::WigleManager;

pub struct VantageApp {
    workspace: Workspace,
    map_engine: Box<dyn MapEngine>,
    interactions: InteractionState,
    history: CommandHistory,
    store: SqliteWorkspaceStore,
    traffic: TrafficManager,
    wigle: WigleManager,
    its_cctv: ItsCctvManager,
    workspace_path_input: String,
    gpx_path_input: String,
    status_message: String,
    last_frame: Instant,
    last_map_query_bounds: Option<GeoBounds>,
    last_saved_camera: MapCamera,
    last_observed_camera: MapCamera,
    last_camera_change_at: Option<Instant>,
}

impl VantageApp {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        let workspace_path = default_workspace_path();
        let store = SqliteWorkspaceStore;
        let (workspace, status_message) = load_initial_workspace(&store, &workspace_path);
        let cache_root = default_cache_path();
        let provider = crate::map::osm::OsmTileProvider::new(cache_root)
            .expect("cache path should be writable");
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("Vantage requires the wgpu renderer");
        let map_engine = Wgpu3dMapEngine::new(provider, render_state);
        let mut traffic = TrafficManager::default();
        traffic.settings.client_id = workspace.app_state.services.opensky_client_id.clone();
        traffic.settings.client_secret = workspace.app_state.services.opensky_client_secret.clone();
        let mut wigle = WigleManager::default();
        wigle.settings.api_name = workspace.app_state.services.wigle_api_name.clone();
        wigle.settings.api_token = workspace.app_state.services.wigle_api_token.clone();
        let mut its_cctv = ItsCctvManager::default();
        its_cctv.settings.api_key = workspace.app_state.services.its_api_key.clone();
        let initial_camera = workspace.app_state.camera.clone();

        Self {
            interactions: InteractionState {
                selected_feature_id: workspace.app_state.ui.selected_feature_id.clone(),
                ..Default::default()
            },
            history: CommandHistory::default(),
            map_engine: Box::new(map_engine),
            workspace,
            store,
            traffic,
            wigle,
            its_cctv,
            workspace_path_input: workspace_path.to_string_lossy().to_string(),
            gpx_path_input: String::new(),
            status_message,
            last_frame: Instant::now(),
            last_map_query_bounds: None,
            last_saved_camera: initial_camera.clone(),
            last_observed_camera: initial_camera,
            last_camera_change_at: None,
        }
    }

    fn sync_selection_from_workspace(&mut self) {
        self.interactions.selected_feature_id =
            self.workspace.app_state.ui.selected_feature_id.clone();
        if self.interactions.selected_feature_id.is_some() {
            self.interactions.selected_aircraft_icao24 = None;
            self.interactions.selected_aircraft = None;
        }
        if self
            .interactions
            .selected_vertex
            .as_ref()
            .is_some_and(|vertex| {
                Some(vertex.feature_id.as_str()) != self.interactions.selected_feature_id.as_deref()
            })
        {
            self.interactions.selected_vertex = None;
        }
    }

    fn sync_traffic_settings_from_workspace(&mut self) {
        self.traffic.settings.client_id =
            self.workspace.app_state.services.opensky_client_id.clone();
        self.traffic.settings.client_secret = self
            .workspace
            .app_state
            .services
            .opensky_client_secret
            .clone();
        self.wigle.settings.api_name = self.workspace.app_state.services.wigle_api_name.clone();
        self.wigle.settings.api_token = self.workspace.app_state.services.wigle_api_token.clone();
        self.its_cctv.settings.api_key = self.workspace.app_state.services.its_api_key.clone();
    }

    fn save_service_settings(&mut self, success_message: &str) {
        match self.save_workspace_quiet() {
            Ok(()) => {
                self.status_message = success_message.into();
            }
            Err(error) => {
                self.status_message = format!("Failed to save service settings: {error}");
            }
        }
    }

    fn mark_workspace_saved(&mut self) {
        self.last_saved_camera = self.workspace.app_state.camera.clone();
        self.last_observed_camera = self.workspace.app_state.camera.clone();
        self.last_camera_change_at = None;
    }

    fn sync_camera_tracking(&mut self) {
        self.last_saved_camera = self.workspace.app_state.camera.clone();
        self.last_observed_camera = self.workspace.app_state.camera.clone();
        self.last_camera_change_at = None;
    }

    fn autosave_view_state_if_due(&mut self, now: Instant) {
        let current_camera = self.workspace.app_state.camera.clone();
        if current_camera != self.last_observed_camera {
            self.last_observed_camera = current_camera.clone();
            self.last_camera_change_at = Some(now);
        }

        if current_camera == self.last_saved_camera {
            self.last_camera_change_at = None;
            return;
        }

        if self
            .last_camera_change_at
            .is_some_and(|changed_at| now.duration_since(changed_at) >= Duration::from_millis(800))
        {
            if let Err(error) = self.save_workspace_quiet() {
                self.status_message = format!("Auto-save failed: {error}");
            }
        }
    }

    fn start_wigle_import(&mut self) {
        let Some(bounds) = self.last_map_query_bounds else {
            self.status_message =
                "Pan or zoom the map first so WiGLE can use the current view bounds".into();
            return;
        };

        match self.wigle.request_import(bounds) {
            Ok(()) => {
                self.status_message = "Started WiGLE import for the current map view".into();
            }
            Err(error) => {
                self.status_message = error;
            }
        }
    }

    fn start_its_cctv_import(&mut self) {
        let Some(bounds) = self.last_map_query_bounds else {
            self.status_message =
                "Pan or zoom the map first so ITS CCTV can use the current view bounds".into();
            return;
        };

        match self.its_cctv.request_import(bounds) {
            Ok(()) => {
                self.status_message = "Started ITS CCTV import for the current map view".into();
            }
            Err(error) => {
                self.status_message = error;
            }
        }
    }

    fn clear_wigle_import_layer(&mut self) {
        let removed = clear_wigle_layer(&mut self.workspace);
        self.sync_selection_from_workspace();
        if removed == 0 {
            self.status_message = "WiGLE layer is already empty".into();
            return;
        }

        match self.save_workspace_quiet() {
            Ok(()) => {
                self.status_message = format!("Cleared {removed} WiGLE network markers");
            }
            Err(error) => {
                self.status_message =
                    format!("Cleared {removed} WiGLE network markers but failed to save: {error}");
            }
        }
    }

    fn clear_its_cctv_import_layer(&mut self) {
        let removed = clear_its_cctv_layer(&mut self.workspace);
        self.sync_selection_from_workspace();
        if removed == 0 {
            self.status_message = "ITS CCTV layer is already empty".into();
            return;
        }

        match self.save_workspace_quiet() {
            Ok(()) => {
                self.status_message = format!("Cleared {removed} ITS CCTV markers");
            }
            Err(error) => {
                self.status_message =
                    format!("Cleared {removed} ITS CCTV markers but failed to save: {error}");
            }
        }
    }

    fn apply_command(&mut self, command: WorkspaceCommand) {
        match self.history.apply_and_record(&mut self.workspace, command) {
            Ok(label) => {
                self.sync_selection_from_workspace();
                self.status_message = format!("Recorded {label}");
            }
            Err(error) => {
                self.status_message = format!("Command failed: {error}");
            }
        }
    }

    fn undo(&mut self) {
        match self.history.undo(&mut self.workspace) {
            Ok(Some(label)) => {
                self.sync_selection_from_workspace();
                self.status_message = format!("Undid {label}");
            }
            Ok(None) => {
                self.status_message = "Nothing to undo".into();
            }
            Err(error) => {
                self.status_message = format!("Undo failed: {error}");
            }
        }
    }

    fn redo(&mut self) {
        match self.history.redo(&mut self.workspace) {
            Ok(Some(label)) => {
                self.sync_selection_from_workspace();
                self.status_message = format!("Redid {label}");
            }
            Ok(None) => {
                self.status_message = "Nothing to redo".into();
            }
            Err(error) => {
                self.status_message = format!("Redo failed: {error}");
            }
        }
    }

    fn delete_selected_vertex(&mut self) {
        let Some(VertexSelection {
            feature_id,
            vertex_index,
        }) = self.interactions.selected_vertex.clone()
        else {
            self.status_message = "No selected vertex to delete".into();
            return;
        };

        let Some(feature) = self.workspace.feature(&feature_id) else {
            self.status_message = "Selected feature is missing".into();
            return;
        };

        let before = feature.geometry.clone();
        let mut after = before.clone();
        let remove_allowed = match &mut after {
            Geometry::Point(_) => false,
            Geometry::Path(points) => {
                if points.len() <= 2 {
                    false
                } else {
                    points.remove(vertex_index);
                    true
                }
            }
            Geometry::Polygon(points) => {
                if points.len() <= 3 {
                    false
                } else {
                    points.remove(vertex_index);
                    true
                }
            }
            Geometry::ImageOverlay(_) => false,
        };

        if !remove_allowed {
            self.status_message =
                "This geometry cannot remove the selected vertex without becoming invalid".into();
            return;
        }

        self.apply_command(WorkspaceCommand::UpdateGeometry {
            feature_id: feature_id.clone(),
            before,
            after,
        });
        self.interactions.selected_vertex = None;
    }

    fn save_workspace(&mut self) {
        match self
            .store
            .save_to_path(&self.workspace_path_input, &self.workspace)
        {
            Ok(()) => {
                self.mark_workspace_saved();
                self.status_message = format!("Saved workspace to {}", self.workspace_path_input);
            }
            Err(error) => {
                self.status_message = format!("Save failed: {error}");
            }
        }
    }

    fn save_workspace_quiet(&mut self) -> Result<(), String> {
        self.store
            .save_to_path(&self.workspace_path_input, &self.workspace)
            .map_err(|error| error.to_string())?;
        self.mark_workspace_saved();
        Ok(())
    }

    fn open_workspace(&mut self) {
        let path = PathBuf::from(&self.workspace_path_input);
        if !path.exists() {
            self.save_workspace();
            self.status_message = format!(
                "Created new SQLite workspace at {}",
                self.workspace_path_input
            );
            return;
        }

        match self.store.load_from_path(&path) {
            Ok(mut workspace) => {
                workspace.recalculate_timeline_bounds();
                self.workspace = workspace;
                self.history = CommandHistory::default();
                self.sync_selection_from_workspace();
                self.sync_traffic_settings_from_workspace();
                self.sync_camera_tracking();
                self.status_message = format!("Opened workspace {}", path.display());
            }
            Err(error) => {
                self.status_message = format!("Open failed: {error}");
            }
        }
    }

    fn import_gpx(&mut self) {
        match import_gpx_file(&self.gpx_path_input, &mut self.workspace) {
            Ok(result) => {
                self.status_message = format!(
                    "Imported GPX into layer {} with {} feature(s)",
                    result.added_layer_id, result.added_feature_count
                );
            }
            Err(error) => {
                self.status_message = format!("GPX import failed: {error}");
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        let (undo, redo, delete_vertex) = ctx.input(|input| {
            let command = input.modifiers.command;
            let shift = input.modifiers.shift;
            (
                command && input.key_pressed(egui::Key::Z) && !shift,
                (command && input.key_pressed(egui::Key::Y))
                    || (command && shift && input.key_pressed(egui::Key::Z)),
                input.key_pressed(egui::Key::Delete),
            )
        });

        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
        if delete_vertex && self.interactions.edit_mode == EditMode::EditGeometry {
            self.delete_selected_vertex();
        }
    }

    fn render_top_bar(&mut self, ctx: &Context) {
        TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Vantage");
                ui.separator();
                ui.label("SQLite workspace");
                ui.text_edit_singleline(&mut self.workspace_path_input);
                if ui.button("Open/Create").clicked() {
                    self.open_workspace();
                }
                if ui.button("Save").clicked() {
                    self.save_workspace();
                }
                if ui.button("Reset Sample").clicked() {
                    self.workspace = sample_workspace();
                    self.history = CommandHistory::default();
                    self.sync_selection_from_workspace();
                    self.sync_traffic_settings_from_workspace();
                    self.sync_camera_tracking();
                    self.status_message = "Reset to sample workspace.".into();
                }
                ui.separator();
                if ui
                    .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    self.redo();
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Edit mode");
                ui.selectable_value(&mut self.interactions.edit_mode, EditMode::Select, "Select");
                ui.selectable_value(
                    &mut self.interactions.edit_mode,
                    EditMode::EditGeometry,
                    "Geometry",
                );
                ui.separator();
                ui.checkbox(
                    &mut self.workspace.app_state.timeline.show_only_active,
                    "Show active only",
                );
                if ui
                    .add_enabled(
                        self.interactions.selected_vertex.is_some(),
                        egui::Button::new("Delete vertex"),
                    )
                    .clicked()
                {
                    self.delete_selected_vertex();
                }
                ui.separator();
                ui.label("GPX path");
                ui.text_edit_singleline(&mut self.gpx_path_input);
                if ui.button("Import GPX").clicked() {
                    self.import_gpx();
                }
                ui.separator();
                ui.small("Tiles cache automatically while you browse.");
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status_message);
                if self.traffic.settings.enabled {
                    ui.separator();
                    ui.small(&self.traffic.status_message);
                }
            });
        });
    }

    fn render_layer_panel(&mut self, ctx: &Context) {
        SidePanel::left("layers")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.heading("Layers");
                ui.separator();

                let mut pending_move: Option<(usize, isize)> = None;
                let total_layers = self.workspace.layers.len();
                let selected_layer_id = self
                    .interactions
                    .selected_feature_id
                    .as_deref()
                    .and_then(|feature_id| self.workspace.feature(feature_id))
                    .map(|feature| feature.layer_id.clone());
                let mut first_feature_by_layer = HashMap::new();
                for feature in &self.workspace.features {
                    first_feature_by_layer
                        .entry(feature.layer_id.clone())
                        .or_insert_with(|| feature.id.clone());
                }

                let mut pending_selection: Option<String> = None;
                for index in 0..total_layers {
                    let mut select_layer = false;
                    ui.group(|ui| {
                        let layer = &mut self.workspace.layers[index];
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut layer.visible, "");
                            let selected = selected_layer_id.as_deref() == Some(layer.id.as_str());
                            if ui.selectable_label(selected, &layer.name).clicked() {
                                select_layer = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.small(format!("{:?}", layer.layer_type));
                            if ui.small_button("Up").clicked() && index > 0 {
                                pending_move = Some((index, -1));
                            }
                            if ui.small_button("Down").clicked() && index + 1 < total_layers {
                                pending_move = Some((index, 1));
                            }
                        });
                        ui.add(Slider::new(&mut layer.opacity, 0.0..=1.0).text("Opacity"));

                        let total_count = self
                            .workspace
                            .features
                            .iter()
                            .filter(|feature| feature.layer_id == layer.id)
                            .count();
                        let active_count = self
                            .workspace
                            .features
                            .iter()
                            .filter(|feature| {
                                feature.layer_id == layer.id
                                    && feature_is_active(
                                        feature,
                                        self.workspace.app_state.timeline.current_time,
                                    )
                            })
                            .count();
                        ui.small(format!("{} total / {} active", total_count, active_count));
                    });

                    if select_layer {
                        let layer_id = self.workspace.layers[index].id.clone();
                        pending_selection = first_feature_by_layer.get(&layer_id).cloned();
                    }
                    ui.add_space(6.0);
                }

                if let Some((index, direction)) = pending_move {
                    let next = (index as isize + direction) as usize;
                    self.workspace.layers.swap(index, next);
                    for (z, layer) in self.workspace.layers.iter_mut().enumerate() {
                        layer.z_index = (z as i32 + 1) * 10;
                    }
                }

                if let Some(feature_id) = pending_selection {
                    self.interactions.selected_feature_id = Some(feature_id.clone());
                    self.workspace.app_state.ui.selected_feature_id = Some(feature_id);
                }

                ui.separator();
                ui.group(|ui| {
                    let mut credentials_changed = false;
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.traffic.settings.enabled, "");
                        ui.label("Live Traffic");
                    });
                    ui.horizontal(|ui| {
                        ui.small("runtime layer");
                        ui.separator();
                        ui.small(format!("{} aircraft", self.traffic.aircraft_count()));
                        if self.traffic.is_pending() {
                            ui.separator();
                            ui.small("refreshing");
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut self.traffic.settings.filter_mode,
                            TrafficFilterMode::CommercialOnly,
                            "Commercial",
                        );
                        ui.selectable_value(
                            &mut self.traffic.settings.filter_mode,
                            TrafficFilterMode::AllAircraft,
                            "All aircraft",
                        );
                    });
                    ui.checkbox(&mut self.traffic.settings.show_labels, "Traffic labels");
                    ui.add(
                        Slider::new(&mut self.traffic.settings.refresh_interval_secs, 5..=300)
                            .text("Refresh s"),
                    );
                    ui.label("OpenSky client ID");
                    if ui
                        .text_edit_singleline(&mut self.traffic.settings.client_id)
                        .changed()
                    {
                        credentials_changed = true;
                    }
                    ui.label("OpenSky client secret");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.traffic.settings.client_secret)
                                .password(true),
                        )
                        .changed()
                    {
                        credentials_changed = true;
                    }
                    self.workspace.app_state.services.opensky_client_id =
                        self.traffic.settings.client_id.clone();
                    self.workspace.app_state.services.opensky_client_secret =
                        self.traffic.settings.client_secret.clone();
                    if credentials_changed {
                        self.save_service_settings("Saved OpenSky credentials to workspace");
                    }
                    ui.small(&self.traffic.status_message);
                });

                ui.separator();
                ui.group(|ui| {
                    let mut credentials_changed = false;
                    ui.horizontal(|ui| {
                        ui.label("WiGLE API");
                    });
                    ui.horizontal(|ui| {
                        ui.small("workspace layer");
                        ui.separator();
                        ui.small(format!("{} networks", wigle_feature_count(&self.workspace)));
                        if self.wigle.is_pending() {
                            ui.separator();
                            ui.small("querying");
                        }
                    });
                    ui.label("WiGLE API name");
                    if ui
                        .text_edit_singleline(&mut self.wigle.settings.api_name)
                        .changed()
                    {
                        credentials_changed = true;
                    }
                    ui.label("WiGLE API token");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.wigle.settings.api_token)
                                .password(true),
                        )
                        .changed()
                    {
                        credentials_changed = true;
                    }
                    self.workspace.app_state.services.wigle_api_name =
                        self.wigle.settings.api_name.clone();
                    self.workspace.app_state.services.wigle_api_token =
                        self.wigle.settings.api_token.clone();
                    if credentials_changed {
                        self.save_service_settings("Saved WiGLE credentials to workspace");
                    }
                    let can_import =
                        self.last_map_query_bounds.is_some() && !self.wigle.is_pending();
                    if ui
                        .add_enabled(can_import, egui::Button::new("Import visible networks"))
                        .clicked()
                    {
                        self.start_wigle_import();
                    }
                    if ui.button("Clear WiGLE layer").clicked() {
                        self.clear_wigle_import_layer();
                    }
                    ui.small("Imports the current map viewport into a dedicated marker layer.");
                    ui.small(&self.wigle.status_message);
                });

                ui.separator();
                ui.group(|ui| {
                    let mut credentials_changed = false;
                    ui.horizontal(|ui| {
                        ui.label("ITS CCTV HLS");
                    });
                    ui.horizontal(|ui| {
                        ui.small("workspace layer");
                        ui.separator();
                        ui.small(format!(
                            "{} cameras",
                            its_cctv_feature_count(&self.workspace)
                        ));
                        if self.its_cctv.is_pending() {
                            ui.separator();
                            ui.small("querying");
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut self.its_cctv.settings.road_type,
                            ItsRoadType::NationalRoad,
                            "National road",
                        );
                        ui.selectable_value(
                            &mut self.its_cctv.settings.road_type,
                            ItsRoadType::Expressway,
                            "Expressway",
                        );
                    });
                    ui.label("ITS API key");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.its_cctv.settings.api_key)
                                .password(true),
                        )
                        .changed()
                    {
                        credentials_changed = true;
                    }
                    self.workspace.app_state.services.its_api_key =
                        self.its_cctv.settings.api_key.clone();
                    if credentials_changed {
                        self.save_service_settings("Saved ITS API key to workspace");
                    }
                    let can_import =
                        self.last_map_query_bounds.is_some() && !self.its_cctv.is_pending();
                    if ui
                        .add_enabled(can_import, egui::Button::new("Import visible CCTV"))
                        .clicked()
                    {
                        self.start_its_cctv_import();
                    }
                    if ui.button("Clear ITS CCTV layer").clicked() {
                        self.clear_its_cctv_import_layer();
                    }
                    ui.small("Imports current-view CCTV points and HLS URLs from its.go.kr.");
                    ui.small(&self.its_cctv.status_message);
                });

                ui.separator();
                if ui.button("Add marker").clicked() {
                    if let Some(layer) = self
                        .workspace
                        .layers
                        .iter()
                        .find(|layer| layer.layer_type == LayerType::Marker)
                    {
                        let mut feature = crate::domain::Feature::new(
                            layer.id.clone(),
                            crate::domain::FeatureType::Marker,
                            "New marker",
                            crate::domain::Geometry::Point(self.workspace.app_state.camera.center),
                            crate::domain::FeatureStyle::marker(
                                egui::Color32::WHITE,
                                egui::Color32::from_rgb(251, 146, 60),
                                9.0,
                            ),
                        );
                        feature.time_start = Some(Utc::now());
                        let feature_id = feature.id.clone();
                        self.apply_command(WorkspaceCommand::AddFeature { feature });
                        self.interactions.selected_feature_id = Some(feature_id.clone());
                        self.workspace.app_state.ui.selected_feature_id = Some(feature_id);
                    }
                }
            });
    }

    fn render_inspector(&mut self, ctx: &Context) {
        SidePanel::right("inspector")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                let selected_aircraft = self
                    .interactions
                    .selected_aircraft_icao24
                    .as_deref()
                    .and_then(|icao24| self.traffic.aircraft(icao24));
                show_inspector(
                    ui,
                    &mut self.workspace,
                    &self.interactions,
                    selected_aircraft,
                );
            });
    }

    fn render_timeline(&mut self, ctx: &Context) {
        TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(210.0)
            .min_height(160.0)
            .show(ctx, |ui| {
                ui.heading("Timeline");
                ui.separator();

                ui.horizontal(|ui| {
                    if ui
                        .button(if self.workspace.app_state.timeline.playing {
                            "Pause"
                        } else {
                            "Play"
                        })
                        .clicked()
                    {
                        self.workspace.app_state.timeline.playing =
                            !self.workspace.app_state.timeline.playing;
                    }
                    ui.add(
                        Slider::new(
                            &mut self.workspace.app_state.timeline.playback_speed,
                            1.0..=120.0,
                        )
                        .text("Speed x"),
                    );
                ui.checkbox(
                    &mut self.workspace.app_state.timeline.show_only_active,
                    "Filter map to active features",
                );
                ui.add(
                    egui::DragValue::new(&mut self.workspace.app_state.timeline.playback_fps_cap)
                        .range(1..=240)
                        .speed(1.0)
                        .suffix(" fps"),
                );
                let mut fraction = time_to_fraction(
                    self.workspace.app_state.timeline.current_time,
                    self.workspace.app_state.timeline.range_start,
                        self.workspace.app_state.timeline.range_end,
                    );
                    if ui
                        .add(Slider::new(&mut fraction, 0.0..=1.0).text("Scrub"))
                        .changed()
                    {
                        self.workspace.app_state.timeline.current_time = scrub_fraction_to_time(
                            fraction,
                            self.workspace.app_state.timeline.range_start,
                            self.workspace.app_state.timeline.range_end,
                        );
                    }
                    ui.label(
                        self.workspace
                            .app_state
                            .timeline
                            .current_time
                            .format("%Y-%m-%d %H:%M:%S UTC")
                            .to_string(),
                    );
                });

                ui.small("Active events are highlighted. In Geometry mode, drag vertices, Shift+click a segment to insert, Delete to remove.");
                ui.add_space(8.0);
                let available = ui.available_rect_before_wrap();
                let track_height = 22.0;
                let painter = ui.painter_at(available);
                painter.rect_filled(available, 8.0, egui::Color32::from_rgb(12, 19, 28));

                for (row, event) in self.workspace.events.iter().enumerate() {
                    let top = available.top() + 12.0 + row as f32 * (track_height + 8.0);
                    if top + track_height > available.bottom() - 12.0 {
                        break;
                    }
                    let active_now =
                        event_is_active(event, self.workspace.app_state.timeline.current_time);
                    let start = time_to_fraction(
                        event.start_time,
                        self.workspace.app_state.timeline.range_start,
                        self.workspace.app_state.timeline.range_end,
                    );
                    let end = time_to_fraction(
                        event.end_time.unwrap_or(event.start_time),
                        self.workspace.app_state.timeline.range_start,
                        self.workspace.app_state.timeline.range_end,
                    )
                    .max(start + 0.01);
                    let row_rect = egui::Rect::from_min_max(
                        egui::pos2(available.left() + 140.0, top),
                        egui::pos2(available.right() - 16.0, top + track_height),
                    );
                    painter.text(
                        egui::pos2(available.left() + 10.0, top + track_height * 0.5),
                        egui::Align2::LEFT_CENTER,
                        &event.title,
                        egui::TextStyle::Small.resolve(ui.style()),
                        if active_now {
                            egui::Color32::from_rgb(248, 250, 252)
                        } else {
                            egui::Color32::from_gray(145)
                        },
                    );
                    painter.rect_filled(row_rect, 4.0, egui::Color32::from_rgb(19, 30, 42));
                    let active_rect = egui::Rect::from_min_max(
                        egui::pos2(egui::lerp(row_rect.x_range(), start), row_rect.top()),
                        egui::pos2(egui::lerp(row_rect.x_range(), end), row_rect.bottom()),
                    );
                    painter.rect_filled(
                        active_rect,
                        4.0,
                        if active_now {
                            egui::Color32::from_rgb(56, 189, 248)
                        } else {
                            egui::Color32::from_rgb(51, 65, 85)
                        },
                    );
                }

                let cursor_fraction = time_to_fraction(
                    self.workspace.app_state.timeline.current_time,
                    self.workspace.app_state.timeline.range_start,
                    self.workspace.app_state.timeline.range_end,
                );
                let cursor_x = egui::lerp(available.x_range(), cursor_fraction);
                painter.line_segment(
                    [
                        egui::pos2(cursor_x, available.top() + 6.0),
                        egui::pos2(cursor_x, available.bottom() - 6.0),
                    ],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(250, 204, 21)),
                );
            });
    }
}

impl App for VantageApp {
    fn update(&mut self, ctx: &Context, _frame: &mut Frame) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame);
        self.last_frame = now;
        advance_playback(&mut self.workspace, delta);
        let traffic_changed = self.traffic.drain_results();
        let its_cctv_result = self.its_cctv.drain_results();
        let wigle_result = self.wigle.drain_results();
        let mut its_cctv_changed = false;
        if let Some(result) = its_cctv_result {
            match result {
                Ok(query) => {
                    let import = apply_its_cctv(&mut self.workspace, query);
                    self.sync_selection_from_workspace();
                    self.status_message = match self.save_workspace_quiet() {
                        Ok(()) => format!(
                            "Imported {} ITS CCTV cameras into layer {}",
                            import.added_feature_count, import.layer_id
                        ),
                        Err(error) => format!(
                            "Imported {} ITS CCTV cameras into layer {} but failed to save: {error}",
                            import.added_feature_count, import.layer_id
                        ),
                    };
                    its_cctv_changed = true;
                }
                Err(error) => {
                    self.status_message = error;
                }
            }
        }
        let mut wigle_changed = false;
        if let Some(result) = wigle_result {
            match result {
                Ok(query) => {
                    let import = apply_wigle_networks(&mut self.workspace, query);
                    self.sync_selection_from_workspace();
                    self.status_message = match self.save_workspace_quiet() {
                        Ok(()) => format!(
                            "Imported {} WiGLE networks into layer {}",
                            import.added_feature_count, import.layer_id
                        ),
                        Err(error) => format!(
                            "Imported {} WiGLE networks into layer {} but failed to save: {error}",
                            import.added_feature_count, import.layer_id
                        ),
                    };
                    wigle_changed = true;
                }
                Err(error) => {
                    self.status_message = error;
                }
            }
        }
        if self.workspace.app_state.timeline.playing {
            let fps = self.workspace.app_state.timeline.playback_fps_cap.max(1);
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / fps as f64));
        }
        self.handle_shortcuts(ctx);

        self.render_top_bar(ctx);
        self.render_layer_panel(ctx);
        self.render_timeline(ctx);

        let previous_feature_selection = self.workspace.app_state.ui.selected_feature_id.clone();
        let previous_aircraft_selection = self.interactions.selected_aircraft_icao24.clone();

        CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&self.workspace.name);
                ui.separator();
                ui.label(&self.workspace.description);
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Camera");
                ui.add(
                    Slider::new(&mut self.workspace.app_state.camera.zoom, 1.0..=17.0).text("Zoom"),
                );
                ui.add(
                    Slider::new(
                        &mut self.workspace.app_state.camera.tilt_degrees,
                        0.0..=60.0,
                    )
                    .text("Tilt"),
                );
                ui.add(
                    Slider::new(
                        &mut self.workspace.app_state.camera.bearing_degrees,
                        -180.0..=180.0,
                    )
                    .text("Bearing"),
                );
                ui.separator();
                ui.label(match self.interactions.edit_mode {
                    EditMode::Select => "Mode: select / pan",
                    EditMode::EditGeometry => "Mode: geometry edit",
                });
                if let Some(vertex) = &self.interactions.selected_vertex {
                    ui.monospace(format!("Vertex {}", vertex.vertex_index + 1));
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.small("Map data © OpenStreetMap contributors");
                ui.hyperlink_to("Attribution", "https://www.openstreetmap.org/copyright");
                ui.separator();
                ui.hyperlink_to(
                    "Report a map issue",
                    "https://www.openstreetmap.org/fixthemap",
                );
            });
            ui.add_space(4.0);
            let result = self.map_engine.ui(
                ui,
                &mut self.workspace,
                &mut self.interactions,
                self.traffic.overlay(),
            );
            self.last_map_query_bounds = Some(result.query_bounds);
            self.traffic.maybe_refresh(result.query_bounds, false);
            if let Some(feature_id) = result.selected_feature_id {
                self.workspace.app_state.ui.selected_feature_id = Some(feature_id);
                self.sync_selection_from_workspace();
            }
            if let Some(aircraft) = result.selected_aircraft {
                self.interactions.select_aircraft(Some(aircraft));
                self.workspace.app_state.ui.selected_feature_id = None;
            }
            if let Some(command) = result.command {
                self.apply_command(command);
            }
            if let Some(status) = result.status {
                self.status_message = status;
            }
            if result.edited {
                ctx.request_repaint();
            }
            if traffic_changed {
                ctx.request_repaint();
            }
            if its_cctv_changed {
                ctx.request_repaint();
            }
            if wigle_changed {
                ctx.request_repaint();
            }
        });

        self.render_inspector(ctx);
        self.autosave_view_state_if_due(now);

        if traffic_changed {
            if let Some(icao24) = self.interactions.selected_aircraft_icao24.clone() {
                self.interactions.selected_aircraft = self
                    .traffic
                    .aircraft(&icao24)
                    .cloned()
                    .or(self.interactions.selected_aircraft.clone());
            }
        }

        if self.workspace.app_state.ui.selected_feature_id != previous_feature_selection
            || self.interactions.selected_aircraft_icao24 != previous_aircraft_selection
            || its_cctv_changed
            || wigle_changed
        {
            ctx.request_repaint();
        }
    }
}

fn default_workspace_path() -> PathBuf {
    let mut base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    base.push("data");
    base.push("vantage.sqlite");
    base
}

fn default_cache_path() -> PathBuf {
    let mut base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    base.push("cache");
    base.push("tiles");
    base
}

fn load_initial_workspace(
    store: &SqliteWorkspaceStore,
    workspace_path: &PathBuf,
) -> (Workspace, String) {
    if workspace_path.exists() {
        match store.load_from_path(workspace_path) {
            Ok(mut workspace) => {
                workspace.recalculate_timeline_bounds();
                return (
                    workspace,
                    format!("Opened workspace {}", workspace_path.display()),
                );
            }
            Err(error) => {
                return (
                    sample_workspace(),
                    format!(
                        "Open failed for {}: {error}. Loaded sample workspace instead.",
                        workspace_path.display()
                    ),
                );
            }
        }
    }

    (
        sample_workspace(),
        "Workspace loaded. Use Geometry mode to drag vertices, Shift+click to insert, Delete to remove."
            .into(),
    )
}
