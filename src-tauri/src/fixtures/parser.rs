use crate::models::fixtures::{FixtureDefinition, FixtureEntry};
use quick_xml::de::from_str;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub struct FixtureIndex {
    pub entries: Vec<FixtureEntry>,
}

pub fn build_index(root_path: &Path) -> Result<FixtureIndex, std::io::Error> {
    let mut entries = Vec::new();

    // Walk the directory
    // Structure: Root/Manufacturer/Model.qxf
    // min_depth 2 ensures we are inside a Manufacturer folder looking at a file
    for entry in WalkDir::new(root_path)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("qxf") {
            // Let's get relative path
            if let Ok(relative) = path.strip_prefix(root_path) {
                let path_str = relative.to_string_lossy().to_string();

                // Infer manufacturer and model from path to avoid opening every file
                // Typically: Manufacturer/Manufacturer-Model.qxf or similar
                if let Some(parent) = relative.parent() {
                    let manufacturer = parent
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let model_filename = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    // Clean up model name if it starts with manufacturer (common in QLC+)
                    // e.g. Acme-Dotline180 -> Dotline180 if in Acme folder
                    let model = if model_filename.starts_with(&format!("{}-", manufacturer)) {
                        model_filename[manufacturer.len() + 1..].to_string()
                    } else {
                        model_filename
                    };

                    entries.push(FixtureEntry {
                        manufacturer,
                        model,
                        path: path_str,
                    });
                }
            }
        }
    }

    // Sort by manufacturer then model
    entries.sort_by(|a, b| {
        a.manufacturer
            .cmp(&b.manufacturer)
            .then(a.model.cmp(&b.model))
    });

    Ok(FixtureIndex { entries })
}

pub fn parse_definition(path: &Path) -> Result<FixtureDefinition, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    from_str(&content).map_err(|e| e.to_string())
}
