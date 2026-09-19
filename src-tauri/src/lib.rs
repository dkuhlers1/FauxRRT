mod boats;
mod generate;
mod geodesy;
mod impact;
mod kde;
mod mission;
mod parse;
mod pick;
mod risk;
mod rocketpy;
mod schema;
mod simulate;
mod store;
mod wind;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use boats::{parse_kml_path, BoatView};
use generate::{generate_track, resolve_winds, GenerateSpec};
use geodesy::ecef_to_lla;
use simulate::{
    build_fts_debris, build_nav_failure, build_spent_stage, regenerate_simulated, sample_stage_times,
    sample_track_state, BuiltTrack, DebrisCatalog, FtsSpec, NavFailSpec, SpentStageSpec, StateSample,
};
use wind::WindSpec;
use impact::extract_impact;
use kde::{kde_lonlat, KdeGrid, WeightedPoint};
use mission::{
    push_recent, read_session, write_session, MissionDocument, MissionUi, RecentMission, SessionFile,
    UNTITLED,
};
use parse::{parse_path, parse_path_with, parse_text, ParsedTrack};
use pick::{pick_kml_sources, pick_trajectory_folder, pick_trajectory_sources};
use rocketpy::{check_rocketpy, fly_rocketpy, RocketPySpec, RocketPyStatus};
use rayon::prelude::*;
use risk::RiskModelView;
use schema::ColumnMapping;
use serde::Serialize;
use store::{display_budget, Store, TrackMeta};
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    store: Mutex<Store>,
    mission: Mutex<MissionSession>,
}

struct MissionSession {
    path: Option<PathBuf>,
    name: String,
    dirty: bool,
    ui: MissionUi,
}

impl Default for MissionSession {
    fn default() -> Self {
        Self {
            path: None,
            name: UNTITLED.into(),
            dirty: false,
            ui: MissionUi::default(),
        }
    }
}

#[derive(Serialize)]
pub struct MissionInfo {
    pub name: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub recent: Vec<RecentMission>,
}

#[derive(Serialize)]
pub struct MissionSnapshot {
    pub mission: MissionInfo,
    pub tracks: Vec<TrackMeta>,
    pub risk: RiskModelView,
    pub errors: Vec<String>,
    pub ui: MissionUi,
    pub boats: Vec<BoatView>,
}

#[derive(Serialize)]
pub struct LoadResult {
    pub tracks: Vec<TrackMeta>,
    pub errors: Vec<String>,
    pub elapsed_ms: u64,
}

#[derive(Serialize)]
pub struct WindChangeResult {
    pub model: RiskModelView,
    pub tracks: Vec<TrackMeta>,
    pub regenerated: usize,
    pub elapsed_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct ProgressEvent {
    pub done: usize,
    pub total: usize,
    pub file: String,
}

#[derive(Serialize)]
pub struct TrackPositions {
    pub id: u64,
    pub lla: Vec<f32>,
}

#[tauri::command]
fn load_files(
    app: AppHandle,
    state: State<AppState>,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
    mode_name: Option<String>,
) -> Result<LoadResult, String> {
    let files = pick_trajectory_sources()?;
    if files.is_empty() {
        return Ok(empty_result());
    }
    let result = load_paths(&app, &state, files, object_id, failure_mode_id, mode_name)?;
    after_store_change(&app, &state);
    Ok(result)
}

#[tauri::command]
fn load_folder(
    app: AppHandle,
    state: State<AppState>,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
    mode_name: Option<String>,
) -> Result<LoadResult, String> {
    let files = pick_trajectory_folder()?;
    if files.is_empty() {
        return Ok(empty_result());
    }
    let result = load_paths(&app, &state, files, object_id, failure_mode_id, mode_name)?;
    after_store_change(&app, &state);
    Ok(result)
}

#[derive(Serialize)]
pub struct BoatLoadResult {
    pub boats: Vec<BoatView>,
    pub added: usize,
    pub errors: Vec<String>,
    pub elapsed_ms: u64,
}

#[tauri::command]
fn load_boats(app: AppHandle, state: State<AppState>) -> Result<BoatLoadResult, String> {
    let files = pick_kml_sources()?;
    if files.is_empty() {
        return Ok(current_boats(&state, Vec::new(), 0, 0));
    }
    load_boat_paths(&app, &state, files)
}

#[tauri::command]
fn load_sample_boats(app: AppHandle, state: State<AppState>) -> Result<BoatLoadResult, String> {
    let dir = sample_boats_dir().ok_or_else(|| "sample boat KML folder not found".to_string())?;
    let files = collect_kml_files(&dir);
    if files.is_empty() {
        return Err(format!("no .kml files in {}", dir.display()));
    }
    load_boat_paths(&app, &state, files)
}

#[tauri::command]
fn import_boats_kml(
    app: AppHandle,
    state: State<AppState>,
    text: String,
    name: Option<String>,
) -> Result<BoatLoadResult, String> {
    let started = Instant::now();
    let drafts = boats::parse_kml(&text)?;
    let added = drafts.len();
    let boats = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.insert_boats(drafts, name.map(PathBuf::from));
        store.boats_view()
    };
    after_store_change(&app, &state);
    Ok(BoatLoadResult {
        boats,
        added,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn get_boats(state: State<AppState>) -> Result<Vec<BoatView>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    Ok(store.boats_view())
}

#[tauri::command]
fn clear_boats(app: AppHandle, state: State<AppState>) -> Result<Vec<BoatView>, String> {
    let boats = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.boats.clear();
        store.boats_view()
    };
    after_store_change(&app, &state);
    Ok(boats)
}

#[tauri::command]
fn remove_boat(app: AppHandle, state: State<AppState>, id: u64) -> Result<Vec<BoatView>, String> {
    let boats = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.remove_boat(id)?;
        store.boats_view()
    };
    after_store_change(&app, &state);
    Ok(boats)
}

#[tauri::command]
fn set_boat_visible(
    app: AppHandle,
    state: State<AppState>,
    id: u64,
    visible: bool,
) -> Result<Vec<BoatView>, String> {
    let boats = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_boat_visible(id, visible)?;
        store.boats_view()
    };
    after_store_change(&app, &state);
    Ok(boats)
}

fn load_boat_paths(app: &AppHandle, state: &AppState, files: Vec<PathBuf>) -> Result<BoatLoadResult, String> {
    let started = Instant::now();
    let mut errors = Vec::new();
    let mut added = 0;
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        for path in files {
            match parse_kml_path(&path) {
                Ok(drafts) => {
                    added += drafts.len();
                    store.insert_boats(drafts, Some(path));
                }
                Err(err) => errors.push(err),
            }
        }
    }
    after_store_change(app, state);
    Ok(current_boats(state, errors, added, started.elapsed().as_millis() as u64))
}

fn current_boats(
    state: &AppState,
    errors: Vec<String>,
    added: usize,
    elapsed_ms: u64,
) -> BoatLoadResult {
    let boats = state
        .store
        .lock()
        .map(|store| store.boats_view())
        .unwrap_or_default();
    BoatLoadResult {
        boats,
        added,
        errors,
        elapsed_ms,
    }
}

