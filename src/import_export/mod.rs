use std::collections::HashMap;
use std::hash::Hash;

use crate::domain::{Feature, Workspace};

pub mod gpx;
pub mod its_cctv;
pub mod openshipdata;
pub mod satellites;
pub mod wigle;

pub use gpx::import_gpx_file;
pub use its_cctv::{apply_its_cctv, clear_its_cctv_layer, its_cctv_feature_count};
pub use openshipdata::{apply_openshipdata, clear_openshipdata_layer, openshipdata_feature_count};
pub use satellites::{apply_satellites, clear_satellite_layer, satellite_feature_count};
pub use wigle::{apply_wigle_networks, clear_wigle_layer, wigle_feature_count};

pub(crate) fn merge_imported_features<K, I, F>(
    workspace: &mut Workspace,
    layer_id: &str,
    incoming: I,
    existing_key: F,
) -> Option<String>
where
    K: Eq + Hash,
    I: IntoIterator<Item = (K, Feature)>,
    F: Fn(&Feature) -> Option<K>,
{
    let mut feature_index_by_key = HashMap::new();
    for (index, feature) in workspace.features.iter().enumerate() {
        if feature.layer_id != layer_id {
            continue;
        }
        if let Some(key) = existing_key(feature) {
            feature_index_by_key.entry(key).or_insert(index);
        }
    }

    let mut selected_feature_id = None;
    for (key, mut feature) in incoming {
        if let Some(index) = feature_index_by_key.get(&key).copied() {
            let feature_id = workspace.features[index].id.clone();
            feature.id = feature_id.clone();
            workspace.features[index] = feature;
            if selected_feature_id.is_none() {
                selected_feature_id = Some(feature_id);
            }
            continue;
        }

        let feature_id = feature.id.clone();
        let index = workspace.features.len();
        workspace.features.push(feature);
        feature_index_by_key.insert(key, index);
        if selected_feature_id.is_none() {
            selected_feature_id = Some(feature_id);
        }
    }

    selected_feature_id
}
