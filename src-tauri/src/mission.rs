//! Mission files (`.fauxrrt`): a named risk analysis the analyst can reopen.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::boats::Boat;
use crate::generate::GenerateSpec;
use crate::rocketpy::RocketPySpec;
use crate::simulate::{DebrisCatalog, SimulateOrigin};
use crate::wind::WindSpec;
use crate::parse::{parse_path_with, ParsedTrack};
use crate::schema::{ColumnMapping, DetectedSchema};
use crate::store::Store;

pub const MISSION_VERSION: u32 = 1;
pub const UNTITLED: &str = "Untitled mission";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDocument {
    #[serde(default = "mission_version")]
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub objects: Vec<MissionObject>,
    #[serde(default)]
    pub unassigned: Vec<MissionTrack>,
    #[serde(default)]
    pub ui: MissionUi,
    #[serde(default)]
    pub wind: WindSpec,
    #[serde(default)]
    pub boats: Vec<Boat>,
    #[serde(default)]
    pub catalogs: Vec<DebrisCatalog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionObject {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub generate: Option<GenerateSpec>,
    #[serde(default)]
    pub rocketpy: Option<RocketPySpec>,
    #[serde(default)]
    pub modes: Vec<MissionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionMode {
    pub name: String,
    #[serde(default)]
    pub probability: f64,
    #[serde(default)]
    pub tracks: Vec<MissionTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionTrack {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub mapping: Option<ColumnMapping>,
    #[serde(default)]
    pub times: Option<Vec<f64>>,
    #[serde(default)]
    pub lla: Option<Vec<f32>>,
    #[serde(default)]
    pub generate: Option<GenerateSpec>,
    #[serde(default)]
    pub simulate: Option<SimulateOrigin>,
    #[serde(default)]
    pub rocketpy: Option<RocketPySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionUi {
    #[serde(default)]
    pub impact_threshold_m: f32,
    #[serde(default = "default_imagery")]
    pub imagery: String,
    #[serde(default = "default_true")]
    pub show_impacts: bool,
    #[serde(default = "default_true")]
    pub show_kde: bool,
    #[serde(default)]
    pub kde_object_name: Option<String>,
    #[serde(default = "default_true")]
    pub show_boats: bool,
    #[serde(default)]
    pub show_iip_boundary: bool,
    #[serde(default)]
    pub show_terminate_boundary: bool,
}

impl Default for MissionUi {
    fn default() -> Self {
        Self {
            impact_threshold_m: 0.0,
            imagery: default_imagery(),
            show_impacts: true,
            show_kde: true,
            kde_object_name: None,
            show_boats: true,
            show_iip_boundary: false,
            show_terminate_boundary: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentMission {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionFile {
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub recent: Vec<RecentMission>,
}

fn mission_version() -> u32 {
    MISSION_VERSION
}

fn default_true() -> bool {
    true
}

fn default_weight() -> f64 {
    1.0
}

fn default_imagery() -> String {
    "satellite".into()
}

impl MissionDocument {
    pub fn from_store(store: &Store, name: &str, ui: MissionUi, mission_dir: Option<&Path>) -> Self {
        let model = store.risk_model();
        let objects = model
            .objects
            .iter()
            .map(|obj| MissionObject {
                name: obj.name.clone(),
                source: Some(obj.source.clone()),
                generate: obj.generate.clone(),
                rocketpy: obj.rocketpy.clone(),
                modes: obj
                    .modes
                    .iter()
                    .map(|mode| MissionMode {
                        name: mode.name.clone(),
                        probability: mode.probability,
                        tracks: mode
                            .tracks
                            .iter()
                            .filter_map(|t| store.tracks.get(&t.id).map(|traj| track_record(traj, mission_dir)))
                            .collect(),
                    })
                    .collect(),
            })
            .collect();
        let unassigned = model
            .unassigned
            .iter()
            .filter_map(|t| store.tracks.get(&t.id).map(|traj| track_record(traj, mission_dir)))
            .collect();
        Self {
            version: MISSION_VERSION,
            name: name.to_string(),
            objects,
            unassigned,
            ui,
            wind: store.wind.without_site_profiles(),
            boats: {
                let mut boats: Vec<Boat> = store.boats.values().cloned().collect();
                boats.sort_by_key(|b| b.id);
                boats
            },
            catalogs: store.catalogs_view(),
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create mission folder: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize mission: {e}"))?;
        fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parse mission {}: {e}", path.display()))
    }

    pub fn apply_to_store(&self, store: &mut Store, mission_dir: Option<&Path>) -> Vec<String> {
        store.clear();
        store.set_wind(self.wind.clone());
        if !self.catalogs.is_empty() {
            store.catalogs.clear();
            for catalog in &self.catalogs {
                store.restore_catalog(catalog.clone());
            }
        }
        let mut errors = Vec::new();
        for obj in &self.objects {
            let object_id = store.create_object_empty(obj.name.clone());
            if let Some(source) = &obj.source {
                let _ = store.set_object_source(object_id, source.clone());
            }
            if let Some(spec) = obj.generate.clone() {
                if store.wind.is_off() && !spec.wind.is_off() {
                    store.set_wind(spec.wind.clone());
                }
                let _ = store.set_generate_spec(object_id, spec);
            }
            if let Some(spec) = obj.rocketpy.clone() {
                let _ = store.set_rocketpy_spec(object_id, spec);
            }
            for mode in &obj.modes {
                let mode_id = match store.add_mode(object_id, mode.name.clone(), mode.probability) {
                    Ok(id) => id,
                    Err(err) => {
                        errors.push(err);
                        continue;
                    }
                };
                for track in &mode.tracks {
                    match restore_track(
                        store,
                        track,
                        mission_dir,
                        Some(object_id),
                        Some(mode_id),
                        obj.generate.as_ref(),
                    ) {
                        Ok(()) => {}
                        Err(err) => errors.push(err),
                    }
                }
            }
            store.ensure_object_mode_sum(object_id);
        }
        for track in &self.unassigned {
            if let Err(err) = restore_track(store, track, mission_dir, None, None, None) {
                errors.push(err);
            }
        }
        for boat in &self.boats {
            store.restore_boat(boat.clone());
        }
        errors
    }
}

fn track_record(track: &crate::store::Trajectory, mission_dir: Option<&Path>) -> MissionTrack {
    let inline = track.path.is_none();
    MissionTrack {
        name: track.name.clone(),
        path: track.path.as_ref().map(|p| store_path(p, mission_dir)),
        color: Some(track.color.clone()),
        visible: track.visible,
        weight: track.weight,
        mapping: Some(track.schema.mapping()),
        times: if inline { track.times.clone() } else { None },
        lla: if inline { Some(track.lla.clone()) } else { None },
        generate: track.generate.clone(),
        simulate: track.simulate.clone(),
        rocketpy: track.rocketpy.clone(),
    }
}

fn restore_track(
    store: &mut Store,
    track: &MissionTrack,
    mission_dir: Option<&Path>,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
    object_generate: Option<&GenerateSpec>,
) -> Result<(), String> {
    let inline = track.lla.as_ref().is_some_and(|lla| lla.len() >= 6);
    let (path, parsed) = if inline {
        (
            None,
            ParsedTrack {
                schema: DetectedSchema::generated(),
                times: track.times.clone(),
                lla: track.lla.clone().unwrap_or_default(),
            },
        )
    } else {
        let path = resolve_track_path(track.path.as_deref(), &track.name, mission_dir)?;
        (Some(path.clone()), parse_path_with(&path, track.mapping.as_ref())?)
    };
    let loaded = store.insert_parsed_into(
        vec![(track.name.clone(), path, parsed)],
        object_id,
        failure_mode_id,
    )?;
    if let Some(id) = loaded.first().map(|m| m.id) {
        store.set_track_style(
            id,
            Some(track.name.clone()),
            track.color.clone(),
            Some(track.visible),
            Some(track.weight),
        )?;
        if let Some(spec) = track
            .generate
            .clone()
            .or_else(|| if inline && track.simulate.is_none() { object_generate.cloned() } else { None })
        {
            store.set_track_generate(id, spec);
        }
        if let Some(origin) = track.simulate.clone() {
            store.set_track_simulate(id, origin);
        }
        if let Some(spec) = track.rocketpy.clone() {
            store.set_track_rocketpy(id, spec);
        }
    }
    Ok(())
}

fn store_path(path: &Path, mission_dir: Option<&Path>) -> String {
    if let Some(dir) = mission_dir {
        if let Ok(rel) = path.strip_prefix(dir) {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

fn resolve_track_path(stored: Option<&str>, name: &str, mission_dir: Option<&Path>) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(raw) = stored.filter(|s| !s.is_empty()) {
        let path = PathBuf::from(raw);
        candidates.push(path.clone());
        if let Some(dir) = mission_dir {
            candidates.push(dir.join(&path));
            if let Some(file) = path.file_name() {
                candidates.push(dir.join(file));
            }
        }
    }
    if let Some(dir) = mission_dir {
        candidates.push(dir.join(name));
    }
    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }
    Err(match stored {
        Some(path) => format!("missing trajectory {name} ({path})"),
        None => format!("cannot restore {name}: no file path stored"),
    })
}

pub fn read_session(path: &Path) -> SessionFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn write_session(path: &Path, session: &SessionFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

pub fn push_recent(session: &mut SessionFile, name: &str, path: &Path) {
    let path_str = path.to_string_lossy().into_owned();
    session.recent.retain(|item| item.path != path_str);
    session.recent.insert(
        0,
        RecentMission {
            name: name.to_string(),
            path: path_str.clone(),
        },
    );
    session.recent.truncate(12);
    session.last = Some(path_str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_path;

    #[test]
    fn mission_roundtrip_reloads_sample_into_named_mode() {
        let mut store = Store::new();
        let object_id = store.create_object_empty("Vehicle".into());
        let mode_id = store.add_mode(object_id, "Nominal".into(), 1.0).unwrap();
        let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("aircraft_lla.csv");
        let parsed = parse_path(&sample).expect("sample");
        store
            .insert_parsed_into(
                vec![("aircraft_lla.csv".into(), Some(sample.clone()), parsed)],
                Some(object_id),
                Some(mode_id),
            )
            .unwrap();

        let doc = MissionDocument::from_store(&store, "Flight test", MissionUi::default(), None);
        let json = serde_json::to_string(&doc).unwrap();
        let loaded: MissionDocument = serde_json::from_str(&json).unwrap();
        let mut store2 = Store::new();
        let errors = loaded.apply_to_store(&mut store2, None);
        assert!(errors.is_empty(), "{errors:?}");
        let model = store2.risk_model();
        assert_eq!(model.objects.len(), 1);
        assert_eq!(model.objects[0].name, "Vehicle");
        assert_eq!(model.objects[0].modes[0].name, "Nominal");
        assert_eq!(model.objects[0].modes[0].track_count, 1);
        assert!(model.unassigned.is_empty());
    }

    #[test]
    fn mission_roundtrip_keeps_boats() {
        let mut store = Store::new();
        let drafts = crate::boats::parse_kml_path(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("samples")
                .join("boats")
                .join("gom_fishing.kml"),
        )
        .unwrap();
        store.insert_boats(drafts, Some(PathBuf::from("gom_fishing.kml")));
        let doc = MissionDocument::from_store(&store, "Gulf boats", MissionUi::default(), None);
        assert_eq!(doc.boats.len(), 7);
        let mut store2 = Store::new();
        let errors = doc.apply_to_store(&mut store2, None);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(store2.boats.len(), 7);
        assert!(store2
            .boats
            .values()
            .all(|b| b.speed_kn.is_some() && b.people_on_board.is_some()));
    }

    #[test]
    fn mission_roundtrip_keeps_debris_catalog() {
        let mut store = Store::new();
        store
            .upsert_catalog(crate::simulate::DebrisCatalog {
                id: 0,
                name: "Range FTS".into(),
                pieces: vec![crate::simulate::DebrisPiece {
                    name: "Tank".into(),
                    ballistic_coeff: 500.0,
                    delta_v_mps: 40.0,
                    count: 2,
                }],
            })
            .unwrap();
        let doc = MissionDocument::from_store(&store, "FTS", MissionUi::default(), None);
        assert!(doc.catalogs.iter().any(|c| c.name == "Range FTS"));
        let mut store2 = Store::new();
        let errors = doc.apply_to_store(&mut store2, None);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(store2.catalogs.values().any(|c| c.name == "Range FTS" && c.pieces.len() == 1));
    }

    #[test]
    fn mission_wind_is_global_and_migrates_from_object() {
        let mut store = Store::new();
        store.set_wind(crate::wind::WindSpec::Constant {
            speed_mps: 12.0,
            from_deg: 270.0,
        });
        let doc = MissionDocument::from_store(&store, "Windy", MissionUi::default(), None);
        assert!(matches!(
            doc.wind,
            crate::wind::WindSpec::Constant { speed_mps, .. } if (speed_mps - 12.0).abs() < 1e-9
        ));
        let mut store2 = Store::new();
        doc.apply_to_store(&mut store2, None);
        assert!(matches!(store2.wind, crate::wind::WindSpec::Constant { speed_mps, .. } if (speed_mps - 12.0).abs() < 1e-9));

        let inherited = serde_json::from_str::<MissionDocument>(
            r#"{"name":"Old","objects":[{"name":"A","generate":{"launch_lat":1.0,"launch_lon":2.0,"aim_lat":3.0,"aim_lon":4.0,"ballistic_coeff":1000.0,"wind":{"type":"constant","speed_mps":8.0,"from_deg":90.0}}}],"ui":{}}"#,
        )
        .unwrap();
        let mut store3 = Store::new();
        inherited.apply_to_store(&mut store3, None);
        assert!(matches!(
            store3.wind,
            crate::wind::WindSpec::Constant { speed_mps, from_deg } if (speed_mps - 8.0).abs() < 1e-9 && (from_deg - 90.0).abs() < 1e-9
        ));
    }
}
