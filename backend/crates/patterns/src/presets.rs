//! Shipped presets: named input values of a form, and named curves for
//! `time` and `hit` sources. The data lives in `presets.json`.
use crate::{BlendMode, Clip, Error, Keyframes, Library, Result, Selection, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A named look: a form and a value for every one of its inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormPreset {
    pub name: String,
    pub form: String,
    pub inputs: BTreeMap<String, Value>,
}

/// A named curve over progress 0–1, with values 0–1. A caller scales the
/// values to the input's range before it stores the source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurvePreset {
    pub name: String,
    pub curve: Keyframes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presets {
    pub presets: Vec<FormPreset>,
    pub curves: Vec<CurvePreset>,
}

/// The shipped presets and curves, in menu order.
pub fn presets() -> &'static Presets {
    static PRESETS: std::sync::OnceLock<Presets> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| {
        serde_json::from_str(include_str!("presets.json")).expect("shipped presets")
    })
}

impl Presets {
    pub fn preset(&self, name: &str) -> Option<&FormPreset> {
        self.presets.iter().find(|preset| preset.name == name)
    }
    pub fn curve(&self, name: &str) -> Option<&Keyframes> {
        self.curves
            .iter()
            .find(|curve| curve.name == name)
            .map(|curve| &curve.curve)
    }
}

impl FormPreset {
    /// A new clip of this preset: the form and a copy of every value.
    pub fn clip(&self, start: f64, duration: f64) -> Clip {
        Clip {
            graph: self.form.clone(),
            start,
            duration,
            seed: 0,
            selection_seed: None,
            selection: Selection::all(),
            z_index: 0,
            blend_mode: BlendMode::Replace,
            inputs: self.inputs.clone(),
        }
    }
    pub fn validate(&self, library: &Library) -> Result<()> {
        let definition = library
            .definitions
            .get(&self.form)
            .filter(|_| crate::is_form(&self.form))
            .ok_or_else(|| Error(format!("{}: unknown form {}", self.name, self.form)))?;
        crate::forms::check_inputs(&self.form, definition, &self.inputs)
            .map_err(|error| Error(format!("{}: {error}", self.name)))
    }
}
