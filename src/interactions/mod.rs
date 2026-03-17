use crate::domain::Geometry;
use crate::traffic::AircraftState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    Select,
    EditGeometry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexSelection {
    pub feature_id: String,
    pub vertex_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragTarget {
    Feature(String),
    Vertex(VertexSelection),
}

#[derive(Clone, Debug)]
pub struct PendingGeometryEdit {
    pub feature_id: String,
    pub before: Geometry,
}

pub struct InteractionState {
    pub selected_feature_id: Option<String>,
    pub selected_aircraft_icao24: Option<String>,
    pub selected_aircraft: Option<AircraftState>,
    pub dragging_target: Option<DragTarget>,
    pub selected_vertex: Option<VertexSelection>,
    pub edit_mode: EditMode,
    pub pending_geometry_edit: Option<PendingGeometryEdit>,
}

impl Default for InteractionState {
    fn default() -> Self {
        Self {
            selected_feature_id: None,
            selected_aircraft_icao24: None,
            selected_aircraft: None,
            dragging_target: None,
            selected_vertex: None,
            edit_mode: EditMode::Select,
            pending_geometry_edit: None,
        }
    }
}

impl InteractionState {
    pub fn select(&mut self, feature_id: Option<String>) {
        self.selected_feature_id = feature_id;
        self.selected_aircraft_icao24 = None;
        self.selected_aircraft = None;
        if self.selected_vertex.as_ref().is_some_and(|vertex| {
            Some(vertex.feature_id.as_str()) != self.selected_feature_id.as_deref()
        }) {
            self.selected_vertex = None;
        }
    }

    pub fn select_aircraft(&mut self, aircraft: Option<AircraftState>) {
        self.selected_aircraft_icao24 = aircraft.as_ref().map(|item| item.icao24.clone());
        self.selected_aircraft = aircraft;
        self.selected_feature_id = None;
        self.selected_vertex = None;
    }
}
