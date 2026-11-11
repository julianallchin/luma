//! The renderer's asset cache: decoded GLBs, generated meshes, floor sets and
//! HDR environments, each loaded once per path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use glam::Vec3;
use luma_scene::mesh::{load, Glb, MeshBounds, MeshData};

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
    footprints: HashMap<[u32; 2], crate::panel::Footprint>,
    fixtures: HashMap<String, Option<std::sync::Arc<luma_fixture::Built>>>,
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
            footprints: HashMap::new(),
            fixtures: HashMap::new(),
        }
    }

    /// The footprint of a beam with these edges
    /// ([`crate::panel::Footprint::of`]), worked out once. A rig's faces
    /// share a handful of beams, and the table behind one is thousands of
    /// samples of the profile: a venue of sixty bars spent 3 ms of every
    /// frame's build on theirs.
    pub(crate) fn footprint(&mut self, cos_beam: f32, cos_field: f32) -> crate::panel::Footprint {
        self.footprints
            .entry([cos_beam.to_bits(), cos_field.to_bits()])
            .or_insert_with(|| crate::panel::Footprint::of(cos_beam, cos_field))
            .clone()
    }

    /// A fixture of the library that ships beside the meshes
    /// (`fixture-library/<version>/fixtures/<id>.json`, the newest version),
    /// built once: the room's own lamps hang one. `None`, said once, where
    /// there is no library or it has no such fixture.
    pub(crate) fn fixture(&mut self, id: &str) -> Option<std::sync::Arc<luma_fixture::Built>> {
        if let Some(known) = self.fixtures.get(id) {
            return known.clone();
        }
        let built = (|| {
            let versions = self.root.parent()?.join("fixture-library");
            let newest = std::fs::read_dir(versions)
                .ok()?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .max()?;
            let spec = newest.join("fixtures").join(format!("{id}.json"));
            let text = std::fs::read_to_string(&spec)
                .inspect_err(|error| eprintln!("{}: {error}", spec.display()))
                .ok()?;
            let fixture = luma_fixture::Fixture::from_json(&text)
                .inspect_err(|error| eprintln!("{}: {error}", spec.display()))
                .ok()?;
            luma_fixture::build(&fixture)
                .inspect_err(|error| eprintln!("{}: {error}", spec.display()))
                .ok()
                .map(std::sync::Arc::new)
        })();
        self.fixtures.insert(id.to_owned(), built.clone());
        built
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

impl MeshBounds for Library {
    fn bounds(&mut self, mesh: &str) -> anyhow::Result<(Vec3, Vec3)> {
        Ok(self.get(mesh)?.bounds())
    }
}
