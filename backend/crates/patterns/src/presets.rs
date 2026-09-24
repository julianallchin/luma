//! Shipped presets: named input values of a form, named curves for `time`
//! and `hit` sources, named gradients, and named frequency ranges for
//! `audio` sources. The data lives in `presets.json`.
use crate::{BlendMode, Clip, Error, Gradient, Keyframes, Library, Result, Selection, Value};
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
    /// The input key this curve is made for. `None` is the general set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    pub curve: Keyframes,
}

/// A named gradient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradientPreset {
    pub name: String,
    pub gradient: Gradient,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presets {
    pub presets: Vec<FormPreset>,
    pub curves: Vec<CurvePreset>,
    pub gradients: Vec<GradientPreset>,
    pub frequencies: Vec<FrequencyPreset>,
}

/// A named frequency range of the full mix for an `audio` source. Picking
/// one only fills `from_hz` and `to_hz`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrequencyPreset {
    pub name: String,
    pub from_hz: f64,
    pub to_hz: f64,
}

/// The shipped presets and curves, in menu order.
pub fn presets() -> &'static Presets {
    static PRESETS: std::sync::OnceLock<Presets> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| {
        serde_json::from_str(include_str!("presets.json")).expect("shipped presets")
    })
}

impl Presets {
    /// The preset of `form` called `name`. A name is unique within its form
    /// only: Chase and Aim each have a Wave.
    pub fn preset(&self, form: &str, name: &str) -> Option<&FormPreset> {
        self.presets
            .iter()
            .find(|preset| preset.form == form && preset.name == name)
    }
    /// The general curve called `name`.
    pub fn curve(&self, name: &str) -> Option<&Keyframes> {
        self.curves
            .iter()
            .find(|curve| curve.input.is_none() && curve.name == name)
            .map(|curve| &curve.curve)
    }
    /// The curves offered for `input`: its own set where it has one, the
    /// general set otherwise. In menu order.
    pub fn curves_for<'a>(&'a self, input: &'a str) -> impl Iterator<Item = &'a CurvePreset> {
        let own = self
            .curves
            .iter()
            .any(|curve| curve.input.as_deref() == Some(input));
        let wanted = own.then_some(input);
        self.curves
            .iter()
            .filter(move |curve| curve.input.as_deref() == wanted)
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
