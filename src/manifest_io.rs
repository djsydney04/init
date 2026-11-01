//! Format selection for the shared, typed service manifest.
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

enum Format {
    Json,
    Yaml,
}

fn format(path: &Path) -> Result<Format> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => Ok(Format::Json),
        Some("yaml" | "yml") => Ok(Format::Yaml),
        _ => bail!(
            "Manifest must have a .json, .yaml, or .yml extension: {}",
            path.display()
        ),
    }
}

pub fn decode<T: DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T> {
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "Manifest exceeds the 1 MiB size limit"
    );
    match format(path)? {
        Format::Json => serde_json::from_slice(bytes)
            .with_context(|| format!("Invalid JSON manifest {}", path.display())),
        Format::Yaml => serde_saphyr::from_slice(bytes)
            .with_context(|| format!("Invalid YAML manifest {}", path.display())),
    }
}

pub fn encode<T: Serialize>(path: &Path, value: &T) -> Result<Vec<u8>> {
    let mut bytes = match format(path)? {
        Format::Json => serde_json::to_vec_pretty(value)?,
        Format::Yaml => serde_saphyr::to_string(value)?.into_bytes(),
    };
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "Manifest exceeds the 1 MiB size limit"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Manifest;

    #[test]
    fn yaml_and_json_share_the_typed_manifest() {
        let yaml = Path::new("services.yaml");
        let manifest: Manifest = decode(yaml, include_bytes!("../examples/services.yaml")).unwrap();
        manifest.validate().unwrap();
        let json = Path::new("services.json");
        let from_json: Manifest = decode(json, &encode(json, &manifest).unwrap()).unwrap();
        let roundtrip: Manifest = decode(yaml, &encode(yaml, &from_json).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(manifest).unwrap(),
            serde_json::to_value(roundtrip).unwrap()
        );
    }

    #[test]
    fn rejects_duplicate_keys_documents_and_schema_errors() {
        let path = Path::new("services.yaml");
        for bytes in [
            b"version: 1\nversion: 2\n".as_slice(),
            b"version: 1\n---\nversion: 1\n",
            b"version: 1\nengine: {command: [docker]}\nbudget: {cpus: 2, memory_mb: 1024}\nservices: []\nunknown: true\n",
            b"version: 1\n",
        ] {
            assert!(decode::<Manifest>(path, bytes).is_err());
        }
    }

    #[test]
    fn rejects_oversize_and_unsupported_formats() {
        let bytes = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert!(decode::<Manifest>(Path::new("services.yaml"), &bytes).is_err());
        assert!(decode::<Manifest>(Path::new("services.txt"), b"{}").is_err());
        assert!(encode(Path::new("services"), &serde_json::json!({})).is_err());
        let huge = "a".repeat(MAX_MANIFEST_BYTES + 1);
        assert!(encode(Path::new("services.json"), &huge).is_err());
    }

    #[test]
    fn yml_extension_and_newline_are_supported() {
        let path = Path::new("services.yml");
        let bytes = encode(path, &serde_json::json!({"name": "worker"})).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let value: serde_json::Value = decode(path, &bytes).unwrap();
        assert_eq!(value["name"], "worker");
    }
}
