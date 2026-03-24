use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};
use thiserror::Error;

use crate::domain::{Feature, Layer, PersistedAppState, Workspace};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Default)]
pub struct SqliteWorkspaceStore;

impl SqliteWorkspaceStore {
    pub fn save_to_path(
        &self,
        path: impl AsRef<Path>,
        workspace: &Workspace,
    ) -> Result<(), StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(path)?;
        self.migrate(&connection)?;

        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM workspace_meta", [])?;
        transaction.execute("DELETE FROM layers", [])?;
        transaction.execute("DELETE FROM features", [])?;
        transaction.execute("DELETE FROM events", [])?;
        transaction.execute("DELETE FROM app_state", [])?;

        transaction.execute(
            "INSERT INTO workspace_meta (id, name, description) VALUES (?1, ?2, ?3)",
            params![workspace.id, workspace.name, workspace.description],
        )?;

        for layer in &workspace.layers {
            transaction.execute(
                "INSERT INTO layers (id, name, layer_type, visible, z_index, opacity, style_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    layer.id,
                    layer.name,
                    serde_json::to_string(&layer.layer_type)?,
                    layer.visible,
                    layer.z_index,
                    layer.opacity,
                    serde_json::to_string(&layer.style_json)?,
                ],
            )?;
        }

        for feature in &workspace.features {
            transaction.execute(
                "INSERT INTO features (
                    id, layer_id, feature_type, name, geometry_json, style_json, metadata_json, time_start, time_end
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    feature.id,
                    feature.layer_id,
                    serde_json::to_string(&feature.feature_type)?,
                    feature.name,
                    serde_json::to_string(&feature.geometry)?,
                    serde_json::to_string(&feature.style)?,
                    serde_json::to_string(&feature.metadata_json)?,
                    feature.time_start.map(|value| value.to_rfc3339()),
                    feature.time_end.map(|value| value.to_rfc3339()),
                ],
            )?;
        }

        for event in &workspace.events {
            transaction.execute(
                "INSERT INTO events (
                    id, feature_id, title, start_time, end_time, event_type, metadata_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    event.feature_id,
                    event.title,
                    event.start_time.to_rfc3339(),
                    event.end_time.map(|value| value.to_rfc3339()),
                    event.event_type,
                    serde_json::to_string(&event.metadata_json)?,
                ],
            )?;
        }

        transaction.execute(
            "INSERT INTO app_state (id, payload_json) VALUES (?1, ?2)",
            params!["workspace", serde_json::to_string(&workspace.app_state)?],
        )?;

        transaction.commit()?;
        Ok(())
    }

    pub fn load_from_path(&self, path: impl AsRef<Path>) -> Result<Workspace, StorageError> {
        let connection = Connection::open(path)?;
        self.migrate(&connection)?;

        let (id, name, description) = connection.query_row(
            "SELECT id, name, description FROM workspace_meta LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;

        let mut layers_statement = connection.prepare(
            "SELECT id, name, layer_type, visible, z_index, opacity, style_json FROM layers ORDER BY z_index ASC",
        )?;
        let layers = layers_statement
            .query_map([], |row| {
                Ok(Layer {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    layer_type: serde_json::from_str(&row.get::<_, String>(2)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    visible: row.get(3)?,
                    z_index: row.get(4)?,
                    opacity: row.get(5)?,
                    style_json: serde_json::from_str(&row.get::<_, String>(6)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut features_statement = connection.prepare(
            "SELECT id, layer_id, feature_type, name, geometry_json, style_json, metadata_json, time_start, time_end FROM features",
        )?;
        let features = features_statement
            .query_map([], |row| {
                let parse_optional_time = |index| -> Result<_, rusqlite::Error> {
                    let value = row.get::<_, Option<String>>(index)?;
                    value
                        .map(|value| {
                            chrono::DateTime::parse_from_rfc3339(&value)
                                .map(|parsed| parsed.with_timezone(&chrono::Utc))
                                .map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        index,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                })
                        })
                        .transpose()
                };

                Ok(Feature {
                    id: row.get(0)?,
                    layer_id: row.get(1)?,
                    feature_type: serde_json::from_str(&row.get::<_, String>(2)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    name: row.get(3)?,
                    geometry: serde_json::from_str(&row.get::<_, String>(4)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    style: serde_json::from_str(&row.get::<_, String>(5)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    metadata_json: serde_json::from_str(&row.get::<_, String>(6)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    time_start: parse_optional_time(7)?,
                    time_end: parse_optional_time(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut events_statement = connection.prepare(
            "SELECT id, feature_id, title, start_time, end_time, event_type, metadata_json FROM events",
        )?;
        let events = events_statement
            .query_map([], |row| {
                let start_raw: String = row.get(3)?;
                let end_raw: Option<String> = row.get(4)?;
                let start_time = chrono::DateTime::parse_from_rfc3339(&start_raw)
                    .map(|parsed| parsed.with_timezone(&chrono::Utc))
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                let end_time = end_raw
                    .map(|value| {
                        chrono::DateTime::parse_from_rfc3339(&value)
                            .map(|parsed| parsed.with_timezone(&chrono::Utc))
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    4,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })
                    })
                    .transpose()?;

                Ok(crate::domain::Event {
                    id: row.get(0)?,
                    feature_id: row.get(1)?,
                    title: row.get(2)?,
                    start_time,
                    end_time,
                    event_type: row.get(5)?,
                    metadata_json: serde_json::from_str(&row.get::<_, String>(6)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let app_state_json = connection.query_row(
            "SELECT payload_json FROM app_state WHERE id = ?1",
            params!["workspace"],
            |row| row.get::<_, String>(0),
        )?;
        let app_state: PersistedAppState = serde_json::from_str(&app_state_json)?;

        Ok(Workspace {
            id,
            name,
            description,
            layers,
            features,
            events,
            app_state,
        })
    }
    fn migrate(&self, connection: &Connection) -> Result<(), StorageError> {
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS workspace_meta (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS layers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                layer_type TEXT NOT NULL,
                visible INTEGER NOT NULL,
                z_index INTEGER NOT NULL,
                opacity REAL NOT NULL,
                style_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS features (
                id TEXT PRIMARY KEY,
                layer_id TEXT NOT NULL,
                feature_type TEXT NOT NULL,
                name TEXT NOT NULL,
                geometry_json TEXT NOT NULL,
                style_json TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                time_start TEXT,
                time_end TEXT
            );
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                feature_id TEXT,
                title TEXT NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT,
                event_type TEXT NOT NULL,
                metadata_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS app_state (
                id TEXT PRIMARY KEY,
                payload_json TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;

    use uuid::Uuid;

    use super::SqliteWorkspaceStore;
    use crate::domain::sample_workspace;

    #[test]
    fn saves_and_loads_opensky_credentials_in_app_state() {
        let mut workspace = sample_workspace();
        workspace.app_state.services.opensky_client_id = "test-client-id".into();
        workspace.app_state.services.opensky_client_secret = "test-client-secret".into();
        workspace.app_state.services.wigle_api_name = "wigle-name".into();
        workspace.app_state.services.wigle_api_token = "wigle-token".into();
        workspace.app_state.services.its_api_key = "its-key".into();
        workspace.app_state.services.openshipdata_api_key = "openship-key".into();
        workspace.app_state.services.celestrak_group = "stations".into();
        workspace.app_state.services.spacetrack_identity = "space-user".into();
        workspace.app_state.services.spacetrack_password = "space-pass".into();

        let db_path = env::temp_dir().join(format!("vantage-{}.sqlite", Uuid::new_v4()));
        let store = SqliteWorkspaceStore;

        store
            .save_to_path(&db_path, &workspace)
            .expect("workspace should save");

        let loaded = store
            .load_from_path(&db_path)
            .expect("workspace should load");

        assert_eq!(
            loaded.app_state.services.opensky_client_id,
            "test-client-id"
        );
        assert_eq!(
            loaded.app_state.services.opensky_client_secret,
            "test-client-secret"
        );
        assert_eq!(loaded.app_state.services.wigle_api_name, "wigle-name");
        assert_eq!(loaded.app_state.services.wigle_api_token, "wigle-token");
        assert_eq!(loaded.app_state.services.its_api_key, "its-key");
        assert_eq!(
            loaded.app_state.services.openshipdata_api_key,
            "openship-key"
        );
        assert_eq!(loaded.app_state.services.celestrak_group, "stations");
        assert_eq!(loaded.app_state.services.spacetrack_identity, "space-user");
        assert_eq!(loaded.app_state.services.spacetrack_password, "space-pass");

        let _ = fs::remove_file(db_path);
    }
}
