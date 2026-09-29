use crate::*;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub(super) struct Geometry {
    pub positions: Vec<f64>,
    pub leans: Vec<(f64, [f64; 3])>,
    pub mirrors: Vec<Option<[f64; 3]>>,
    pub units: Vec<usize>,
    pub keys: Vec<String>,
    pub representatives: Vec<usize>,
}
pub(super) fn resolve(
    cells: &[Cell],
    seed: u64,
    axis: &MappingSpec,
    grain: Grain,
) -> Result<Geometry> {
    let mut by_fixture: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (n, c) in cells.iter().enumerate() {
        by_fixture
            .entry(crate::mapping::fixture_of(&c.id).into())
            .or_default()
            .push(n);
    }
    let mut units = vec![0; cells.len()];
    let mut keys = Vec::new();
    let mut grouped = cells.to_vec();
    for members in by_fixture.values_mut() {
        members.sort_by_key(|n| {
            cells[*n]
                .id
                .rsplit_once(':')
                .and_then(|(_, n)| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let size = if grain.size() == 0 {
            members.len()
        } else {
            grain.size()
        };
        for chunk in members.chunks(size.max(1)) {
            let unit = keys.len();
            keys.push(cells[chunk[0]].id.clone());
            let center: [f64; 3] = std::array::from_fn(|ch| {
                chunk.iter().map(|n| cells[*n].uvz[ch]).sum::<f64>() / chunk.len() as f64
            });
            let world: [f64; 3] = std::array::from_fn(|ch| {
                chunk.iter().map(|n| cells[*n].world[ch]).sum::<f64>() / chunk.len() as f64
            });
            for n in chunk {
                units[*n] = unit;
                grouped[*n].uvz = center;
                grouped[*n].world = world;
            }
        }
    }
    let mut representatives = Vec::new();
    for (unit, key) in keys.iter().enumerate() {
        if let Some(n) = units.iter().position(|u| *u == unit) {
            let mut c = grouped[n].clone();
            c.id = key.clone();
            representatives.push(c);
        }
    }
    let coordinates: BTreeMap<_, _> = axis
        .resolve(&representatives, seed)?
        .coordinates
        .into_iter()
        .map(|c| (c.cell, c.position))
        .collect();
    let leans = axis.leans(&representatives, seed)?;
    let mirrors = axis.mirrored(&representatives)?;
    let geometry = Geometry {
        positions: units
            .iter()
            .map(|g| coordinates.get(&keys[*g]).copied().unwrap_or(0.5))
            .collect(),
        leans: units
            .iter()
            .map(|g| leans.get(&keys[*g]).copied().unwrap_or((0., [0.; 3])))
            .collect(),
        mirrors: units
            .iter()
            .map(|g| mirrors.get(keys[*g].as_str()).copied())
            .collect(),
        representatives: (0..keys.len())
            .map(|unit| {
                units
                    .iter()
                    .position(|u| *u == unit)
                    .expect("unit has a head")
            })
            .collect(),
        units,
        keys,
    };
    Ok(geometry)
}

pub(super) fn default_axis(source: MappingSource) -> MappingSpec {
    MappingSpec {
        source,
        span: Span::Selection,
        plane: None,
        per_group: false,
        reverse: false,
        mirror: None,
    }
}
pub(super) fn identity(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
pub(super) fn salt(name: &str) -> f64 {
    (identity(name) % 10_000) as f64 + 0.5
}
