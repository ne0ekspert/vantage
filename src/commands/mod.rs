use crate::domain::{Feature, Geometry, Workspace};

#[derive(Clone, Debug)]
pub enum WorkspaceCommand {
    AddFeature {
        feature: Feature,
    },
    UpdateGeometry {
        feature_id: String,
        before: Geometry,
        after: Geometry,
    },
}

#[derive(Default)]
pub struct CommandHistory {
    undo_stack: Vec<WorkspaceCommand>,
    redo_stack: Vec<WorkspaceCommand>,
}

impl CommandHistory {
    pub fn apply_and_record(
        &mut self,
        workspace: &mut Workspace,
        command: WorkspaceCommand,
    ) -> Result<String, String> {
        let label = command.label().to_owned();
        command.apply(workspace)?;
        workspace.recalculate_timeline_bounds();
        self.undo_stack.push(command);
        self.redo_stack.clear();
        Ok(label)
    }

    pub fn undo(&mut self, workspace: &mut Workspace) -> Result<Option<String>, String> {
        let Some(command) = self.undo_stack.pop() else {
            return Ok(None);
        };
        let label = command.label().to_owned();
        command.undo(workspace)?;
        workspace.recalculate_timeline_bounds();
        self.redo_stack.push(command);
        Ok(Some(label))
    }

    pub fn redo(&mut self, workspace: &mut Workspace) -> Result<Option<String>, String> {
        let Some(command) = self.redo_stack.pop() else {
            return Ok(None);
        };
        let label = command.label().to_owned();
        command.apply(workspace)?;
        workspace.recalculate_timeline_bounds();
        self.undo_stack.push(command);
        Ok(Some(label))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}

impl WorkspaceCommand {
    pub fn label(&self) -> &'static str {
        match self {
            Self::AddFeature { .. } => "add feature",
            Self::UpdateGeometry { .. } => "edit geometry",
        }
    }

    fn apply(&self, workspace: &mut Workspace) -> Result<(), String> {
        match self {
            Self::AddFeature { feature } => {
                if workspace.features.iter().any(|item| item.id == feature.id) {
                    return Ok(());
                }
                workspace.features.push(feature.clone());
                workspace.app_state.ui.selected_feature_id = Some(feature.id.clone());
                Ok(())
            }
            Self::UpdateGeometry {
                feature_id, after, ..
            } => {
                let feature = workspace
                    .feature_mut(feature_id)
                    .ok_or_else(|| format!("feature not found: {feature_id}"))?;
                feature.geometry = after.clone();
                workspace.app_state.ui.selected_feature_id = Some(feature_id.clone());
                Ok(())
            }
        }
    }

    fn undo(&self, workspace: &mut Workspace) -> Result<(), String> {
        match self {
            Self::AddFeature { feature } => {
                workspace.features.retain(|item| item.id != feature.id);
                if workspace.app_state.ui.selected_feature_id.as_deref()
                    == Some(feature.id.as_str())
                {
                    workspace.app_state.ui.selected_feature_id = None;
                }
                Ok(())
            }
            Self::UpdateGeometry {
                feature_id, before, ..
            } => {
                let feature = workspace
                    .feature_mut(feature_id)
                    .ok_or_else(|| format!("feature not found: {feature_id}"))?;
                feature.geometry = before.clone();
                workspace.app_state.ui.selected_feature_id = Some(feature_id.clone());
                Ok(())
            }
        }
    }
}