fn sample_boats_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("samples").join("boats"));
        candidates.push(cwd.join("..").join("samples").join("boats"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("samples").join("boats"));
            candidates.push(dir.join("..").join("samples").join("boats"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("boats"),
    );
    candidates.into_iter().find(|p| p.is_dir())
}

fn collect_kml_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("kml"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[tauri::command]
fn load_demo(
    app: AppHandle,
    state: State<AppState>,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
) -> Result<LoadResult, String> {
    let started = Instant::now();
    let demos = demo_tracks();
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.insert_parsed_into(demos, object_id, failure_mode_id)?
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn load_risk_demo(app: AppHandle, state: State<AppState>) -> Result<LoadResult, String> {
    let started = Instant::now();
    let tracks = {
        let demos = risk_demo_tracks();
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.clear();
        store.insert_parsed_many(demos);
        seed_risk_demo(&mut store)?;
        store.meta_all()
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[derive(Serialize)]
pub struct ImpactOut {
    pub track_id: u64,
    pub name: String,
    pub object_id: Option<u64>,
    pub object_name: Option<String>,
    pub failure_mode_id: Option<u64>,
    pub lon: f64,
    pub lat: f64,
    pub alt: f64,
    pub time: Option<f64>,
    pub probability: f64,
    pub color: String,
    pub impacted: bool,
}

#[derive(Serialize)]
pub struct ImpactExtractResult {
    pub impacts: Vec<ImpactOut>,
    pub missing: Vec<ImpactOut>,
    pub elapsed_ms: u64,
}

#[derive(Serialize)]
pub struct KdeResult {
    pub object_id: Option<u64>,
    pub object_name: String,
    pub inclusive: bool,
    pub grid: Option<KdeGrid>,
    pub impacts: Vec<ImpactOut>,
    pub missing: usize,
    pub trajectory_prob_sum: f64,
    pub object_valid: Option<bool>,
    pub bandwidth_east_m: Option<f64>,
    pub bandwidth_north_m: Option<f64>,
    pub mass: f64,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub boats: Vec<BoatView>,
}

#[tauri::command]
fn get_risk_model(state: State<AppState>) -> Result<RiskModelView, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    Ok(store.risk_model())
}

#[tauri::command]
fn create_object(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    source: Option<String>,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let name = if name.trim().is_empty() {
            format!("Object {}", store.objects.len() + 1)
        } else {
            name.trim().to_string()
        };
        let view = store.create_object(name);
        if let Some(source) = source {
            store.set_object_source(view.id, source)?;
        }
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn set_object_source(
    app: AppHandle,
    state: State<AppState>,
    id: u64,
    source: String,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_object_source(id, source)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn sample_state_vector(state: State<AppState>, track_id: u64, time_s: f64) -> Result<StateSample, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    sample_from_store(&store, track_id, time_s)
}

#[tauri::command]
fn upsert_debris_catalog(
    app: AppHandle,
    state: State<AppState>,
    catalog: DebrisCatalog,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.upsert_catalog(catalog)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn remove_debris_catalog(app: AppHandle, state: State<AppState>, id: u64) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.remove_catalog(id)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn simulate_spent_stage(
    app: AppHandle,
    state: State<AppState>,
    spec: SpentStageSpec,
) -> Result<LoadResult, String> {
    let started = Instant::now();
    let (wind, samples, floor_alt) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let track = store
            .tracks
            .get(&spec.source_track_id)
            .ok_or_else(|| "unknown track".to_string())?;
        let t0 = track.times.as_ref().and_then(|t| t.first()).copied().unwrap_or(0.0);
        let t1 = track
            .times
            .as_ref()
            .and_then(|t| t.last())
            .copied()
            .unwrap_or((track.lla.len() / 3).saturating_sub(1) as f64);
        let times = sample_stage_times(&spec, t0, t1)?;
        let mut samples = Vec::with_capacity(times.len());
        for t in times {
            samples.push(sample_from_store(&store, spec.source_track_id, t)?);
        }
        (store.wind.clone(), samples, floor_alt_of(&store, spec.source_track_id))
    };
    let mut built = Vec::new();
    let mut skipped = 0usize;
    for sample in &samples {
        if sample.alt_m < 20.0 {
            skipped += 1;
            continue;
        }
        built.push(build_spent_stage(sample, &spec, &wind, floor_alt)?);
    }
    if built.is_empty() {
        return Err(if skipped > 0 {
            "all sampled separation times are on the ground — pick an in-flight window".into()
        } else {
            "spent stage produced no trajectories".into()
        });
    }
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let (object_id, mode_id) = store.resolve_sim_target(
            spec.object_id,
            spec.object_name.as_deref(),
            &spec.mode_name,
            "Spent stage",
        )?;
        insert_built(&mut store, object_id, mode_id, built)?
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn simulate_fts(app: AppHandle, state: State<AppState>, spec: FtsSpec) -> Result<LoadResult, String> {
    let started = Instant::now();
    let (wind, sample, catalog, floor_alt) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let sample = sample_from_store(&store, spec.source_track_id, spec.time_s)?;
        let catalog = store.catalog(spec.catalog_id)?;
        let floor_alt = floor_alt_of(&store, spec.source_track_id);
        (store.wind.clone(), sample, catalog, floor_alt)
    };
    if sample.alt_m < simulate::MIN_BREAKUP_ALT_M {
        return Err(format!(
            "FTS state is at {:.0} m — pick an in-flight time so debris can reach the ground",
            sample.alt_m
        ));
    }
    let built = build_fts_debris(&sample, &catalog, spec.seed, &wind, "", floor_alt)?;
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let (object_id, mode_id) = store.resolve_sim_target(
            spec.object_id,
            spec.object_name.as_deref(),
            &spec.mode_name,
            "FTS debris",
        )?;
        insert_built(&mut store, object_id, mode_id, built)?
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn simulate_nav_failure(
    app: AppHandle,
    state: State<AppState>,
    spec: NavFailSpec,
) -> Result<LoadResult, String> {
    let started = Instant::now();
    if spec.times_s.is_empty() {
        return Err("add at least one time along the source trajectory".into());
    }
    let (wind, catalog, samples, floor_alt) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let catalog = store.catalog(spec.catalog_id)?;
        let sides = match spec.turn_side {
            simulate::TurnSide::Both => 2,
            _ => 1,
        };
        let total = spec.times_s.len() * sides * catalog.fragment_count();
        if total > 2000 {
            return Err(format!(
                "nav + FTS would spawn {total} fragments (max 2000). Use fewer times or pieces."
            ));
        }
        let mut samples = Vec::new();
        for t in &spec.times_s {
            samples.push(sample_from_store(&store, spec.source_track_id, *t)?);
        }
        let floor_alt = floor_alt_of(&store, spec.source_track_id);
        (store.wind.clone(), catalog, samples, floor_alt)
    };
    if samples.iter().all(|s| s.alt_m < simulate::MIN_BREAKUP_ALT_M) {
        return Err("all selected times are near the ground — pick in-flight times on the source trajectory".into());
    }
    let mut built = Vec::new();
    for (i, sample) in samples.iter().enumerate() {
        if sample.alt_m < simulate::MIN_BREAKUP_ALT_M {
            continue;
        }
        let mut one = spec.clone();
        one.times_s = vec![sample.time_s];
        one.seed = spec.seed.wrapping_add((i as u64).saturating_mul(1009));
        built.extend(
            build_nav_failure(sample, &one, &catalog, &wind, floor_alt)
                .map_err(|e| format!("nav + FTS at t={:.1} s: {e}", sample.time_s))?,
        );
    }
    if built.is_empty() {
        return Err("nav + FTS produced no trajectories".into());
    }
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let (object_id, mode_id) = store.resolve_sim_target(
            spec.object_id,
            spec.object_name.as_deref(),
            &spec.mode_name,
            "Nav + FTS",
        )?;
        insert_built(&mut store, object_id, mode_id, built)?
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn sample_from_store(store: &Store, track_id: u64, time_s: f64) -> Result<StateSample, String> {
    let track = store.tracks.get(&track_id).ok_or_else(|| "unknown track".to_string())?;
    sample_track_state(track_id, &track.lla, track.times.as_deref(), time_s)
}

fn floor_alt_of(store: &Store, track_id: u64) -> f64 {
    store
        .tracks
        .get(&track_id)
        .map(|t| simulate::ground_floor_alt(&t.lla))
        .unwrap_or(0.0)
}

fn insert_built(
    store: &mut Store,
    object_id: u64,
    mode_id: u64,
    built: Vec<BuiltTrack>,
) -> Result<Vec<TrackMeta>, String> {
    store.insert_built_tracks(object_id, mode_id, built)
}

#[tauri::command]
fn generate_trajectories(
    app: AppHandle,
    state: State<AppState>,
    object_id: u64,
    spec: GenerateSpec,
    mode_name: Option<String>,
) -> Result<LoadResult, String> {
    let started = Instant::now();
    let mut spec = spec;
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        spec.wind = store.wind.without_site_profiles();
    }
    let spec = resolve_winds(spec)?;
    if let WindSpec::Historical { source, date, hour_utc, .. } = &spec.wind {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_wind(WindSpec::Historical {
            date: date.clone(),
            hour_utc: *hour_utc,
            profiles: Vec::new(),
            source: source.clone(),
        });
    }
    let parsed = generate_track(&spec)?;
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_generate_spec(object_id, spec.clone())?;
        let name = mode_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "enter a mode name, then generate".to_string())?;
        let mode_id = store.ensure_named_mode(object_id, name)?;
        let track_name = store.next_track_name(object_id, name);
        let inserted = store.insert_parsed_into(vec![(track_name, None, parsed)], Some(object_id), Some(mode_id))?;
        if let Some(id) = inserted.first().map(|t| t.id) {
            store.set_track_generate(id, spec);
        }
        inserted
    };
    after_store_change(&app, &state);
    Ok(LoadResult {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[derive(Serialize)]
struct RocketPyLoad {
    tracks: Vec<TrackMeta>,
    errors: Vec<String>,
    elapsed_ms: u64,
    apogee_m: Option<f64>,
    impact_time_s: Option<f64>,
}

#[tauri::command]
fn rocketpy_status() -> RocketPyStatus {
    check_rocketpy()
}

#[tauri::command]
fn generate_rocketpy(
    app: AppHandle,
    state: State<AppState>,
    object_id: u64,
    spec: RocketPySpec,
    mode_name: Option<String>,
) -> Result<RocketPyLoad, String> {
    let started = Instant::now();
    spec.validate()?;
    let wind = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.wind.without_site_profiles()
    };
    let flown = fly_rocketpy(&spec.clone().with_mission_wind(&wind))?;
    let tracks = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_rocketpy_spec(object_id, spec.clone())?;
        let name = mode_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "enter a mode name, then fly RocketPy".to_string())?;
        let mode_id = store.ensure_named_mode(object_id, name)?;
        let track_name = store.next_track_name(object_id, name);
        let inserted = store.insert_parsed_into(
            vec![(track_name, None, flown.parsed)],
            Some(object_id),
            Some(mode_id),
        )?;
        if let Some(id) = inserted.first().map(|t| t.id) {
            store.set_track_rocketpy(id, spec);
        }
        inserted
    };
    after_store_change(&app, &state);
    Ok(RocketPyLoad {
        tracks,
        errors: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        apogee_m: flown.apogee_m,
        impact_time_s: flown.impact_time_s,
    })
}

#[tauri::command]
fn rename_object(app: AppHandle, state: State<AppState>, id: u64, name: String) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.rename_object(id, name)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn remove_object(app: AppHandle, state: State<AppState>, id: u64) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.remove_object(id)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn create_failure_mode(
    app: AppHandle,
    state: State<AppState>,
    object_id: u64,
    name: String,
    probability: f64,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let name = if name.trim().is_empty() { "Mode".into() } else { name.trim().to_string() };
        store.create_failure_mode(object_id, name, probability)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn update_failure_mode(
    app: AppHandle,
    state: State<AppState>,
    id: u64,
    name: Option<String>,
    probability: Option<f64>,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.update_failure_mode(id, name, probability)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn remove_failure_mode(app: AppHandle, state: State<AppState>, id: u64) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.remove_failure_mode(id)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn assign_tracks(
    app: AppHandle,
    state: State<AppState>,
    track_ids: Vec<u64>,
    object_id: u64,
    failure_mode_id: u64,
) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.assign_tracks(&track_ids, object_id, failure_mode_id)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn unassign_tracks(app: AppHandle, state: State<AppState>, track_ids: Vec<u64>) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.unassign_tracks(&track_ids);
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn set_track_weight(app: AppHandle, state: State<AppState>, id: u64, weight: f64) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_track_weight(id, weight)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn normalize_object(app: AppHandle, state: State<AppState>, object_id: u64) -> Result<RiskModelView, String> {
    let model = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.normalize_object(object_id)?;
        store.risk_model()
    };
    after_store_change(&app, &state);
    Ok(model)
}

#[tauri::command]
fn extract_impacts(state: State<AppState>, threshold_m: Option<f32>) -> Result<ImpactExtractResult, String> {
    let started = Instant::now();
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let threshold = threshold_m.unwrap_or(0.0);
    let (impacts, missing, _) = gather_impacts(&store, None, threshold);
    Ok(ImpactExtractResult {
        impacts,
        missing,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn compute_kde(
    state: State<AppState>,
    object_id: Option<u64>,
    threshold_m: Option<f32>,
) -> Result<KdeResult, String> {
    let started = Instant::now();
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let threshold = threshold_m.unwrap_or(0.0);
    let (impacts, missing, points) = gather_impacts(&store, object_id, threshold);
    let trajectory_prob_sum: f64 = store
        .assigned_tracks_for(object_id)
        .iter()
        .map(|t| store.track_probability(t))
        .sum();
    let grid = kde_lonlat(&points);
    let (object_name, inclusive, object_valid) = match object_id {
        Some(id) => {
            let view = store.object_view(id).ok_or_else(|| "unknown object".to_string())?;
            (view.name, false, Some(view.valid))
        }
        None => ("All objects (inclusive)".into(), true, None),
    };
    let (bandwidth_east_m, bandwidth_north_m, mass) = match &grid {
        Some(g) => (Some(g.bandwidth_east_m), Some(g.bandwidth_north_m), g.mass),
        None => (None, None, 0.0),
    };
    let boats = store.boats_view_scored(grid.as_ref());
    Ok(KdeResult {
        object_id,
        object_name,
        inclusive,
        grid,
        impacts,
        missing: missing.len(),
        trajectory_prob_sum,
        object_valid,
        bandwidth_east_m,
        bandwidth_north_m,
        mass,
        elapsed_ms: started.elapsed().as_millis() as u64,
        boats,
    })
}

#[tauri::command]
fn score_boats(state: State<AppState>) -> Result<Vec<BoatView>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let (_, _, points) = gather_impacts(&store, None, 0.0);
    let grid = kde_lonlat(&points);
    Ok(store.boats_view_scored(grid.as_ref()))
}

fn gather_impacts(
    store: &Store,
    object_id: Option<u64>,
    threshold: f32,
) -> (Vec<ImpactOut>, Vec<ImpactOut>, Vec<WeightedPoint>) {
    let mut impacts = Vec::new();
    let mut missing = Vec::new();
    let mut points = Vec::new();
    for track in store.assigned_tracks_for(object_id) {
        if track.weight <= 0.0 || !track.visible {
            continue;
        }
        let probability = store.track_probability(track);
        let object_name = track
            .object_id
            .and_then(|id| store.objects.get(&id).map(|o| o.name.clone()));
        let base = |impacted: bool, lon: f64, lat: f64, alt: f64, time: Option<f64>| ImpactOut {
            track_id: track.id,
            name: track.name.clone(),
            object_id: track.object_id,
            object_name: object_name.clone(),
            failure_mode_id: track.failure_mode_id,
            lon,
            lat,
            alt,
            time,
            probability,
            color: track.color.clone(),
            impacted,
        };
        match extract_impact(&track.lla, track.times.as_deref(), threshold) {
            Some(hit) => {
                let kde_w = store.kde_impact_weight(track);
                if kde_w > 0.0 {
                    points.push(WeightedPoint {
                        lon: hit.lon,
                        lat: hit.lat,
                        weight: kde_w,
                    });
                }
                impacts.push(base(true, hit.lon, hit.lat, hit.alt, hit.time));
            }
            None => missing.push(base(false, 0.0, 0.0, 0.0, None)),
        }
    }
    (impacts, missing, points)
}

fn seed_risk_demo(store: &mut Store) -> Result<(), String> {
    let vehicle = store.create_object("Reentry vehicle".into());
    let nom_id = store.ensure_named_mode(vehicle.id, "Nominal")?;
    store.update_failure_mode(nom_id, Some("Nominal".into()), Some(0.92))?;
    let vehicle_view = store.create_failure_mode(vehicle.id, "Control fail".into(), 0.08)?;
    let fail_id = vehicle_view
        .modes
        .iter()
        .find(|m| m.name == "Control fail")
        .map(|m| m.id)
        .ok_or_else(|| "control-fail mode missing".to_string())?;

    let stage = store.create_object("First stage".into());
    let stage_mode = store.ensure_named_mode(stage.id, "Staging")?;
    store.update_failure_mode(stage_mode, Some("Staging".into()), Some(1.0))?;

    let rv_nom: Vec<u64> = store
        .tracks
        .values()
        .filter(|t| t.name.starts_with("RV-nom"))
        .map(|t| t.id)
        .collect();
    let rv_fail: Vec<u64> = store
        .tracks
        .values()
        .filter(|t| t.name.starts_with("RV-fail"))
        .map(|t| t.id)
        .collect();
    let s1: Vec<u64> = store
        .tracks
        .values()
        .filter(|t| t.name.starts_with("S1-"))
        .map(|t| t.id)
        .collect();
    store.assign_tracks(&rv_nom, vehicle.id, nom_id)?;
    store.assign_tracks(&rv_fail, vehicle.id, fail_id)?;
    store.assign_tracks(&s1, stage.id, stage_mode)?;
    Ok(())
}

#[tauri::command]
fn track_positions(state: State<AppState>, id: u64, budget: Option<u32>) -> Result<TrackPositions, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let budget = budget.unwrap_or(8000) as usize;
    let lla = store.positions(id, budget).ok_or_else(|| "unknown track".to_string())?;
    Ok(TrackPositions { id, lla })
}

#[tauri::command]
fn clear_tracks(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.clear();
    }
    after_store_change(&app, &state);
    Ok(())
}

#[tauri::command]
fn remove_track(app: AppHandle, state: State<AppState>, id: u64) -> Result<(), String> {
    remove_tracks(app, state, vec![id])
}

#[tauri::command]
fn remove_tracks(app: AppHandle, state: State<AppState>, ids: Vec<u64>) -> Result<(), String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        for id in ids {
            store.tracks.remove(&id);
        }
    }
    after_store_change(&app, &state);
    Ok(())
}

#[tauri::command]
fn set_visible(app: AppHandle, state: State<AppState>, id: u64, visible: bool) -> Result<(), String> {
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        if let Some(track) = store.tracks.get_mut(&id) {
            track.visible = visible;
        } else {
            return Err("unknown track".into());
        }
    }
    after_store_change(&app, &state);
    Ok(())
}

#[tauri::command]
fn remap_track(app: AppHandle, state: State<AppState>, id: u64, mapping: ColumnMapping) -> Result<TrackMeta, String> {
    let meta = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        let track = store.tracks.get(&id).ok_or_else(|| "unknown track".to_string())?;
        let path = track.path.clone().ok_or_else(|| "demo tracks cannot be remapped".to_string())?;
        let name = track.name.clone();
        let color = track.color.clone();
        let visible = track.visible;

        let parsed = parse_path_with(&path, Some(&mapping))?;
        let budget = display_budget(store.tracks.len());
        {
            let track = store.tracks.get_mut(&id).ok_or_else(|| "unknown track".to_string())?;
            track.schema = parsed.schema;
            track.times = parsed.times;
            track.lla = parsed.lla;
            track.color = color;
            track.visible = visible;
            track.name = name;
        }
        let track = store.tracks.get(&id).unwrap();
        store.summarize_track(track, budget)
    };
    after_store_change(&app, &state);
    Ok(meta)
}

