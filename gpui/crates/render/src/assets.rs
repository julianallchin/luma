//! The renderer's asset cache: decoded GLBs, generated meshes, floor sets and
//! HDR environments, each loaded once per path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use luma_scene::mesh::{load, Glb, MeshData};

/// Decoded linear RGB pixels from a Radiance HDR environment.
#[derive(Clone)]
pub struct HdrImage {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Tightly packed linear RGB32F pixels.
    pub rgb: Arc<[f32]>,
}

/// Loads and caches GLBs by path so repeated fixtures share one parse.
#[derive(Default)]
pub struct Library {
    root: PathBuf,
    loaded: HashMap<String, Glb>,
    environments: HashMap<String, HdrImage>,
    procedural: HashMap<String, MeshData>,
    floors: HashMap<(String, bool), crate::floor::Maps>,
}

impl Library {
    /// A library rooted at the directory holding the mesh tree.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            loaded: HashMap::new(),
            environments: HashMap::new(),
            procedural: HashMap::new(),
            floors: HashMap::new(),
        }
    }

    /// Generated geometry under a key that already names every parameter
    /// that changes a vertex — [`MeshData::key`]'s contract.
    ///
    /// The frame's own mesh bank is rebuilt per frame, so without this the
    /// lattice of every truss in the venue is baked again for every frame
    /// drawn. The data is immutable and `MeshData` is two `Arc`s, so a hit
    /// costs a hash and two refcounts.
    pub fn procedural(&mut self, key: &str, build: impl FnOnce() -> MeshData) -> MeshData {
        if let Some(mesh) = self.procedural.get(key) {
            return mesh.clone();
        }
        let mesh = build();
        self.procedural.insert(key.to_string(), mesh.clone());
        mesh
    }

    /// The parsed asset at `rel`, loading it on first use.
    ///
    /// # Errors
    /// Fails if the file is missing or is not a readable glTF binary.
    pub fn get(&mut self, rel: &str) -> anyhow::Result<&Glb> {
        if !self.loaded.contains_key(rel) {
            let mut glb = load(&self.root.join(rel))?;
            crate::materials::apply(rel, &mut glb);
            self.loaded.insert(rel.to_string(), glb);
        }
        Ok(&self.loaded[rel])
    }

    /// Floor material set `name`, decoded and packed once, at half size when
    /// `half` (see [`crate::floor`]).
    ///
    /// # Errors
    /// Fails if the set's images are missing or unreadable.
    pub fn floor(&mut self, name: &str, half: bool) -> anyhow::Result<crate::floor::Maps> {
        let key = (name.to_string(), half);
        if !self.floors.contains_key(&key) {
            let maps = crate::floor::load(&self.root, name, half)?;
            self.floors.insert(key.clone(), maps);
        }
        Ok(self.floors[&key].clone())
    }

    /// Decode and cache a Radiance HDR environment by stable relative path.
    ///
    /// # Errors
    /// Fails if the file is missing or is not a readable Radiance HDR image.
    pub fn environment(&mut self, rel: &str) -> anyhow::Result<&HdrImage> {
        if !self.environments.contains_key(rel) {
            let path = self.root.join(rel);
            let image = image::load(
                std::io::BufReader::new(std::fs::File::open(&path)?),
                image::ImageFormat::Hdr,
            )?
            .into_rgb32f();
            let (width, height) = image.dimensions();
            anyhow::ensure!(
                width > 0 && height > 0,
                "empty HDR environment: {}",
                path.display()
            );
            let rgb = image
                .into_raw()
                .into_iter()
                .map(|channel| {
                    if channel.is_finite() {
                        channel.max(0.0)
                    } else {
                        0.0
                    }
                })
                .collect::<Vec<_>>()
                .into();
            self.environments
                .insert(rel.to_string(), HdrImage { width, height, rgb });
        }
        Ok(&self.environments[rel])
    }
}
