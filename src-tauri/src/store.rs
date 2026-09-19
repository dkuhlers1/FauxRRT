use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::boats::{boat_color, score_boat, Boat, BoatDraft, BoatView};
use crate::kde::KdeGrid;
use crate::generate::GenerateSpec;
use crate::rocketpy::RocketPySpec;
use crate::simulate::{DebrisCatalog, SimulateOrigin};
use crate::wind::WindSpec;
use crate::parse::{downsample_lla, ParsedTrack};
use crate::risk::{
    object_is_valid, trajectory_probability, FailureMode, FailureModeView, ObjectView, RiskModelView,
    RiskObject, TrackProb, PROB_TOL,
};
use crate::schema::DetectedSchema;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_OBJECT: AtomicU64 = AtomicU64::new(1);
static NEXT_MODE: AtomicU64 = AtomicU64::new(1);
static NEXT_BOAT: AtomicU64 = AtomicU64::new(1);
static NEXT_CATALOG: AtomicU64 = AtomicU64::new(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub west: f32,
    pub south: f32,
    pub east: f32,
    pub north: f32,
    pub min_alt: f32,
    pub max_alt: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackMeta {
    pub id: u64,
    pub name: String,
    pub path: Option<String>,
    pub point_count: usize,
    pub display_count: usize,
    pub schema: DetectedSchema,
    pub color: String,
    pub visible: bool,
    pub bounds: Bounds,
    pub time_start: Option<f64>,
    pub time_end: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub display_lla: Vec<f32>,
    pub object_id: Option<u64>,
    pub failure_mode_id: Option<u64>,
    pub weight: f64,
    pub probability: f64,
    /// False for FTS catalogue fragments: globe draws impact points, not the coast.
    pub show_path: bool,
    /// Lon of FTS fire (turn-track end or fragment start) for the terminate hull.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakup_lon: Option<f32>,
    /// Lat of FTS fire (turn-track end or fragment start) for the terminate hull.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakup_lat: Option<f32>,
}

pub struct Trajectory {
    pub id: u64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub schema: DetectedSchema,
    pub times: Option<Vec<f64>>,
    pub lla: Vec<f32>,
    pub color: String,
    pub visible: bool,
    pub object_id: Option<u64>,
    pub failure_mode_id: Option<u64>,
    pub weight: f64,
    pub generate: Option<GenerateSpec>,
    pub simulate: Option<SimulateOrigin>,
    pub rocketpy: Option<RocketPySpec>,
}

pub struct Store {
    pub tracks: HashMap<u64, Trajectory>,
    pub objects: HashMap<u64, RiskObject>,
    pub modes: HashMap<u64, FailureMode>,
    pub boats: HashMap<u64, Boat>,
    pub catalogs: HashMap<u64, DebrisCatalog>,
    pub wind: WindSpec,
}

impl Store {
    pub fn new() -> Self {
        Self {
            tracks: HashMap::new(),
            objects: HashMap::new(),
            modes: HashMap::new(),
            boats: HashMap::new(),
            catalogs: default_catalogs(),
            wind: WindSpec::Off,
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.objects.clear();
        self.modes.clear();
        self.boats.clear();
        self.catalogs = default_catalogs();
        self.wind = WindSpec::Off;
    }

    pub fn set_wind(&mut self, wind: WindSpec) {
        self.wind = wind.without_site_profiles();
    }

    pub fn insert_boats(&mut self, drafts: Vec<BoatDraft>, source: Option<PathBuf>) -> Vec<BoatView> {
        let src = source.map(|p| p.to_string_lossy().into_owned());
        let mut out = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let id = NEXT_BOAT.fetch_add(1, Ordering::Relaxed);
            let boat = Boat {
                id,
                name: draft.name,
                lon: draft.lon,
                lat: draft.lat,
                alt_m: draft.alt_m,
                speed_kn: draft.speed_kn,
                heading_deg: draft.heading_deg,
                people_on_board: draft.people_on_board,
                length_m: draft.length_m,
                age_s: draft.age_s,
                source: src.clone(),
                visible: true,
                color: boat_color(id),
            };
            out.push(boat.view());
            self.boats.insert(id, boat);
        }
        out
    }

    pub fn restore_boat(&mut self, boat: Boat) -> BoatView {
        let id = if boat.id == 0 {
            NEXT_BOAT.fetch_add(1, Ordering::Relaxed)
        } else {
            NEXT_BOAT.fetch_max(boat.id + 1, Ordering::Relaxed);
            boat.id
        };
        let mut boat = boat;
        boat.id = id;
        if boat.color.is_empty() {
            boat.color = boat_color(id);
        }
        let view = boat.view();
        self.boats.insert(id, boat);
        view
    }

    pub fn boats_view(&self) -> Vec<BoatView> {
        self.boats_view_scored(None)
    }

    pub fn boats_view_scored(&self, grid: Option<&KdeGrid>) -> Vec<BoatView> {
        let mut boats: Vec<BoatView> = self
            .boats
            .values()
            .map(|boat| {
                let view = boat.view();
                match grid {
                    Some(grid) => score_boat(view, grid),
                    None => view,
                }
            })
            .collect();
        boats.sort_by(|a, b| {
            b.expected_casualties
                .unwrap_or(-1.0)
                .partial_cmp(&a.expected_casualties.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.p_hit
                        .unwrap_or(-1.0)
                        .partial_cmp(&a.p_hit.unwrap_or(-1.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then(a.id.cmp(&b.id))
        });
        boats
    }

    pub fn remove_boat(&mut self, id: u64) -> Result<(), String> {
        self.boats.remove(&id).ok_or_else(|| "unknown boat".to_string())?;
        Ok(())
    }

    pub fn set_boat_visible(&mut self, id: u64, visible: bool) -> Result<(), String> {
        let boat = self.boats.get_mut(&id).ok_or_else(|| "unknown boat".to_string())?;
        boat.visible = visible;
        Ok(())
    }

    pub fn insert_parsed_many(&mut self, items: Vec<(String, Option<PathBuf>, ParsedTrack)>) -> Vec<TrackMeta> {
        self.insert_parsed_into(items, None, None)
            .expect("unassigned insert cannot fail")
    }

    pub fn insert_parsed_into(
        &mut self,
        items: Vec<(String, Option<PathBuf>, ParsedTrack)>,
        object_id: Option<u64>,
        failure_mode_id: Option<u64>,
    ) -> Result<Vec<TrackMeta>, String> {
        if object_id.is_some() || failure_mode_id.is_some() {
            let oid = object_id.ok_or_else(|| "object and mode are both required".to_string())?;
            let mid = failure_mode_id.ok_or_else(|| "object and mode are both required".to_string())?;
            self.validate_assignment(oid, mid)?;
        }
        let budget = display_budget(items.len().max(self.tracks.len() + items.len()));
        let mut ids = Vec::with_capacity(items.len());
        for (name, path, parsed) in items {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let color = color_for(id);
            let track = Trajectory {
                id,
                name,
                path,
                schema: parsed.schema,
                times: parsed.times,
                lla: parsed.lla,
                color,
                visible: true,
                object_id: None,
                failure_mode_id: None,
                weight: 1.0,
                generate: None,
                simulate: None,
                rocketpy: None,
            };
            self.tracks.insert(id, track);
            ids.push(id);
        }
        if let (Some(oid), Some(mid)) = (object_id, failure_mode_id) {
            self.assign_tracks(&ids, oid, mid)?;
        }
        Ok(ids
            .iter()
            .filter_map(|id| self.tracks.get(id).map(|t| self.summarize_track(t, budget)))
            .collect())
    }

    pub fn insert_built_tracks(
        &mut self,
        object_id: u64,
        failure_mode_id: u64,
        built: Vec<crate::simulate::BuiltTrack>,
    ) -> Result<Vec<TrackMeta>, String> {
        if built.is_empty() {
            return Err("simulation produced no trajectories".into());
        }
        self.validate_assignment(object_id, failure_mode_id)?;
        let path_n = self.tracks.values().filter(|t| shows_path(t)).count()
            + built
                .iter()
                .filter(|item| item.origin.delta_v_ecef.is_none())
                .count();
        let budget = display_budget(path_n.max(1));
        let mut ids = Vec::with_capacity(built.len());
        for item in built {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let track = Trajectory {
                id,
                name: item.name,
                path: None,
                schema: item.parsed.schema,
                times: item.parsed.times,
                lla: item.parsed.lla,
                color: color_for(id),
                visible: true,
                object_id: None,
                failure_mode_id: None,
                weight: item.weight,
                generate: None,
                simulate: Some(item.origin),
                rocketpy: None,
            };
            self.tracks.insert(id, track);
            ids.push(id);
        }
        self.assign_tracks(&ids, object_id, failure_mode_id)?;
        Ok(ids
            .iter()
            .filter_map(|id| self.tracks.get(id).map(|t| self.summarize_track(t, budget)))
            .collect())
    }

    pub fn validate_assignment(&self, object_id: u64, failure_mode_id: u64) -> Result<(), String> {
        let mode = self
            .modes
            .get(&failure_mode_id)
            .ok_or_else(|| "unknown mode".to_string())?;
        if mode.object_id != object_id {
            return Err("mode does not belong to that object".into());
        }
        if !self.objects.contains_key(&object_id) {
            return Err("unknown object".into());
        }
        Ok(())
    }

    pub fn positions(&self, id: u64, budget: usize) -> Option<Vec<f32>> {
        self.tracks.get(&id).map(|t| downsample_lla(&t.lla, budget))
    }

    pub fn create_object(&mut self, name: String) -> ObjectView {
        let id = self.create_object_empty(name);
        self.object_view(id).expect("fresh object")
    }

    pub fn create_object_empty(&mut self, name: String) -> u64 {
        let id = NEXT_OBJECT.fetch_add(1, Ordering::Relaxed);
        self.objects.insert(
            id,
            RiskObject {
                id,
                name,
                source: "files".into(),
                generate: None,
                rocketpy: None,
            },
        );
        id
    }

    pub fn set_object_source(&mut self, id: u64, source: String) -> Result<ObjectView, String> {
        let obj = self.objects.get_mut(&id).ok_or_else(|| "unknown object".to_string())?;
        obj.source = match source.as_str() {
            "generated" => "generated".into(),
            "rocketpy" => "rocketpy".into(),
            _ => "files".into(),
        };
        self.object_view(id).ok_or_else(|| "unknown object".into())
    }

    pub fn set_generate_spec(&mut self, id: u64, spec: GenerateSpec) -> Result<(), String> {
        let obj = self.objects.get_mut(&id).ok_or_else(|| "unknown object".to_string())?;
        obj.source = "generated".into();
        obj.generate = Some(spec.without_wind());
        Ok(())
    }

    pub fn set_track_generate(&mut self, id: u64, spec: GenerateSpec) {
        if let Some(track) = self.tracks.get_mut(&id) {
            track.generate = Some(spec.without_wind());
        }
    }

    pub fn set_rocketpy_spec(&mut self, id: u64, spec: RocketPySpec) -> Result<(), String> {
        let obj = self.objects.get_mut(&id).ok_or_else(|| "unknown object".to_string())?;
        obj.source = "rocketpy".into();
        obj.rocketpy = Some(spec);
        Ok(())
    }

    pub fn set_track_rocketpy(&mut self, id: u64, spec: RocketPySpec) {
        if let Some(track) = self.tracks.get_mut(&id) {
            track.rocketpy = Some(spec);
        }
    }

    pub fn generated_regen_jobs(&self) -> Vec<(u64, GenerateSpec)> {
        let mut jobs: Vec<(u64, GenerateSpec)> = self
            .tracks
            .values()
            .filter_map(|track| {
                if track.simulate.is_some() {
                    return None;
                }
                if let Some(spec) = track.generate.clone() {
                    return Some((track.id, spec));
                }
                if track.path.is_some() {
                    return None;
                }
                let spec = track
                    .object_id
                    .and_then(|oid| self.objects.get(&oid))
                    .and_then(|obj| obj.generate.clone())?;
                Some((track.id, spec))
            })
            .collect();
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }

    pub fn simulate_regen_jobs(&self) -> Vec<(u64, SimulateOrigin)> {
        let mut jobs: Vec<(u64, SimulateOrigin)> = self
            .tracks
            .values()
            .filter_map(|track| track.simulate.clone().map(|origin| (track.id, origin)))
            .collect();
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }

    pub fn set_track_simulate(&mut self, id: u64, origin: SimulateOrigin) {
        if let Some(track) = self.tracks.get_mut(&id) {
            track.simulate = Some(origin);
        }
    }

    pub fn catalogs_view(&self) -> Vec<DebrisCatalog> {
        let mut list: Vec<DebrisCatalog> = self.catalogs.values().cloned().collect();
        list.sort_by_key(|c| c.id);
        list
    }

    pub fn catalog(&self, id: u64) -> Result<DebrisCatalog, String> {
        self.catalogs
            .get(&id)
            .cloned()
            .or_else(|| self.catalogs.values().next().cloned())
            .ok_or_else(|| "no debris catalogue — open Edit catalogue and add pieces".to_string())
    }

    pub fn upsert_catalog(&mut self, mut catalog: DebrisCatalog) -> Result<DebrisCatalog, String> {
        catalog.validate()?;
        if catalog.id == 0 {
            catalog.id = NEXT_CATALOG.fetch_add(1, Ordering::Relaxed);
        } else {
            NEXT_CATALOG.fetch_max(catalog.id + 1, Ordering::Relaxed);
        }
        if catalog.name.trim().is_empty() {
            catalog.name = format!("Catalogue {}", catalog.id);
        }
        self.catalogs.insert(catalog.id, catalog.clone());
        Ok(catalog)
    }

    pub fn restore_catalog(&mut self, catalog: DebrisCatalog) {
        NEXT_CATALOG.fetch_max(catalog.id + 1, Ordering::Relaxed);
        self.catalogs.insert(catalog.id, catalog);
    }

    pub fn remove_catalog(&mut self, id: u64) -> Result<(), String> {
        self.catalogs.remove(&id).ok_or_else(|| "unknown debris catalogue".to_string())?;
        if self.catalogs.is_empty() {
            self.catalogs = default_catalogs();
        }
        Ok(())
    }

    pub fn resolve_sim_target(
        &mut self,
        object_id: Option<u64>,
        object_name: Option<&str>,
        mode_name: &str,
        fallback_object: &str,
    ) -> Result<(u64, u64), String> {
        let mode_name = mode_name.trim();
        if mode_name.is_empty() {
            return Err("enter a mode name".into());
        }
        let oid = if let Some(id) = object_id {
            if !self.objects.contains_key(&id) {
                return Err("unknown object".into());
            }
            id
        } else {
            let name = object_name
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback_object);
            match self
                .objects
                .values()
                .find(|o| o.name.eq_ignore_ascii_case(name))
                .map(|o| o.id)
            {
                Some(id) => id,
                None => self.create_object_empty(name.to_string()),
            }
        };
        let mid = self.ensure_named_mode(oid, mode_name)?;
        let p = self.modes.get(&mid).map(|m| m.probability).unwrap_or(0.0);
        if p <= 0.0 {
            if self.mode_ids(oid).len() <= 1 {
                if let Some(mode) = self.modes.get_mut(&mid) {
                    mode.probability = 1.0;
                }
            } else {
                self.rebalance_modes(oid, mid, 0.25);
            }
        }
        Ok((oid, mid))
    }

    pub fn replace_track_geometry(&mut self, id: u64, parsed: ParsedTrack) -> Option<TrackMeta> {
        let track = self.tracks.get_mut(&id)?;
        track.schema = parsed.schema;
        track.times = parsed.times;
        track.lla = parsed.lla;
        let budget = display_budget(self.tracks.len());
        self.tracks.get(&id).map(|t| self.summarize_track(t, budget))
    }

    pub fn remove_object_tracks(&mut self, object_id: u64) -> Vec<u64> {
        let ids: Vec<u64> = self
            .tracks
            .values()
            .filter(|t| t.object_id == Some(object_id))
            .map(|t| t.id)
            .collect();
        for id in &ids {
            self.tracks.remove(id);
        }
        ids
    }

    pub fn next_track_name(&self, object_id: u64, prefix: &str) -> String {
        let prefix = prefix.trim();
        let prefix = if prefix.is_empty() { "Generated" } else { prefix };
        let used: Vec<String> = self
            .tracks
            .values()
            .filter(|t| t.object_id == Some(object_id))
            .map(|t| t.name.to_ascii_lowercase())
            .collect();
        for i in 1..=9999 {
            let name = format!("{prefix}-{i:02}");
            if !used.iter().any(|existing| existing == &name.to_ascii_lowercase()) {
                return name;
            }
        }
        format!("{prefix}-{}", NEXT_ID.load(Ordering::Relaxed))
    }

    pub fn ensure_named_mode(&mut self, object_id: u64, name: &str) -> Result<u64, String> {
        if let Some(id) = self
            .modes
            .values()
            .find(|m| m.object_id == object_id && m.name.eq_ignore_ascii_case(name))
            .map(|m| m.id)
        {
            return Ok(id);
        }
        let first = self.mode_ids(object_id).is_empty();
        self.add_mode(object_id, name.to_string(), if first { 1.0 } else { 0.0 })
    }

    pub fn rename_object(&mut self, id: u64, name: String) -> Result<ObjectView, String> {
        let obj = self.objects.get_mut(&id).ok_or_else(|| "unknown object".to_string())?;
        obj.name = name;
        self.object_view(id).ok_or_else(|| "unknown object".into())
    }

    pub fn remove_object(&mut self, id: u64) -> Result<Vec<u64>, String> {
        self.objects.remove(&id).ok_or_else(|| "unknown object".to_string())?;
        self.modes.retain(|_, m| m.object_id != id);
        Ok(self.remove_object_tracks(id))
    }

    pub fn add_mode(&mut self, object_id: u64, name: String, probability: f64) -> Result<u64, String> {
        if !self.objects.contains_key(&object_id) {
            return Err("unknown object".into());
        }
        let id = NEXT_MODE.fetch_add(1, Ordering::Relaxed);
        self.modes.insert(
            id,
            FailureMode {
                id,
                object_id,
                name,
                probability: probability.max(0.0),
            },
        );
        Ok(id)
    }

    pub fn create_failure_mode(&mut self, object_id: u64, name: String, probability: f64) -> Result<ObjectView, String> {
        let id = self.add_mode(object_id, name, 0.0)?;
        self.rebalance_modes(object_id, id, probability);
        self.object_view(object_id).ok_or_else(|| "unknown object".into())
    }

    pub fn set_track_style(
        &mut self,
        id: u64,
        name: Option<String>,
        color: Option<String>,
        visible: Option<bool>,
        weight: Option<f64>,
    ) -> Result<(), String> {
        let track = self.tracks.get_mut(&id).ok_or_else(|| "unknown track".to_string())?;
        if let Some(name) = name {
            if !name.trim().is_empty() {
                track.name = name;
            }
        }
        if let Some(color) = color {
            if color.starts_with('#') {
                track.color = color;
            }
        }
        if let Some(visible) = visible {
            track.visible = visible;
        }
        if let Some(weight) = weight {
            track.weight = weight.max(0.0);
        }
        Ok(())
    }

    pub fn update_failure_mode(
        &mut self,
        id: u64,
        name: Option<String>,
        probability: Option<f64>,
    ) -> Result<ObjectView, String> {
        let object_id = {
            let mode = self.modes.get_mut(&id).ok_or_else(|| "unknown mode".to_string())?;
            if let Some(name) = name {
                mode.name = name;
            }
            mode.object_id
        };
        if let Some(p) = probability {
            self.rebalance_modes(object_id, id, p);
        }
        self.object_view(object_id).ok_or_else(|| "unknown object".into())
    }

    pub fn remove_failure_mode(&mut self, id: u64) -> Result<u64, String> {
        let mode = self.modes.remove(&id).ok_or_else(|| "unknown mode".to_string())?;
        let ids: Vec<u64> = self
            .tracks
            .values()
            .filter(|t| t.failure_mode_id == Some(id))
            .map(|t| t.id)
            .collect();
        for track_id in ids {
            self.tracks.remove(&track_id);
        }
        if let Some(nominal) = self.nominal_mode_id(mode.object_id) {
            if let Some(n) = self.modes.get_mut(&nominal) {
                n.probability = (n.probability + mode.probability).clamp(0.0, 1.0);
            }
        }
        self.ensure_object_mode_sum(mode.object_id);
        Ok(mode.object_id)
    }

    pub fn mode_ids(&self, object_id: u64) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .modes
            .values()
            .filter(|m| m.object_id == object_id)
            .map(|m| m.id)
            .collect();
        ids.sort();
        ids
    }

    pub fn nominal_mode_id(&self, object_id: u64) -> Option<u64> {
        self.modes
            .values()
            .find(|m| m.object_id == object_id && m.name.eq_ignore_ascii_case("nominal"))
            .map(|m| m.id)
    }

    /// Keep this object's mode probabilities summing to 1.
    /// The edited mode is set to `new_p`; leftover mass goes to the other modes
    /// (scaled in place, or onto Nominal when the others are zero).
    pub fn rebalance_modes(&mut self, object_id: u64, edited_id: u64, new_p: f64) {
        let p = new_p.clamp(0.0, 1.0);
        let ids = self.mode_ids(object_id);
        if ids.is_empty() {
            return;
        }
        if ids.len() == 1 {
            if let Some(mode) = self.modes.get_mut(&ids[0]) {
                mode.probability = 1.0;
            }
            return;
        }
        if let Some(mode) = self.modes.get_mut(&edited_id) {
            mode.probability = p;
        }
        let remaining = (1.0 - p).max(0.0);
        let others: Vec<u64> = ids.into_iter().filter(|id| *id != edited_id).collect();
        let other_sum: f64 = others
            .iter()
            .filter_map(|id| self.modes.get(id))
            .map(|m| m.probability.max(0.0))
            .sum();
        if other_sum > PROB_TOL {
            for id in &others {
                if let Some(mode) = self.modes.get_mut(id) {
                    mode.probability = remaining * (mode.probability.max(0.0) / other_sum);
                }
            }
        } else {
            let residual = self
                .nominal_mode_id(object_id)
                .filter(|id| *id != edited_id)
                .unwrap_or(others[0]);
            for id in &others {
                if let Some(mode) = self.modes.get_mut(id) {
                    mode.probability = if *id == residual { remaining } else { 0.0 };
                }
            }
        }
    }

    pub fn ensure_object_mode_sum(&mut self, object_id: u64) {
        let ids = self.mode_ids(object_id);
        if ids.is_empty() {
            return;
        }
        if ids.len() == 1 {
            if let Some(mode) = self.modes.get_mut(&ids[0]) {
                mode.probability = 1.0;
            }
            return;
        }
        let sum: f64 = ids
            .iter()
            .filter_map(|id| self.modes.get(id))
            .map(|m| m.probability.max(0.0))
            .sum();
        if (sum - 1.0).abs() <= PROB_TOL {
            return;
        }
        if sum > PROB_TOL {
            for id in &ids {
                if let Some(mode) = self.modes.get_mut(id) {
                    mode.probability /= sum;
                }
            }
            return;
        }
        if let Some(nominal) = self.nominal_mode_id(object_id) {
            for id in &ids {
                if let Some(mode) = self.modes.get_mut(id) {
                    mode.probability = if *id == nominal { 1.0 } else { 0.0 };
                }
            }
        } else if let Some(first) = ids.first() {
            for id in &ids {
                if let Some(mode) = self.modes.get_mut(id) {
                    mode.probability = if id == first { 1.0 } else { 0.0 };
                }
            }
        }
    }

    pub fn assign_tracks(&mut self, track_ids: &[u64], object_id: u64, failure_mode_id: u64) -> Result<(), String> {
        self.validate_assignment(object_id, failure_mode_id)?;
        for id in track_ids {
            if let Some(track) = self.tracks.get_mut(id) {
                track.object_id = Some(object_id);
                track.failure_mode_id = Some(failure_mode_id);
            }
        }
        Ok(())
    }

    pub fn unassign_tracks(&mut self, track_ids: &[u64]) {
        for id in track_ids {
            if let Some(track) = self.tracks.get_mut(id) {
                track.object_id = None;
                track.failure_mode_id = None;
            }
        }
    }

    pub fn set_track_weight(&mut self, id: u64, weight: f64) -> Result<(), String> {
        let track = self.tracks.get_mut(&id).ok_or_else(|| "unknown track".to_string())?;
        track.weight = weight.max(0.0);
        Ok(())
    }

    pub fn normalize_object(&mut self, object_id: u64) -> Result<ObjectView, String> {
        if !self.objects.contains_key(&object_id) {
            return Err("unknown object".into());
        }
        self.ensure_object_mode_sum(object_id);
        self.object_view(object_id).ok_or_else(|| "unknown object".into())
    }

    pub fn mode_weight_sum(&self, mode_id: u64) -> f64 {
        self.tracks
            .values()
            .filter(|t| t.failure_mode_id == Some(mode_id))
            .map(|t| t.weight.max(0.0))
            .sum()
    }

    pub fn track_probability(&self, track: &Trajectory) -> f64 {
        let Some(mode_id) = track.failure_mode_id else {
            return 0.0;
        };
        let Some(mode) = self.modes.get(&mode_id) else {
            return 0.0;
        };
        trajectory_probability(mode.probability, track.weight, self.mode_weight_sum(mode_id))
    }

    /// KDE mass for one impact. Catalogue fragments from one FTS event all occur
    /// together, so each carries the parent mode probability instead of an
    /// exclusive 1/N split (which made debris disappear under the nominal).
    pub fn kde_impact_weight(&self, track: &Trajectory) -> f64 {
        if track.weight <= 0.0 {
            return 0.0;
        }
        let simultaneous = track
            .simulate
            .as_ref()
            .map(|s| s.delta_v_ecef.is_some())
            .unwrap_or(false);
        if !simultaneous {
            return self.track_probability(track);
        }
        let Some(mode_id) = track.failure_mode_id else {
            return 0.0;
        };
        let mode_p = self
            .modes
            .get(&mode_id)
            .map(|m| m.probability.max(0.0))
            .unwrap_or(0.0);
        mode_p * track.weight.max(0.0)
    }

    pub fn object_view(&self, id: u64) -> Option<ObjectView> {
        let obj = self.objects.get(&id)?;
        let mut modes: Vec<&FailureMode> = self.modes.values().filter(|m| m.object_id == id).collect();
        modes.sort_by_key(|m| m.id);
        let mut mode_views = Vec::new();
        let mut empty_modes = 0usize;
        let mut failure_mode_sum = 0.0;
        let mut trajectory_prob_sum = 0.0;

        for mode in modes {
            let mut tracks: Vec<&Trajectory> = self
                .tracks
                .values()
                .filter(|t| t.failure_mode_id == Some(mode.id))
                .collect();
            tracks.sort_by_key(|t| t.id);
            let wsum = tracks.iter().map(|t| t.weight.max(0.0)).sum::<f64>();
            if tracks.is_empty() {
                empty_modes += 1;
            }
            let mut view_tracks = Vec::new();
            let mut mode_prob_sum = 0.0;
            for t in tracks {
                let p = trajectory_probability(mode.probability, t.weight, wsum);
                mode_prob_sum += p;
                view_tracks.push(TrackProb {
                    id: t.id,
                    name: t.name.clone(),
                    weight: t.weight,
                    probability: p,
                    object_id: t.object_id,
                    failure_mode_id: t.failure_mode_id,
                });
            }
            failure_mode_sum += mode.probability;
            trajectory_prob_sum += mode_prob_sum;
            mode_views.push(FailureModeView {
                id: mode.id,
                name: mode.name.clone(),
                probability: mode.probability,
                track_count: view_tracks.len(),
                trajectory_prob_sum: mode_prob_sum,
                tracks: view_tracks,
            });
        }

        let mut issues = Vec::new();
        if mode_views.is_empty() {
            issues.push("Add a mode and trajectories — Load files or Generate".into());
        } else {
            if (failure_mode_sum - 1.0).abs() > PROB_TOL {
                issues.push(format!(
                    "mode probabilities sum to {failure_mode_sum:.6}, need 1"
                ));
            }
            if empty_modes > 0 {
                issues.push(format!("{empty_modes} mode(s) have no trajectories"));
            }
            if (trajectory_prob_sum - 1.0).abs() > PROB_TOL && empty_modes == 0 {
                issues.push(format!(
                    "trajectory probabilities sum to {trajectory_prob_sum:.6}, need 1"
                ));
            }
        }

        Some(ObjectView {
            id: obj.id,
            name: obj.name.clone(),
            inclusive: true,
            trajectories_exclusive: true,
            failure_mode_sum,
            trajectory_prob_sum,
            valid: object_is_valid(failure_mode_sum, trajectory_prob_sum, empty_modes),
            issues,
            modes: mode_views,
            source: obj.source.clone(),
            generate: obj.generate.clone(),
            rocketpy: obj.rocketpy.clone(),
        })
    }

    pub fn risk_model(&self) -> RiskModelView {
        let mut objects: Vec<ObjectView> = self.objects.keys().filter_map(|id| self.object_view(*id)).collect();
        objects.sort_by_key(|o| o.id);
        let mut unassigned: Vec<TrackProb> = self
            .tracks
            .values()
            .filter(|t| t.object_id.is_none() || t.failure_mode_id.is_none())
            .map(|t| TrackProb {
                id: t.id,
                name: t.name.clone(),
                weight: t.weight,
                probability: 0.0,
                object_id: t.object_id,
                failure_mode_id: t.failure_mode_id,
            })
            .collect();
        unassigned.sort_by_key(|t| t.id);
        RiskModelView {
            objects,
            unassigned,
            wind: self.wind.without_site_profiles(),
            catalogs: self.catalogs_view(),
        }
    }

    pub fn assigned_tracks_for(&self, object_id: Option<u64>) -> Vec<&Trajectory> {
        let mut list: Vec<&Trajectory> = self
            .tracks
            .values()
            .filter(|t| match object_id {
                Some(id) => t.object_id == Some(id) && t.failure_mode_id.is_some(),
                None => t.object_id.is_some() && t.failure_mode_id.is_some(),
            })
            .collect();
        list.sort_by_key(|t| t.id);
        list
    }

    pub fn summarize_track(&self, track: &Trajectory, budget: usize) -> TrackMeta {
        summarize_with_prob(track, budget, self.track_probability(track))
    }

    pub fn meta_all(&self) -> Vec<TrackMeta> {
        let path_n = self.tracks.values().filter(|t| shows_path(t)).count();
        let budget = display_budget(path_n.max(1));
        let mut metas: Vec<TrackMeta> = self
            .tracks
            .values()
            .map(|t| self.summarize_track(t, budget))
            .collect();
        metas.sort_by_key(|t| t.id);
        metas
    }
}

fn default_catalogs() -> HashMap<u64, DebrisCatalog> {
    let cat = DebrisCatalog::default_fts();
    let mut map = HashMap::new();
    map.insert(cat.id, cat);
    map
}

pub fn display_budget(track_count: usize) -> usize {
    // Keep IPC + GPU vertices bounded as catalogues grow into the thousands.
    let total = 96_000usize;
    (total / track_count.max(1)).clamp(24, 720)
}

fn shows_path(track: &Trajectory) -> bool {
    track
        .simulate
        .as_ref()
        .map(|s| s.delta_v_ecef.is_none())
        .unwrap_or(true)
}

fn summarize_with_prob(track: &Trajectory, budget: usize, probability: f64) -> TrackMeta {
    let show_path = shows_path(track);
    let fts = fts_fire_lla(track);
    let display_lla = if show_path {
        downsample_lla(&track.lla, budget)
    } else {
        Vec::new()
    };
    let bounds = bounds_of(&track.lla).unwrap_or(Bounds {
        west: 0.0,
        south: 0.0,
        east: 0.0,
        north: 0.0,
        min_alt: 0.0,
        max_alt: 0.0,
    });
    let (time_start, time_end) = match &track.times {
        Some(t) if !t.is_empty() => (t.first().copied(), t.last().copied()),
        _ => (None, None),
    };
    TrackMeta {
        id: track.id,
        name: track.name.clone(),
        path: track.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        point_count: track.lla.len() / 3,
        display_count: display_lla.len() / 3,
        schema: track.schema.clone(),
        color: track.color.clone(),
        visible: track.visible,
        bounds,
        time_start,
        time_end,
        display_lla,
        object_id: track.object_id,
        failure_mode_id: track.failure_mode_id,
        weight: track.weight,
        probability,
        show_path,
        breakup_lon: fts.0,
        breakup_lat: fts.1,
    }
}

/// Nav-failure FTS fire: fragments start there; coordinated-turn tracks end there.
fn fts_fire_lla(track: &Trajectory) -> (Option<f32>, Option<f32>) {
    let Some(sim) = track.simulate.as_ref() else {
        return (None, None);
    };
    if sim.turn.is_none() && sim.delta_v_ecef.is_none() {
        return (None, None);
    }
    let n = track.lla.len() / 3;
    if n == 0 {
        return (None, None);
    }
    let i = if sim.delta_v_ecef.is_some() { 0 } else { n - 1 };
    (Some(track.lla[i * 3]), Some(track.lla[i * 3 + 1]))
}

fn bounds_of(lla: &[f32]) -> Option<Bounds> {
    if lla.len() < 3 {
        return None;
    }
    let mut west = f32::MAX;
    let mut east = f32::MIN;
    let mut south = f32::MAX;
    let mut north = f32::MIN;
    let mut min_alt = f32::MAX;
    let mut max_alt = f32::MIN;
    for p in lla.chunks_exact(3) {
        west = west.min(p[0]);
        east = east.max(p[0]);
        south = south.min(p[1]);
        north = north.max(p[1]);
        min_alt = min_alt.min(p[2]);
        max_alt = max_alt.max(p[2]);
    }
    Some(Bounds {
        west,
        south,
        east,
        north,
        min_alt,
        max_alt,
    })
}

pub fn color_for(id: u64) -> String {
    let h = (id as f64 * 0.6180339887498949).fract();
    let (r, g, b) = hsl_to_rgb(h, 0.72, 0.58);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let a = s * l.min(1.0 - l);
    let f = |n: f64| {
        let k = (n + h * 12.0) % 12.0;
        let v = l - a * ((k - 3.0).min(9.0 - k)).max(-1.0).min(1.0);
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (f(0.0), f(8.0), f(4.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_p(store: &Store, object_id: u64, name: &str) -> f64 {
        store
            .modes
            .values()
            .find(|m| m.object_id == object_id && m.name == name)
            .map(|m| m.probability)
            .unwrap()
    }

    fn mode_sum(store: &Store, object_id: u64) -> f64 {
        store
            .modes
            .values()
            .filter(|m| m.object_id == object_id)
            .map(|m| m.probability)
            .sum()
    }

    fn object_with_nominal(store: &mut Store) -> ObjectView {
        let obj = store.create_object("Vehicle".into());
        store.ensure_named_mode(obj.id, "Nominal").unwrap();
        store.object_view(obj.id).unwrap()
    }

    #[test]
    fn new_object_has_no_modes() {
        let mut store = Store::new();
        let obj = store.create_object("Vehicle".into());
        assert!(obj.modes.is_empty());
        assert!(!obj.valid);
        assert!(obj.issues.iter().any(|s| s.contains("Add a mode")));
    }

    #[test]
    fn first_named_mode_takes_full_mass() {
        let mut store = Store::new();
        let obj = store.create_object("Vehicle".into());
        let id = store.ensure_named_mode(obj.id, "Nominal").unwrap();
        assert!((store.modes.get(&id).unwrap().probability - 1.0).abs() < 1e-12);
    }

    #[test]
    fn setting_failure_reduces_nominal() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        store.create_failure_mode(obj.id, "Failure".into(), 0.0).unwrap();
        let fail_id = store
            .modes
            .values()
            .find(|m| m.name == "Failure")
            .map(|m| m.id)
            .unwrap();
        store.update_failure_mode(fail_id, None, Some(0.1)).unwrap();
        assert!((mode_p(&store, obj.id, "Failure") - 0.1).abs() < 1e-12);
        assert!((mode_p(&store, obj.id, "Nominal") - 0.9).abs() < 1e-12);
        assert!((mode_sum(&store, obj.id) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn single_mode_stays_at_one() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let nom = obj.modes[0].id;
        store.update_failure_mode(nom, None, Some(0.4)).unwrap();
        assert!((mode_p(&store, obj.id, "Nominal") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn removing_failure_returns_mass_to_nominal() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let fail = store.create_failure_mode(obj.id, "Failure".into(), 0.25).unwrap();
        let fail_id = fail.modes.iter().find(|m| m.name == "Failure").unwrap().id;
        store.remove_failure_mode(fail_id).unwrap();
        assert!((mode_p(&store, obj.id, "Nominal") - 1.0).abs() < 1e-12);
        assert!((mode_sum(&store, obj.id) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn removing_object_deletes_its_tracks() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let nom = obj.modes[0].id;
        let parsed = crate::parse::ParsedTrack {
            schema: crate::schema::DetectedSchema::generated(),
            times: Some(vec![0.0, 1.0]),
            lla: vec![-106.0, 32.0, 100.0, -105.0, 32.1, 80.0],
        };
        store
            .insert_parsed_into(vec![("t".into(), None, parsed)], Some(obj.id), Some(nom))
            .unwrap();
        assert_eq!(store.tracks.len(), 1);
        store.remove_object(obj.id).unwrap();
        assert!(store.tracks.is_empty());
        assert!(store.risk_model().unassigned.is_empty());
    }

    fn stub_track(lon: f32) -> crate::parse::ParsedTrack {
        crate::parse::ParsedTrack {
            schema: crate::schema::DetectedSchema::generated(),
            times: Some(vec![0.0, 1.0]),
            lla: vec![lon, 32.0, 100.0, lon + 1.0, 32.1, 80.0],
        }
    }

    #[test]
    fn generated_names_increment_without_replacing() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let nom = obj.modes[0].id;
        store
            .insert_parsed_into(vec![("Nominal-01".into(), None, stub_track(-106.0))], Some(obj.id), Some(nom))
            .unwrap();
        assert_eq!(store.next_track_name(obj.id, "Nominal"), "Nominal-02");
        let fail_id = store.ensure_named_mode(obj.id, "Failure").unwrap();
        store
            .insert_parsed_into(
                vec![(store.next_track_name(obj.id, "Failure"), None, stub_track(-105.0))],
                Some(obj.id),
                Some(fail_id),
            )
            .unwrap();
        assert_eq!(store.tracks.len(), 2);
        assert_eq!(
            store.tracks.values().filter(|t| t.failure_mode_id == Some(nom)).count(),
            1
        );
        assert_eq!(
            store.tracks.values().filter(|t| t.failure_mode_id == Some(fail_id)).count(),
            1
        );
    }

    #[test]
    fn wind_regen_updates_generated_not_files() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let nom = obj.modes[0].id;
        let spec = GenerateSpec {
            launch_lat: 32.4,
            launch_lon: -106.4,
            launch_alt_m: 0.0,
            aim_lat: 34.0,
            aim_lon: -99.0,
            aim_alt_m: 0.0,
            ballistic_coeff: 2500.0,
            burnout_alt_m: 80_000.0,
            failure_count: 0,
            wind: WindSpec::Off,
        };
        store.set_generate_spec(obj.id, spec.clone()).unwrap();
        let calm = crate::generate::generate_track(&spec).unwrap();
        let inserted = store
            .insert_parsed_into(vec![("Nominal-01".into(), None, calm)], Some(obj.id), Some(nom))
            .unwrap();
        let gen_id = inserted[0].id;
        store.set_track_generate(gen_id, spec.clone());
        store
            .insert_parsed_into(
                vec![(
                    "file.csv".into(),
                    Some(std::path::PathBuf::from("file.csv")),
                    stub_track(-100.0),
                )],
                Some(obj.id),
                Some(nom),
            )
            .unwrap();
        let jobs = store.generated_regen_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].0, gen_id);
        let before = store.tracks.get(&gen_id).unwrap().lla.clone();
        let mut windy = spec;
        windy.wind = WindSpec::Constant {
            speed_mps: 40.0,
            from_deg: 270.0,
        };
        let parsed = crate::generate::generate_track(&windy).unwrap();
        store.replace_track_geometry(gen_id, parsed);
        let after = &store.tracks.get(&gen_id).unwrap().lla;
        assert_ne!(&before, after);
        assert_eq!(store.tracks.len(), 2);
    }

    #[test]
    fn simulate_jobs_include_spent_stage() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let nom = obj.modes[0].id;
        let parsed = stub_track(-106.0);
        let inserted = store
            .insert_parsed_into(vec![("Nominal-01".into(), None, parsed)], Some(obj.id), Some(nom))
            .unwrap();
        let source_id = inserted[0].id;
        let origin = crate::simulate::SimulateOrigin {
            r_ecef: [0.0, 0.0, 6_400_000.0],
            v_ecef: [100.0, 0.0, 0.0],
            ballistic_coeff: 150.0,
            time_offset: 10.0,
            turn: None,
            source_track_id: Some(source_id),
            source_time_s: Some(10.0),
            delta_v_ecef: None,
            ground_alt_m: 0.0,
        };
        let (oid, mid) = store
            .resolve_sim_target(None, Some("Spent stage"), "Staging", "Spent stage")
            .unwrap();
        let stage = store
            .insert_parsed_into(
                vec![("Stage-01".into(), None, stub_track(-105.0))],
                Some(oid),
                Some(mid),
            )
            .unwrap();
        store.set_track_simulate(stage[0].id, origin);
        let jobs = store.simulate_regen_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].0, stage[0].id);
        assert_eq!(jobs[0].1.source_track_id, Some(source_id));
    }

    #[test]
    fn resolve_sim_target_gives_new_object_probability() {
        let mut store = Store::new();
        let (oid, mid) = store
            .resolve_sim_target(None, Some("Nav + FTS"), "Nav + FTS", "Nav + FTS")
            .unwrap();
        let mode = store.modes.get(&mid).unwrap();
        assert_eq!(mode.object_id, oid);
        assert!((mode.probability - 1.0).abs() < 1e-12);
        assert!((mode_sum(&store, oid) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn resolve_sim_target_rebalances_zero_mode_on_existing_object() {
        let mut store = Store::new();
        let obj = object_with_nominal(&mut store);
        let (oid, mid) = store
            .resolve_sim_target(Some(obj.id), None, "Nav + FTS", "Nav + FTS")
            .unwrap();
        assert_eq!(oid, obj.id);
        let p = store.modes.get(&mid).unwrap().probability;
        assert!(p > 0.0, "new sim mode should carry probability, p={p}");
        assert!((mode_sum(&store, oid) - 1.0).abs() < 1e-9);
    }

    fn dummy_schema() -> DetectedSchema {
        DetectedSchema {
            delimiter: ",".into(),
            has_header: true,
            frame: crate::schema::Frame::Lla,
            confidence: 1.0,
            columns: vec![],
            time_col: None,
            lat_col: None,
            lon_col: None,
            alt_col: None,
            x_col: None,
            y_col: None,
            z_col: None,
        }
    }

    fn dummy_origin(turn: bool, delta_v: bool) -> SimulateOrigin {
        SimulateOrigin {
            r_ecef: [0.0; 3],
            v_ecef: [0.0; 3],
            ballistic_coeff: 50.0,
            time_offset: 0.0,
            turn: turn.then_some(crate::simulate::TurnOrigin {
                duration_s: 5.0,
                max_g: 5.0,
                side: 1.0,
                sustain_speed: true,
            }),
            source_track_id: None,
            source_time_s: Some(10.0),
            delta_v_ecef: delta_v.then_some([1.0, 0.0, 0.0]),
            ground_alt_m: 0.0,
        }
    }

    fn dummy_track(lla: Vec<f32>, origin: Option<SimulateOrigin>) -> Trajectory {
        Trajectory {
            id: 1,
            name: "t".into(),
            path: None,
            schema: dummy_schema(),
            times: None,
            lla,
            color: "#fff".into(),
            visible: true,
            object_id: None,
            failure_mode_id: None,
            weight: 1.0,
            generate: None,
            simulate: origin,
            rocketpy: None,
        }
    }

    #[test]
    fn fts_fire_uses_fragment_start_and_turn_end() {
        let turn = dummy_track(
            vec![-86.0, 30.0, 1000.0, -85.9, 29.9, 800.0],
            Some(dummy_origin(true, false)),
        );
        assert_eq!(fts_fire_lla(&turn), (Some(-85.9), Some(29.9)));

        let frag = dummy_track(
            vec![-85.9, 29.9, 800.0, -85.8, 29.8, 0.0],
            Some(dummy_origin(true, true)),
        );
        assert_eq!(fts_fire_lla(&frag), (Some(-85.9), Some(29.9)));

        let parent = dummy_track(vec![-86.0, 30.0, 1000.0], None);
        assert_eq!(fts_fire_lla(&parent), (None, None));
    }
}
