use serde::{Deserialize, Serialize};

/// Where one projector may send light: a polygon in its own view, galvo
/// coordinates −1 to 1 across and up its scan. Light outside it is
/// blanked.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaserLimit {
    pub fixture_id: String,
    pub points: Vec<[f64; 2]>,
}

impl LaserLimit {
    /// Whether `points` are a limit: a polygon of 3 to 64 corners, each
    /// inside the scan.
    ///
    /// # Errors
    /// Says what a limit is.
    pub fn check(points: &[[f64; 2]]) -> Result<(), String> {
        let inside = points
            .iter()
            .flatten()
            .all(|c| c.is_finite() && c.abs() <= 1.);
        if (3..=64).contains(&points.len()) && inside {
            return Ok(());
        }
        Err("A laser limit is a polygon of 3 to 64 corners, each between -1 and 1 across and up the scan".into())
    }
}

/// The kind of DAC a projector is played through. One driver per kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum LaserDeviceKind {
    /// An Ether Dream, on the network.
    EtherDream,
}

impl LaserDeviceKind {
    /// The `laser_routes.device_kind` spelling, which is also the wire one.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LaserDeviceKind::EtherDream => "ether-dream",
        }
    }
}

impl TryFrom<String> for LaserDeviceKind {
    type Error = String;

    fn try_from(kind: String) -> Result<Self, String> {
        match kind.as_str() {
            "ether-dream" => Ok(LaserDeviceKind::EtherDream),
            other => Err(format!("{other} is not a kind of laser DAC")),
        }
    }
}

/// Which DAC plays a projector: this machine's wiring, never synced.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LaserRoute {
    /// The projector's fixture id.
    pub fixture_id: String,
    #[sqlx(try_from = "String")]
    pub device_kind: LaserDeviceKind,
    /// What names the DAC wherever it is plugged in: an Ether Dream's MAC
    /// address.
    pub device_id: String,
    pub device_name: Option<String>,
    /// The `ip:port` to connect to until the DAC is heard on the network.
    pub address: Option<String>,
}

/// A DAC a driver can see now, bound or not.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaserDevice {
    pub kind: LaserDeviceKind,
    pub id: String,
    pub name: String,
    /// The `ip:port` it answers on.
    pub address: String,
    /// The most points a second it plays.
    pub max_rate: u32,
}

/// How a projector's DAC is doing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", tag = "state", content = "detail")]
pub enum LaserLink {
    /// Its DAC has not been found, or does not answer.
    Searching,
    /// Connected and dark: output is off, or there is nothing to draw.
    Idle,
    /// Playing, at this many scans a second.
    Playing(f64),
    /// The DAC refuses to play; why.
    Fault(String),
}

/// Laser output as a whole: whether it is on, and each bound projector's
/// DAC.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaserOutput {
    /// Off at every launch; nothing is sent to a DAC until it is turned on.
    pub on: bool,
    /// The projector whose test pattern is showing.
    pub testing: Option<String>,
    /// By projector fixture id.
    pub links: std::collections::BTreeMap<String, LaserLink>,
}