fn resolve_load_mode(
    store: &mut Store,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
    mode_name: Option<String>,
) -> Result<Option<u64>, String> {
    if let Some(id) = failure_mode_id {
        return Ok(Some(id));
    }
    let Some(object_id) = object_id else {
        return Ok(None);
    };
    let name = mode_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "enter a mode name, then load files".to_string())?;
    Ok(Some(store.ensure_named_mode(object_id, name)?))
}

fn load_paths(
    app: &AppHandle,
    state: &State<AppState>,
    files: Vec<PathBuf>,
    object_id: Option<u64>,
    failure_mode_id: Option<u64>,
    mode_name: Option<String>,
) -> Result<LoadResult, String> {
    let started = Instant::now();
    let total = files.len();
    let parsed: Vec<(usize, Result<(String, Option<PathBuf>, ParsedTrack), String>)> = files
        .par_iter()
        .enumerate()
        .map(|(i, path)| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let result = parse_path(path).map(|track| (name.clone(), Some(path.clone()), track));
            let _ = app.emit(
                "load-progress",
                ProgressEvent {
                    done: i + 1,
                    total,
                    file: name,
                },
            );
            (i, result)
        })
        .collect();

    let mut items = Vec::new();
    let mut errors = Vec::new();
    for (_, result) in parsed {
        match result {
            Ok(item) => items.push(item),
            Err(err) => errors.push(err),
        }
    }

    if items.is_empty() {
        return Ok(LoadResult {
            tracks: Vec::new(),
            errors,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    let failure_mode_id = resolve_load_mode(&mut store, object_id, failure_mode_id, mode_name)?;
    let tracks = store.insert_parsed_into(items, object_id, failure_mode_id)?;
    Ok(LoadResult {
        tracks,
        errors,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn empty_result() -> LoadResult {
    LoadResult {
        tracks: Vec::new(),
        errors: Vec::new(),
        elapsed_ms: 0,
    }
}

fn risk_demo_tracks() -> Vec<(String, Option<PathBuf>, ParsedTrack)> {
    const LON0: f64 = -106.47;
    const LAT0: f64 = 32.40;
    let mut tracks = Vec::new();
    for i in 0..12 {
        let u = (i as f64 - 5.5) / 5.5;
        tracks.push(synthetic_ballistic(
            &format!("RV-nom-{:02}", i + 1),
            LON0,
            LAT0,
            18.0 + u * 0.35,
            185_000.0 + u * 2500.0,
            78_000.0,
            1200.0,
            160,
        ));
    }
    for i in 0..12 {
        let u = (i as f64 - 5.5) / 5.5;
        tracks.push(synthetic_ballistic(
            &format!("RV-fail-{:02}", i + 1),
            LON0,
            LAT0,
            16.0 + u * 4.5,
            155_000.0 + u * 35_000.0,
            62_000.0,
            1200.0,
            160,
        ));
    }
    for i in 0..16 {
        let u = (i as f64 - 7.5) / 7.5;
        tracks.push(synthetic_ballistic(
            &format!("S1-{:02}", i + 1),
            LON0,
            LAT0,
            17.0 + u * 1.8,
            32_000.0 + u * 8_000.0,
            22_000.0,
            1200.0,
            120,
        ));
    }
    tracks
}

fn synthetic_ballistic(
    name: &str,
    lon0: f64,
    lat0: f64,
    az_deg: f64,
    range_m: f64,
    apogee_m: f64,
    pad_m: f64,
    n: usize,
) -> (String, Option<PathBuf>, ParsedTrack) {
    let mut csv = String::from("time,lat,lon,alt\n");
    let az = az_deg.to_radians();
    let lat_m = 111_132.0;
    let lon_m = 111_132.0 * lat0.to_radians().cos().max(0.2);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let dist = range_m * t;
        let lat = lat0 + (dist * az.cos()) / lat_m;
        let lon = lon0 + (dist * az.sin()) / lon_m;
        let alt = (1.0 - t) * (1.0 - t) * pad_m + 2.0 * (1.0 - t) * t * apogee_m + t * t * (-12.0);
        csv.push_str(&format!("{:.2},{:.6},{:.6},{:.1}\n", t * 240.0, lat, lon, alt));
    }
    let parsed = parse_text(&csv, None).expect("demo ballistic");
    (name.to_string(), None, parsed)
}

fn demo_tracks() -> Vec<(String, Option<PathBuf>, ParsedTrack)> {
    vec![
        synthetic_great_circle("KSFO → KJFK", -122.375, 37.619, -73.779, 40.640, 11_000.0, 240),
        synthetic_great_circle("EGLL → RJTT", -0.461, 51.470, 139.779, 35.552, 12_000.0, 280),
        synthetic_great_circle("OMDB → WSSS", 55.365, 25.253, 103.989, 1.359, 10_500.0, 200),
        synthetic_leo("LEO pass", 51.6, 420_000.0, 180),
        synthetic_ground("Ground survey", -105.27, 40.01, 80),
    ]
}

fn synthetic_great_circle(
    name: &str,
    lon1: f64,
    lat1: f64,
    lon2: f64,
    lat2: f64,
    alt: f64,
    n: usize,
) -> (String, Option<PathBuf>, ParsedTrack) {
    let mut csv = String::from("time,lat,lon,alt\n");
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let (lon, lat) = slerp_ll(lon1, lat1, lon2, lat2, t);
        let climb = (std::f64::consts::PI * t).sin() * (alt * 0.08);
        csv.push_str(&format!("{:.1},{:.6},{:.6},{:.1}\n", t * 3600.0, lat, lon, alt + climb));
    }
    let parsed = parse_text(&csv, None).expect("demo lla");
    (name.to_string(), None, parsed)
}

fn synthetic_leo(name: &str, inc_deg: f64, alt: f64, n: usize) -> (String, Option<PathBuf>, ParsedTrack) {
    let mut csv = String::from("t,x,y,z\n");
    let r = 6378137.0 + alt;
    let inc = inc_deg.to_radians();
    for i in 0..n {
        let nu = (i as f64 / n as f64) * std::f64::consts::TAU;
        let x = r * nu.cos();
        let y = r * nu.sin() * inc.cos();
        let z = r * nu.sin() * inc.sin();
        csv.push_str(&format!("{i},{x:.3},{y:.3},{z:.3}\n"));
    }
    let parsed = parse_text(&csv, None).expect("demo ecef");
    (name.to_string(), None, parsed)
}

fn synthetic_ground(name: &str, lon0: f64, lat0: f64, n: usize) -> (String, Option<PathBuf>, ParsedTrack) {
    let mut csv = String::from("lat lon alt\n");
    for i in 0..n {
        let a = i as f64 / 6.0;
        let lat = lat0 + (a * 0.35).sin() * 0.08;
        let lon = lon0 + (a * 0.21).cos() * 0.12;
        csv.push_str(&format!("{lat:.6} {lon:.6} 1620\n"));
    }
    let parsed = parse_text(&csv, None).expect("demo ground");
    (name.to_string(), None, parsed)
}

fn slerp_ll(lon1: f64, lat1: f64, lon2: f64, lat2: f64, t: f64) -> (f64, f64) {
    let p1 = latlon_to_vec(lon1, lat1);
    let p2 = latlon_to_vec(lon2, lat2);
    let dot = (p1[0] * p2[0] + p1[1] * p2[1] + p1[2] * p2[2]).clamp(-1.0, 1.0);
    let omega = dot.acos();
    if omega.abs() < 1e-8 {
        return (lon1, lat1);
    }
    let s1 = ((1.0 - t) * omega).sin() / omega.sin();
    let s2 = (t * omega).sin() / omega.sin();
    let x = s1 * p1[0] + s2 * p2[0];
    let y = s1 * p1[1] + s2 * p2[1];
    let z = s1 * p1[2] + s2 * p2[2];
    let lon = y.atan2(x).to_degrees();
    let lat = z.atan2((x * x + y * y).sqrt()).to_degrees();
    (lon, lat)
}

fn latlon_to_vec(lon: f64, lat: f64) -> [f64; 3] {
    let lon = lon.to_radians();
    let lat = lat.to_radians();
    [
        lat.cos() * lon.cos(),
        lat.cos() * lon.sin(),
        lat.sin(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_samples() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("samples");
        for name in ["aircraft_lla.csv", "ecef_track.txt", "no_header.csv", "survey.tsv"] {
            let parsed = parse_path(&root.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(parsed.lla.len() >= 6, "{name} too short");
        }
    }

    #[test]
    fn risk_demo_objects_are_complete_and_inclusive() {
        let mut store = Store::new();
        store.insert_parsed_many(risk_demo_tracks());
        seed_risk_demo(&mut store).unwrap();
        let model = store.risk_model();
        assert_eq!(model.objects.len(), 2);
        assert!(model.unassigned.is_empty());
        for obj in &model.objects {
            assert!(obj.inclusive);
            assert!(obj.trajectories_exclusive);
            assert!(obj.valid, "{:?}: {:?}", obj.name, obj.issues);
            assert!((obj.trajectory_prob_sum - 1.0).abs() < 1e-6);
        }
        let expected_impacts: f64 = model.objects.iter().map(|o| o.trajectory_prob_sum).sum();
        assert!((expected_impacts - 2.0).abs() < 1e-6);

        let (impacts, missing, points) = gather_impacts(&store, None, 0.0);
        assert!(missing.is_empty(), "demo tracks should impact, missing {}", missing.len());
        assert_eq!(impacts.len(), 40);
        let mass: f64 = points.iter().map(|p| p.weight).sum();
        assert!((mass - 2.0).abs() < 1e-6, "inclusive mass {mass}");
        let grid = kde_lonlat(&points).expect("kde");
        assert!(grid.max_value > 0.0);
        assert!(grid.bandwidth_east_m > 0.0);
    }

    #[test]
    fn load_into_failure_mode_splits_probability_exclusively() {
        let mut store = Store::new();
        let vehicle = store.create_object("Vehicle".into());
        let nom_id = store.ensure_named_mode(vehicle.id, "Nominal").unwrap();
        store.update_failure_mode(nom_id, None, Some(0.9)).unwrap();
        let fail_view = store.create_failure_mode(vehicle.id, "Control fail".into(), 0.1).unwrap();
        let fail_id = fail_view
            .modes
            .iter()
            .find(|m| m.name == "Control fail")
            .map(|m| m.id)
            .unwrap();

        let items = vec![
            synthetic_ballistic("a", -106.4, 32.4, 18.0, 180_000.0, 70_000.0, 1200.0, 40),
            synthetic_ballistic("b", -106.4, 32.4, 18.2, 182_000.0, 70_000.0, 1200.0, 40),
            synthetic_ballistic("c", -106.4, 32.4, 17.8, 178_000.0, 70_000.0, 1200.0, 40),
        ];
        let loaded = store
            .insert_parsed_into(items, Some(vehicle.id), Some(fail_id))
            .unwrap();
        assert_eq!(loaded.len(), 3);
        for meta in &loaded {
            assert_eq!(meta.object_id, Some(vehicle.id));
            assert_eq!(meta.failure_mode_id, Some(fail_id));
            assert!((meta.probability - 0.1 / 3.0).abs() < 1e-12, "{}", meta.probability);
        }
        let sum: f64 = loaded.iter().map(|t| t.probability).sum();
        assert!((sum - 0.1).abs() < 1e-12);
    }

    #[test]
    fn fts_fragments_contribute_mode_mass_to_kde() {
        let mut store = Store::new();
        let vehicle = store.create_object("Vehicle".into());
        let nom_id = store.ensure_named_mode(vehicle.id, "Nominal").unwrap();
        store.update_failure_mode(nom_id, None, Some(0.9)).unwrap();
        store
            .insert_parsed_into(
                vec![synthetic_ballistic("nom", -106.4, 32.4, 18.0, 80_000.0, 40_000.0, 1_400.0, 40)],
                Some(vehicle.id),
                Some(nom_id),
            )
            .unwrap();
        let fail_view = store.create_failure_mode(vehicle.id, "Nav + FTS".into(), 0.1).unwrap();
        let fail_id = fail_view
            .modes
            .iter()
            .find(|m| m.name == "Nav + FTS")
            .map(|m| m.id)
            .unwrap();
        let loaded = store
            .insert_parsed_into(
                vec![
                    synthetic_ballistic("frag-a", -106.35, 32.45, 10.0, 60_000.0, 30_000.0, 1_400.0, 24),
                    synthetic_ballistic("frag-b", -106.50, 32.38, 30.0, 55_000.0, 28_000.0, 1_400.0, 24),
                    synthetic_ballistic("frag-c", -106.28, 32.52, 50.0, 70_000.0, 32_000.0, 1_400.0, 24),
                ],
                Some(vehicle.id),
                Some(fail_id),
            )
            .unwrap();
        let origin = simulate::SimulateOrigin {
            r_ecef: [0.0; 3],
            v_ecef: [0.0; 3],
            ballistic_coeff: 80.0,
            time_offset: 0.0,
            turn: None,
            source_track_id: None,
            source_time_s: None,
            delta_v_ecef: Some([40.0, 0.0, 0.0]),
            ground_alt_m: 0.0,
        };
        for meta in &loaded {
            store.set_track_simulate(meta.id, origin.clone());
        }
        let exclusive: f64 = loaded
            .iter()
            .map(|t| store.track_probability(store.tracks.get(&t.id).unwrap()))
            .sum();
        assert!((exclusive - 0.1).abs() < 1e-9, "exclusive UI mass {exclusive}");
        let kde_mass: f64 = loaded
            .iter()
            .map(|t| store.kde_impact_weight(store.tracks.get(&t.id).unwrap()))
            .sum();
        assert!(
            (kde_mass - 0.3).abs() < 1e-9,
            "each simultaneous fragment keeps the 0.1 mode mass, kde_mass={kde_mass}"
        );
        let (_, _, points) = gather_impacts(&store, None, 0.0);
        let mass: f64 = points.iter().map(|p| p.weight).sum();
        assert!(mass > 1.1, "nominal 0.9 + debris 0.3 should appear on KDE, mass={mass}");
        for meta in &loaded {
            let track = store.tracks.get(&meta.id).unwrap();
            let summary = store.summarize_track(track, 120);
            assert!(!summary.show_path, "FTS fragments must not upload coast polylines");
            assert!(summary.display_lla.is_empty());
        }
    }
}

#[tauri::command]
fn get_mission(app: AppHandle, state: State<AppState>) -> Result<MissionInfo, String> {
    Ok(mission_info(&app, &state)?)
}

#[tauri::command]
fn new_mission(app: AppHandle, state: State<AppState>) -> Result<MissionSnapshot, String> {
    autosave_if_named(&app, &state)?;
    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.clear();
    }
    {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        *mission = MissionSession::default();
    }
    update_title(&app, &state);
    snapshot(&app, &state, Vec::new())
}

#[tauri::command]
fn open_mission(app: AppHandle, state: State<AppState>) -> Result<MissionSnapshot, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("FauxRRT mission", &["fauxrrt", "json"])
        .add_filter("All files", &["*"])
        .pick_file()
    else {
        return snapshot(&app, &state, Vec::new());
    };
    autosave_if_named(&app, &state)?;
    load_mission_from_path(&app, &state, path)
}

#[tauri::command]
fn open_mission_path(app: AppHandle, state: State<AppState>, path: String) -> Result<MissionSnapshot, String> {
    autosave_if_named(&app, &state)?;
    load_mission_from_path(&app, &state, PathBuf::from(path))
}

#[tauri::command]
fn open_last_mission(app: AppHandle, state: State<AppState>) -> Result<MissionSnapshot, String> {
    if let Some(last) = load_session(&app).last {
        if Path::new(&last).is_file() {
            return load_mission_from_path(&app, &state, PathBuf::from(last));
        }
    }
    snapshot(&app, &state, Vec::new())
}

#[tauri::command]
fn save_mission(app: AppHandle, state: State<AppState>) -> Result<MissionSnapshot, String> {
    write_current_mission(&app, &state, false)
}

#[tauri::command]
fn save_mission_as(app: AppHandle, state: State<AppState>) -> Result<MissionSnapshot, String> {
    write_current_mission(&app, &state, true)
}

#[tauri::command]
fn rename_mission(app: AppHandle, state: State<AppState>, name: String) -> Result<MissionInfo, String> {
    {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        let name = if name.trim().is_empty() {
            UNTITLED.to_string()
        } else {
            name.trim().to_string()
        };
        if mission.name != name {
            mission.name = name;
            mission.dirty = true;
        }
    }
    autosave_if_named(&app, &state)?;
    update_title(&app, &state);
    mission_info(&app, &state)
}

#[tauri::command]
fn set_mission_wind(app: AppHandle, state: State<AppState>, wind: WindSpec) -> Result<WindChangeResult, String> {
    let started = Instant::now();
    let (jobs, sim_jobs) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        (store.generated_regen_jobs(), store.simulate_regen_jobs())
    };
    let (gen_parsed, settings_wind, compute_wind) = if jobs.is_empty() {
        let compute = resolve_wind_for_simulate(&wind, &sim_jobs)?;
        (Vec::new(), compute.without_site_profiles(), compute)
    } else {
        regenerate_generated_with_wind(&jobs, &wind)?
    };
    let (model, tracks, regenerated) = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store.set_wind(settings_wind);
        let mut tracks = Vec::with_capacity(gen_parsed.len() + sim_jobs.len());
        for (id, geometry) in gen_parsed {
            if let Some(meta) = store.replace_track_geometry(id, geometry) {
                tracks.push(meta);
            }
        }
        let sim_jobs = store.simulate_regen_jobs();
        let sim_parsed = regenerate_simulated_with_wind(&store, &sim_jobs, &compute_wind)?;
        let regenerated = tracks.len() + sim_parsed.len();
        for (id, geometry) in sim_parsed {
            if let Some(meta) = store.replace_track_geometry(id, geometry) {
                tracks.push(meta);
            }
        }
        (store.risk_model(), tracks, regenerated)
    };
    after_store_change(&app, &state);
    Ok(WindChangeResult {
        model,
        tracks,
        regenerated,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn regenerate_generated_with_wind(
    jobs: &[(u64, GenerateSpec)],
    wind: &WindSpec,
) -> Result<(Vec<(u64, ParsedTrack)>, WindSpec, WindSpec), String> {
    let mut resolved_wind = wind.without_site_profiles();
    let mut cache: HashMap<String, WindSpec> = HashMap::new();
    let mut prepared = Vec::with_capacity(jobs.len());
    for (id, spec) in jobs {
        let mut spec = spec.clone();
        spec.wind = wind.without_site_profiles();
        let key = wind_resolve_key(&spec);
        if let Some(cached) = cache.get(&key) {
            spec.wind = cached.clone();
        } else {
            spec = resolve_winds(spec)?;
            cache.insert(key, spec.wind.clone());
        }
        if let WindSpec::Historical {
            source,
            date,
            hour_utc,
            ..
        } = &spec.wind
        {
            resolved_wind = WindSpec::Historical {
                date: date.clone(),
                hour_utc: *hour_utc,
                profiles: Vec::new(),
                source: source.clone(),
            };
        }
        prepared.push((*id, spec));
    }
    let parsed = prepared
        .par_iter()
        .map(|(id, spec)| generate_track(spec).map(|track| (*id, track)))
        .collect::<Result<Vec<_>, _>>()?;
    let compute_wind = prepared
        .first()
        .map(|(_, spec)| spec.wind.clone())
        .unwrap_or_else(|| wind.clone());
    Ok((parsed, resolved_wind, compute_wind))
}

fn regenerate_simulated_with_wind(
    store: &Store,
    jobs: &[(u64, simulate::SimulateOrigin)],
    wind: &WindSpec,
) -> Result<Vec<(u64, ParsedTrack)>, String> {
    let prepared: Vec<(u64, simulate::SimulateOrigin, Option<StateSample>)> = jobs
        .iter()
        .map(|(id, origin)| {
            let sample = origin.source_track_id.and_then(|sid| {
                let track = store.tracks.get(&sid)?;
                let time = origin.source_time_s.unwrap_or(origin.time_offset);
                sample_track_state(sid, &track.lla, track.times.as_deref(), time).ok()
            });
            (*id, origin.clone(), sample)
        })
        .collect();
    prepared
        .par_iter()
        .map(|(id, origin, sample)| {
            regenerate_simulated(origin, wind, sample.as_ref()).map(|track| (*id, track))
        })
        .collect()
}

fn resolve_wind_for_simulate(
    wind: &WindSpec,
    jobs: &[(u64, simulate::SimulateOrigin)],
) -> Result<WindSpec, String> {
    if jobs.is_empty() || !matches!(wind, WindSpec::Historical { .. }) {
        return Ok(wind.without_site_profiles());
    }
    let (lat, lon) = if let Some((_, origin)) = jobs.first() {
        let (lon, lat, _) = ecef_to_lla(origin.r_ecef[0], origin.r_ecef[1], origin.r_ecef[2]);
        (lat, lon)
    } else {
        return Ok(wind.without_site_profiles());
    };
    let spec = GenerateSpec {
        launch_lat: lat,
        launch_lon: lon,
        launch_alt_m: 0.0,
        aim_lat: lat,
        aim_lon: (lon + 1.0).clamp(-180.0, 180.0),
        aim_alt_m: 0.0,
        ballistic_coeff: 1000.0,
        burnout_alt_m: 80_000.0,
        failure_count: 0,
        wind: wind.clone(),
    };
    Ok(resolve_winds(spec)?.wind)
}

fn wind_resolve_key(spec: &GenerateSpec) -> String {
    match &spec.wind {
        WindSpec::Off => "off".into(),
        WindSpec::Constant { speed_mps, from_deg } => format!("c:{speed_mps}:{from_deg}"),
        WindSpec::Historical { date, hour_utc, .. } => format!(
            "h:{date}:{hour_utc}:{:.5}:{:.5}:{:.5}:{:.5}",
            spec.launch_lat, spec.launch_lon, spec.aim_lat, spec.aim_lon
        ),
    }
}

#[tauri::command]
fn set_mission_ui(app: AppHandle, state: State<AppState>, ui: MissionUi) -> Result<MissionInfo, String> {
    {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        mission.ui = ui;
        if mission.path.is_some() {
            mission.dirty = true;
        }
    }
    autosave_if_named(&app, &state)?;
    mission_info(&app, &state)
}

fn after_store_change(app: &AppHandle, state: &AppState) {
    if let Ok(mut mission) = state.mission.lock() {
        mission.dirty = true;
    }
    let _ = autosave_if_named(app, state);
    update_title(app, state);
}

fn autosave_if_named(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let path = state
        .mission
        .lock()
        .map_err(|e| e.to_string())?
        .path
        .clone();
    if path.is_some() {
        write_current_mission(app, state, false)?;
    }
    Ok(())
}

fn write_current_mission(app: &AppHandle, state: &AppState, force_picker: bool) -> Result<MissionSnapshot, String> {
    let (mut name, existing) = {
        let mission = state.mission.lock().map_err(|e| e.to_string())?;
        (mission.name.clone(), mission.path.clone())
    };
    let path = if force_picker || existing.is_none() {
        let suggested = sanitize_filename(&name);
        match rfd::FileDialog::new()
            .add_filter("FauxRRT mission", &["fauxrrt"])
            .set_file_name(&format!("{suggested}.fauxrrt"))
            .save_file()
        {
            Some(mut path) => {
                if path.extension().is_none() {
                    path.set_extension("fauxrrt");
                }
                if name == UNTITLED {
                    if let Some(stem) = path.file_stem() {
                        name = stem.to_string_lossy().into_owned();
                    }
                }
                path
            }
            None => return snapshot(app, state, Vec::new()),
        }
    } else {
        existing.unwrap()
    };

    let ui = {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        mission.name = name.clone();
        mission.path = Some(path.clone());
        mission.ui.clone()
    };
    let doc = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        MissionDocument::from_store(&store, &name, ui, path.parent())
    };
    doc.write(&path)?;
    {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        mission.dirty = false;
        mission.path = Some(path.clone());
        mission.name = name.clone();
    }
    remember_recent(app, &name, &path)?;
    update_title(app, state);
    snapshot(app, state, Vec::new())
}

fn load_mission_from_path(app: &AppHandle, state: &AppState, path: PathBuf) -> Result<MissionSnapshot, String> {
    let doc = MissionDocument::read(&path)?;
    let errors = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        doc.apply_to_store(&mut store, path.parent())
    };
    {
        let mut mission = state.mission.lock().map_err(|e| e.to_string())?;
        mission.path = Some(path.clone());
        mission.name = if doc.name.trim().is_empty() {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| UNTITLED.to_string())
        } else {
            doc.name.clone()
        };
        mission.ui = doc.ui.clone();
        mission.dirty = false;
    }
    remember_recent(app, &doc.name, &path)?;
    update_title(app, state);
    snapshot(app, state, errors)
}

fn snapshot(app: &AppHandle, state: &AppState, errors: Vec<String>) -> Result<MissionSnapshot, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let mission = state.mission.lock().map_err(|e| e.to_string())?;
    Ok(MissionSnapshot {
        mission: MissionInfo {
            name: mission.name.clone(),
            path: mission.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            dirty: mission.dirty,
            recent: load_session(app).recent,
        },
        tracks: store.meta_all(),
        risk: store.risk_model(),
        errors,
        ui: mission.ui.clone(),
        boats: store.boats_view(),
    })
}

