//! Parses only the approved release archive; no scraper or upstream executable code is used.
use std::{
    collections::HashSet,
    io::{Cursor, Read},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::domain::vehicle::{ARCHIVE_SHA256, CATALOG_VERSION, CatalogEntity, CatalogVariant};

pub const ARCHIVE_URL: &str =
    "https://github.com/gor3a/vehicle-makes-models/releases/download/v1.0.2/vehicle-data-json.zip";

pub struct ParsedCatalog {
    pub makes: Vec<CatalogEntity>,
    pub models: Vec<CatalogEntity>,
    pub generations: Vec<CatalogEntity>,
    pub variants: Vec<CatalogVariant>,
    pub invalid_measurements: usize,
}

#[derive(Deserialize)]
struct SourceFile {
    makes: Vec<SourceMake>,
}
#[derive(Deserialize)]
struct SourceMake {
    name: String,
    models: Vec<SourceModel>,
}
#[derive(Deserialize)]
struct SourceModel {
    name: String,
    #[serde(default)]
    generations: Vec<SourceGeneration>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceGeneration {
    name: String,
    year_start: Option<i32>,
    year_end: Option<i32>,
    #[serde(default)]
    engines: Vec<SourceEngine>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceEngine {
    label: String,
    length_mm: Option<f64>,
    width_mm: Option<f64>,
    height_mm: Option<f64>,
    curb_weight_kg: Option<f64>,
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn parse_pinned_archive(bytes: &[u8]) -> Result<ParsedCatalog> {
    ensure!(bytes.len() <= 50 * 1024 * 1024, "Catalog archive too large");
    ensure!(
        sha256(bytes) == ARCHIVE_SHA256,
        "Catalog SHA-256 mismatch; database unchanged"
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("Invalid catalog archive")?;
    let mut names = Vec::new();
    let mut unpacked = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        // Never extract paths, and never import the all.json aggregate a second time.
        if name.starts_with("json/") && name.ends_with(".json") && name != "json/all.json" {
            ensure!(
                !name.contains("..") && name.matches('/').count() == 1,
                "Unsafe catalog path"
            );
            unpacked += entry.size();
            ensure!(unpacked < 200 * 1024 * 1024, "Expanded catalog too large");
            names.push(name.to_owned());
        }
    }
    names.sort();
    ensure!(names.len() == 164, "Unexpected pinned catalog file count");
    let mut parsed = ParsedCatalog::empty();
    let mut ids = HashSet::new();
    for name in names {
        let mut text = String::new();
        archive
            .by_name(&name)?
            .take(32 * 1024 * 1024)
            .read_to_string(&mut text)?;
        parse_file(&name, &text, &mut parsed, &mut ids)?;
    }
    ensure!(
        (
            parsed.makes.len(),
            parsed.models.len(),
            parsed.generations.len(),
            parsed.variants.len()
        ) == (164, 2649, 7217, 30535),
        "Unexpected pinned catalog record counts"
    );
    Ok(parsed)
}

impl ParsedCatalog {
    fn empty() -> Self {
        Self {
            makes: vec![],
            models: vec![],
            generations: vec![],
            variants: vec![],
            invalid_measurements: 0,
        }
    }

    #[cfg(test)]
    pub fn fixture(text: &str) -> Result<Self> {
        let mut parsed = Self::empty();
        parse_file("json/fixture.json", text, &mut parsed, &mut HashSet::new())?;
        Ok(parsed)
    }
}

fn checked_name(value: &str) -> Result<()> {
    ensure!(!value.trim().is_empty() && value.len() <= 1024, "Invalid catalog name");
    Ok(())
}

fn stable_id(kind: &str, path: &str, ids: &mut HashSet<String>) -> Result<String> {
    let id = format!("{kind}-{}", sha256(format!("{CATALOG_VERSION}:{path}").as_bytes()));
    ensure!(ids.insert(id.clone()), "Catalog path/ID collision");
    Ok(id)
}

fn entity(
    kind: &str,
    name: &str,
    path: &str,
    parent_id: Option<String>,
    year_start: Option<i32>,
    year_end: Option<i32>,
    ids: &mut HashSet<String>,
) -> Result<CatalogEntity> {
    checked_name(name)?;
    Ok(CatalogEntity {
        id: stable_id(kind, path, ids)?,
        name: name.to_owned(),
        parent_id,
        year_start,
        year_end,
        source_path: path.to_owned(),
        version: CATALOG_VERSION.to_owned(),
    })
}

fn measurement(value: Option<f64>, min: f64, max: f64, invalid: &mut usize) -> Option<f64> {
    value.and_then(|value| {
        if value.is_finite() && value >= min && value <= max {
            Some(value)
        } else {
            *invalid += 1;
            None
        }
    })
}

fn parse_file(name: &str, text: &str, out: &mut ParsedCatalog, ids: &mut HashSet<String>) -> Result<()> {
    let source: SourceFile = serde_json::from_str(text).context("Invalid catalog source schema")?;
    for (mi, make) in source.makes.into_iter().enumerate() {
        let mp = format!("{name}#/makes/{mi}");
        let m = entity("make", &make.name, &mp, None, None, None, ids)?;
        for (moi, model) in make.models.into_iter().enumerate() {
            let mop = format!("{mp}/models/{moi}");
            let mo = entity("model", &model.name, &mop, Some(m.id.clone()), None, None, ids)?;
            for (gi, generation) in model.generations.into_iter().enumerate() {
                for year in [generation.year_start, generation.year_end].into_iter().flatten() {
                    ensure!((1800..=2100).contains(&year), "Invalid catalog generation year");
                }
                if let (Some(start), Some(end)) = (generation.year_start, generation.year_end) {
                    ensure!(start <= end, "Inverted catalog generation years");
                }
                let gp = format!("{mop}/generations/{gi}");
                let g = entity(
                    "generation",
                    &generation.name,
                    &gp,
                    Some(mo.id.clone()),
                    generation.year_start,
                    generation.year_end,
                    ids,
                )?;
                for (ei, engine) in generation.engines.into_iter().enumerate() {
                    checked_name(&engine.label)?;
                    let ep = format!("{gp}/engines/{ei}");
                    let id = stable_id("variant", &ep, ids)?;
                    out.variants.push(CatalogVariant {
                        id,
                        make_id: m.id.clone(),
                        model_id: mo.id.clone(),
                        generation_id: g.id.clone(),
                        make: m.name.clone(),
                        model: mo.name.clone(),
                        generation: g.name.clone(),
                        engine: engine.label,
                        year_start: g.year_start,
                        year_end: g.year_end,
                        length_mm: measurement(engine.length_mm, 100.0, 50000.0, &mut out.invalid_measurements),
                        width_mm: measurement(engine.width_mm, 100.0, 10000.0, &mut out.invalid_measurements),
                        height_mm: measurement(engine.height_mm, 100.0, 10000.0, &mut out.invalid_measurements),
                        curb_weight_kg: measurement(
                            engine.curb_weight_kg,
                            1.0,
                            200000.0,
                            &mut out.invalid_measurements,
                        ),
                        source_path: ep,
                        version: CATALOG_VERSION.to_owned(),
                    });
                }
                out.generations.push(g);
            }
            out.models.push(mo);
        }
        out.makes.push(m);
    }
    if out.variants.len() > 100000 {
        bail!("Catalog record limit exceeded");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = r#"{"makes":[{"name":"Test","models":[{"name":"Empty"},{"name":"Model","generations":[{"name":"Same (2020)","yearStart":2020,"yearEnd":2024,"engines":[{"label":"Engine","lengthMm":4500,"widthMm":null,"heightMm":999999,"curbWeightKg":1200},{"label":"Engine"}]},{"name":"Same (2020)","engines":[{"label":"Engine"}]}]}]}]}"#;

    #[test]
    fn stable_source_paths_preserve_duplicates_and_unknown_values() {
        let a = ParsedCatalog::fixture(FIXTURE).unwrap();
        let b = ParsedCatalog::fixture(FIXTURE).unwrap();
        assert_eq!(a.variants.len(), 3);
        assert_eq!(a.models.len(), 2);
        assert_eq!(a.variants[0].id, b.variants[0].id);
        assert_ne!(a.variants[0].id, a.variants[1].id);
        assert_ne!(a.generations[0].id, a.generations[1].id);
        assert_eq!(a.variants[0].length_mm, Some(4500.0));
        assert_eq!(a.variants[0].width_mm, None);
        assert_eq!(a.variants[0].height_mm, None);
        assert_eq!(a.variants[1].curb_weight_kg, None);
        assert_eq!(a.invalid_measurements, 1);
    }

    #[test]
    fn rejects_tampering_and_invalid_schema() {
        assert!(parse_pinned_archive(b"untrusted archive").is_err());
        assert!(ParsedCatalog::fixture(r#"{"makes":[{"name":"","models":[]}]}"#).is_err());
        assert!(ParsedCatalog::fixture(r#"{"makes":[{"name":"A","models":[{"name":"B","generations":[{"name":"C","yearStart":2024,"yearEnd":2020}]}]}]}"#).is_err());
    }
}
