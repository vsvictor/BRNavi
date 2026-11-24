//! Opt-in stage 2 for the two BRNavi backend units.
//!
//! `COOPNAV_STAGE2=false` (the default) does not read Redis. Both units must
//! share one epoch and one donor brand when the flag is true.

use anyhow::{Context, bail};
use json_rpc_2_coopnav::cooperative::{GtfsConfig, VehicleClass};
use std::collections::HashMap;

const GTFS_VARS: [&str; 4] = [
    "COOPNAV_GTFS_URL",
    "COOPNAV_GTFS_ROUTES",
    "COOPNAV_GTFS_ACCURACY_METERS",
    "COOPNAV_GTFS_TRUST_SCORE",
];

pub struct Stage2Env {
    pub redis_url: String,
    pub epoch: String,
    pub brand: String,
    pub gtfs: Option<GtfsConfig>,
}

impl std::fmt::Debug for Stage2Env {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Stage2Env")
            .field("redis_url", &self.redis_url)
            .field("epoch", &self.epoch)
            .field("brand", &self.brand)
            .field("gtfs", &self.gtfs.is_some())
            .finish()
    }
}

pub fn stage2_from_env() -> anyhow::Result<Option<Stage2Env>> {
    stage2_from_vars(|name| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} must be UTF-8"),
    })
}

pub fn stage2_from_vars(
    mut lookup: impl FnMut(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<Option<Stage2Env>> {
    let enabled = match lookup("COOPNAV_STAGE2")? {
        Some(value) => parse_bool("COOPNAV_STAGE2", &value)?,
        None => false,
    };
    if !enabled {
        return Ok(None);
    }
    let epoch = required(&mut lookup, "COOPNAV_EPOCH")?;
    let brand = required(&mut lookup, "COOPNAV_COOP_BRAND")?;
    let redis_url = required(&mut lookup, "COOPNAV_REDIS_URL")?;
    let gtfs = gtfs_from_vars(&mut lookup, &brand)?;
    Ok(Some(Stage2Env {
        redis_url,
        epoch,
        brand,
        gtfs,
    }))
}

fn required(
    lookup: &mut impl FnMut(&str) -> anyhow::Result<Option<String>>,
    name: &str,
) -> anyhow::Result<String> {
    match lookup(name)? {
        Some(value) if !value.is_empty() && value.trim() == value => Ok(value),
        _ => bail!("{name} is required when COOPNAV_STAGE2=true"),
    }
}

fn gtfs_from_vars(
    lookup: &mut impl FnMut(&str) -> anyhow::Result<Option<String>>,
    brand: &str,
) -> anyhow::Result<Option<GtfsConfig>> {
    let mut values = Vec::with_capacity(GTFS_VARS.len());
    for name in GTFS_VARS {
        values.push(lookup(name)?.filter(|value| !value.is_empty()));
    }
    if values.iter().all(Option::is_none) || values.iter().any(Option::is_none) {
        return Ok(None);
    }
    let url = values[0].clone().unwrap();
    let routes = values[1].clone().unwrap();
    let accuracy_meters: f64 = values[2]
        .as_deref()
        .unwrap()
        .parse()
        .context("COOPNAV_GTFS_ACCURACY_METERS must be numeric")?;
    let trust_score: f64 = values[3]
        .as_deref()
        .unwrap()
        .parse()
        .context("COOPNAV_GTFS_TRUST_SCORE must be numeric")?;
    let mut route_classes = HashMap::new();
    for item in routes.split(',') {
        let (class, id) = item
            .split_once(':')
            .context("GTFS route mapping must be class:routeId")?;
        let class = match class {
            "bus" => VehicleClass::Bus,
            "trolleybus" => VehicleClass::Trolleybus,
            _ => bail!("GTFS route mapping supports bus and trolleybus only"),
        };
        if route_classes.insert(id.to_owned(), class).is_some() {
            bail!("duplicate GTFS route mapping");
        }
    }
    Ok(Some(GtfsConfig {
        url,
        brand_id: brand.to_owned(),
        route_classes,
        accuracy_meters,
        trust_score,
    }))
}

fn parse_bool(name: &str, value: &str) -> anyhow::Result<bool> {
    value
        .parse()
        .with_context(|| format!("{name} must be true or false"))
}