fn mission_info(app: &AppHandle, state: &AppState) -> Result<MissionInfo, String> {
    let mission = state.mission.lock().map_err(|e| e.to_string())?;
    Ok(MissionInfo {
        name: mission.name.clone(),
        path: mission.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        dirty: mission.dirty,
        recent: load_session(app).recent,
    })
}

fn session_file_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("session.json"))
}

fn load_session(app: &AppHandle) -> SessionFile {
    session_file_path(app)
        .map(|path| read_session(&path))
        .unwrap_or_default()
}

fn remember_recent(app: &AppHandle, name: &str, path: &Path) -> Result<(), String> {
    let Some(session_path) = session_file_path(app) else {
        return Ok(());
    };
    let mut session = read_session(&session_path);
    push_recent(&mut session, name, path);
    write_session(&session_path, &session)
}

fn update_title(app: &AppHandle, state: &AppState) {
    let Ok(mission) = state.mission.lock() else {
        return;
    };
    let dirty = if mission.dirty { " •" } else { "" };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_title(&format!("FauxRRT — {}{dirty}", mission.name));
    }
}

fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let cleaned: String = trimmed
        .chars()
        .map(|ch| if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { ch })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.');
    if cleaned.is_empty() {
        "mission".into()
    } else {
        cleaned.to_string()
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState {
            store: Mutex::new(Store::new()),
            mission: Mutex::new(MissionSession::default()),
        })
        .invoke_handler(tauri::generate_handler![
            load_files,
            load_folder,
            load_boats,
            load_sample_boats,
            import_boats_kml,
            get_boats,
            clear_boats,
            remove_boat,
            set_boat_visible,
            score_boats,
            load_demo,
            load_risk_demo,
            track_positions,
            clear_tracks,
            remove_track,
            remove_tracks,
            set_visible,
            remap_track,
            get_risk_model,
            create_object,
            set_object_source,
            generate_trajectories,
            generate_rocketpy,
            rocketpy_status,
            sample_state_vector,
            upsert_debris_catalog,
            remove_debris_catalog,
            simulate_spent_stage,
            simulate_fts,
            simulate_nav_failure,
            rename_object,
            remove_object,
            create_failure_mode,
            update_failure_mode,
            remove_failure_mode,
            assign_tracks,
            unassign_tracks,
            set_track_weight,
            normalize_object,
            extract_impacts,
            compute_kde,
            get_mission,
            new_mission,
            open_mission,
            open_mission_path,
            open_last_mission,
            save_mission,
            save_mission_as,
            rename_mission,
            set_mission_ui,
            set_mission_wind
        ])
        .run(tauri::generate_context!())
        .expect("error while running FauxRRT");
}
