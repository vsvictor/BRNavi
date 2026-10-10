//! Explicit, operator-authorized pinned database import. Never invoked by normal startup.
use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
};

use anyhow::{Result, anyhow, bail, ensure};
use quic_jsonrpc_server::{
    application::catalog_import::{ARCHIVE_URL, parse_pinned_archive},
    domain::vehicle::license,
    env,
    infrastructure::{config::MongoOptions, persistence::vehicles::MongoVehicleRepository},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn write_rows<T: serde::Serialize>(path: PathBuf, rows: &[T]) -> Result<()> {
    let mut file = BufWriter::new(fs::File::create(path)?);
    for row in rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    Ok(())
}

fn run() -> Result<()> {
    let mut validate_only = false;
    let mut data_dir = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--validate-only" => validate_only = true,
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow!("--data-dir requires a directory"))?,
                ))
            }
            _ => bail!("Usage: import-catalog [--validate-only] [--data-dir PATH]; pinned v1.0.2 only"),
        }
    }
    env::load_dotenv_checked()?;
    let directory = data_dir.unwrap_or_else(|| env::workspace_root().join("data/vehicle-catalog/v1.0.2"));
    fs::create_dir_all(&directory)?;
    let archive = directory.join("vehicle-data-json.zip");
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        if !archive.is_file() {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build()?;
            let mut response = client.get(ARCHIVE_URL).send().await?.error_for_status()?;
            ensure!(response.content_length().is_none_or(|n| n <= 50*1024*1024), "Catalog archive too large");
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(bytes.len()+chunk.len() <= 50*1024*1024, "Catalog archive too large");
                bytes.extend_from_slice(&chunk);
            }
            // Do not retain a download whose digest/schema is invalid.
            parse_pinned_archive(&bytes)?;
            fs::write(&archive,bytes)?;
        }
        let bytes = fs::read(&archive)?;
        let parsed = parse_pinned_archive(&bytes)?;
        for name in ["LICENSE-DATA","ODbL-1.0.html"] {
            ensure!(directory.join(name).is_file(), "Required catalog license artifact missing: {name}");
        }
        write_rows(directory.join("adapted-makes.jsonl"), &parsed.makes)?;
        write_rows(directory.join("adapted-models.jsonl"), &parsed.models)?;
        write_rows(directory.join("adapted-generations.jsonl"), &parsed.generations)?;
        write_rows(directory.join("adapted-variants.jsonl"), &parsed.variants)?;
        fs::write(directory.join("adapted-metadata.json"),serde_json::to_vec_pretty(&license())?)?;
        if !validate_only {
            let repository = MongoVehicleRepository::connect(&MongoOptions::from_env()?).await
                .map_err(|_| anyhow!("Unable to initialize catalog database collections"))?;
            repository.import(&parsed).await.map_err(|_| anyhow!("Catalog database import failed; readiness remains unavailable unless already complete. Inspect catalog metadata and operator logs; no private data was imported."))?;
        }
        println!("{}: {} makes, {} models, {} generations, {} variants; {} invalid measurements replaced by null. Artifacts: {}",
            if validate_only { "Validated" } else { "Imported" },parsed.makes.len(),parsed.models.len(),parsed.generations.len(),parsed.variants.len(),
            parsed.invalid_measurements,directory.display());
        Ok(())
    })
}
