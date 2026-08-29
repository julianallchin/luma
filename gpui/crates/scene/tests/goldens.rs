//! Golden-vector parity with the TypeScript stage builder.
//!
//! These are characterization tests, not specifications: they pin the exact
//! numeric output of `src/features/stage/lib/{snap,sockets}.ts` so this port
//! reproduces it rather than something that merely looks right. The two JSON
//! files are produced on the TS side — see
//! `src/features/stage/lib/__tests__/snap-goldens.gen.ts` — and are read here
//! verbatim.
//!
//! Serialization conventions, from the generator:
//!   - matrices are 16 numbers in three.js `Matrix4.toArray()` order, i.e.
//!     **column-major**, which is also glam's `to_cols_array`, so no transpose
//!     is needed here;
//!   - the frame is three.js Y-up right-handed, quaternions are `(x, y, z, w)`;
//!   - the snap pose is recorded as a composed world matrix rather than
//!     position + quaternion, because `q ≡ -q` makes raw components unstable
//!     across decompose implementations while the matrix is unique;
//!   - floats are rounded to 1e-6, and `Infinity` is the string `"Infinity"`.

use glam::{DMat4, DQuat, DVec3};
use luma_scene::aabb::DAabb;
use luma_scene::snap::{solve_snap, ScenePiece, SnapInput, SnapSurface};
use luma_scene::sockets::{
    resolve_anchor, resolve_socket, BboxAnchor, ResolvedSocket, SocketDef, SocketMode, SocketType,
};
use serde_json::Value;
use std::collections::HashMap;

/// Absolute, matching `snap.golden.test.ts`. Loose enough to absorb
/// cross-engine ULP noise in trig and sqrt, tight enough that a flipped axis,
/// a reordered multiply, or a different tie-break fails.
const SNAP_TOL: f64 = 1e-6;
/// `sockets.golden.test.ts` compares to 9 decimals.
const SOCKET_TOL: f64 = 1e-9;

fn golden(name: &str) -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../harness/goldens/").to_string() + name;
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str::<Value>(&text)
        .expect("golden is valid JSON")
        .as_array()
        .expect("golden is an array of cases")
        .clone()
}

fn nums(v: &Value) -> Vec<f64> {
    v.as_array()
        .expect("array of numbers")
        .iter()
        .map(|n| n.as_f64().expect("number"))
        .collect()
}

fn vec3(v: &Value) -> DVec3 {
    let n = nums(v);
    DVec3::new(n[0], n[1], n[2])
}

fn mat4(v: &Value) -> DMat4 {
    let n = nums(v);
    DMat4::from_cols_slice(&n)
}

fn close(actual: f64, expected: f64, tol: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{label}: got {actual}, want {expected} (Δ {})",
        (actual - expected).abs()
    );
}

fn close_vec(actual: DVec3, expected: &Value, tol: f64, label: &str) {
    let want = nums(expected);
    for (i, w) in want.iter().enumerate() {
        close(actual[i], *w, tol, &format!("{label}[{i}]"));
    }
}

// ---------------------------------------------------------------------------
// stage-sockets.json
// ---------------------------------------------------------------------------

fn bbox(v: &Value) -> DAabb {
    let pair = v.as_array().expect("[min, max]");
    DAabb::new(vec3(&pair[0]), vec3(&pair[1]))
}

fn socket_def(v: &Value) -> SocketDef {
    SocketDef {
        name: v["name"].as_str().expect("name").to_string(),
        socket_type: SocketType::from_name(v["type"].as_str().expect("type")).expect("socket type"),
        anchor: BboxAnchor::from_name(v["anchor"].as_str().expect("anchor")).expect("anchor"),
        offset: v.get("offset").map(vec3),
        normal: v.get("normal").map(vec3),
        tangent: v.get("tangent").map(vec3),
        mode: v
            .get("mode")
            .map(|m| SocketMode::from_name(m.as_str().expect("mode")).expect("mode"))
            .unwrap_or_default(),
        roll: None,
    }
}

#[test]
fn sockets_golden_vectors() {
    let cases = golden("stage-sockets.json");
    assert_eq!(cases.len(), 43, "case count changed");

    let mut anchor_cases = 0;
    let mut socket_cases = 0;
    for case in &cases {
        let name = case["case"].as_str().expect("case name");
        let input = &case["input"];
        let output = &case["output"];
        let bbox = bbox(&input["bbox"]);

        match input["fn"].as_str().expect("fn") {
            "resolveAnchor" => {
                anchor_cases += 1;
                let anchors = input["anchors"].as_array().expect("anchors");
                assert_eq!(
                    output.as_object().expect("output map").len(),
                    27,
                    "{name}: the golden must cover the whole anchor vocabulary"
                );
                for a in anchors {
                    let key = a.as_str().expect("anchor name");
                    let anchor = BboxAnchor::from_name(key).expect("known anchor");
                    close_vec(
                        resolve_anchor(anchor, &bbox),
                        &output[key],
                        SOCKET_TOL,
                        &format!("{name}/{key}"),
                    );
                }
            }
            "resolveSocket" => {
                socket_cases += 1;
                let got = resolve_socket(&socket_def(&input["def"]), &bbox);
                assert_eq!(got.name, output["name"].as_str().unwrap(), "{name}: name");
                assert_eq!(
                    got.socket_type.as_str(),
                    output["type"].as_str().unwrap(),
                    "{name}: type"
                );
                assert_eq!(
                    got.mode.as_str(),
                    output["mode"].as_str().unwrap(),
                    "{name}: mode"
                );
                close_vec(got.position, &output["position"], SOCKET_TOL, name);
                close_vec(got.normal, &output["normal"], SOCKET_TOL, name);
                close_vec(got.tangent, &output["tangent"], SOCKET_TOL, name);
                close_vec(got.outward, &output["outward"], SOCKET_TOL, name);
            }
            other => panic!("{name}: unknown fn {other}"),
        }
    }
    assert_eq!((anchor_cases, socket_cases), (4, 39));
}

// ---------------------------------------------------------------------------
// stage-snap.json
// ---------------------------------------------------------------------------

/// The synthetic socket tables the goldens were recorded against — the Rust
/// twin of `snap-fixtures.ts`. They mimic what `resolve_socket` would produce
/// for four canonical pieces but bypass anchor resolution, so the math under
/// test is transparent and no GLB loading is involved.
fn fixtures() -> HashMap<String, Vec<ResolvedSocket>> {
    fn socket(
        name: &str,
        socket_type: SocketType,
        position: DVec3,
        normal: DVec3,
        tangent: DVec3,
        mode: SocketMode,
        outward: DVec3,
    ) -> ResolvedSocket {
        ResolvedSocket {
            name: name.to_string(),
            socket_type,
            position,
            normal,
            tangent,
            mode,
            outward,
            roll: socket_type.roll(),
        }
    }
    use SocketMode::{Edge, Face};
    use SocketType::*;
    let x = DVec3::X;
    let z = DVec3::Z;
    let up = DVec3::Y;
    let down = DVec3::NEG_Y;
    let s = std::f64::consts::FRAC_1_SQRT_2;

    // 1×1×0.6 m deck, pivot at the bottom face; bbox centre at (0, 0.3, 0).
    let deck = vec![
        socket("grab", Grab, DVec3::new(0.0, 0.3, 0.0), up, x, Face, up),
        socket("bottom", BottomMount, DVec3::ZERO, down, x, Face, down),
        // No discrete floor_top socket — the surface fallback puts equipment
        // at the actual cursor hit point on the deck top.
        socket(
            "edge_front",
            FloorEdge,
            DVec3::new(0.0, 0.6, 0.5),
            up,
            x,
            Edge,
            z,
        ),
        socket(
            "edge_back",
            FloorEdge,
            DVec3::new(0.0, 0.6, -0.5),
            up,
            x,
            Edge,
            -z,
        ),
        socket(
            "edge_left",
            FloorEdge,
            DVec3::new(-0.5, 0.6, 0.0),
            up,
            z,
            Edge,
            -x,
        ),
        socket(
            "edge_right",
            FloorEdge,
            DVec3::new(0.5, 0.6, 0.0),
            up,
            z,
            Edge,
            x,
        ),
        socket(
            "corner_fl",
            FloorCorner,
            DVec3::new(-0.35, 0.6, 0.35),
            up,
            x,
            Face,
            DVec3::new(-s, 0.0, s),
        ),
        socket(
            "corner_fr",
            FloorCorner,
            DVec3::new(0.35, 0.6, 0.35),
            up,
            x,
            Face,
            DVec3::new(s, 0.0, s),
        ),
    ];
    // 1.22 m straight truss along X, centred at the origin.
    let truss = vec![
        socket("grab", Grab, DVec3::ZERO, up, x, Face, up),
        socket(
            "end_a",
            TrussEnd,
            DVec3::new(-0.61, 0.0, 0.0),
            -x,
            z,
            Face,
            -x,
        ),
        socket("end_b", TrussEnd, DVec3::new(0.61, 0.0, 0.0), x, z, Face, x),
    ];
    // 1 m speaker stand: centroid at the origin, top +0.5, base -0.5.
    let stand = vec![
        socket("grab", Grab, DVec3::ZERO, up, x, Face, up),
        socket("top", StandTop, DVec3::new(0.0, 0.5, 0.0), up, x, Face, up),
        socket(
            "base",
            StandBottom,
            DVec3::new(0.0, -0.5, 0.0),
            down,
            x,
            Face,
            down,
        ),
    ];
    // 0.4 m speaker, mount on the bottom face.
    let speaker = vec![
        socket("grab", Grab, DVec3::ZERO, up, x, Face, up),
        socket(
            "mount",
            SpeakerMount,
            DVec3::new(0.0, -0.2, 0.0),
            down,
            x,
            Face,
            down,
        ),
    ];
    HashMap::from([
        ("deck".to_string(), deck),
        ("truss".to_string(), truss),
        ("stand".to_string(), stand),
        ("speaker".to_string(), speaker),
    ])
}

fn pieces(v: &Value) -> Vec<ScenePiece> {
    v.as_array()
        .expect("pieces")
        .iter()
        .map(|p| ScenePiece {
            id: p["id"].as_str().expect("id").to_string(),
            mesh_path: p["meshPath"].as_str().expect("meshPath").to_string(),
            world_matrix: mat4(&p["worldMatrix"]),
        })
        .collect()
}

fn surface(v: &Value) -> SnapSurface {
    SnapSurface {
        piece_id: v["pieceId"].as_str().map(str::to_string),
        host_matrix: mat4(&v["hostMatrix"]),
        local_point: vec3(&v["localPoint"]),
        local_normal: vec3(&v["localNormal"]),
        surface_type: SocketType::from_name(v["type"].as_str().expect("type")).expect("type"),
    }
}

type Fixtures = HashMap<String, Vec<ResolvedSocket>>;

fn build_input<'a>(
    input: &'a Value,
    pieces: &'a [ScenePiece],
    surface: Option<&'a SnapSurface>,
    current_quaternion: Option<DQuat>,
    lookup: &'a Fixtures,
    exclude: Option<&'a str>,
) -> SnapInput<'a, Fixtures> {
    SnapInput {
        held_mesh_path: input["heldMeshPath"].as_str().expect("heldMeshPath"),
        cursor_world: vec3(&input["cursorWorld"]),
        current_quaternion,
        pieces,
        exclude_id: exclude.or_else(|| input.get("excludeId").and_then(Value::as_str)),
        shift_held: input["shiftHeld"].as_bool().expect("shiftHeld"),
        surface,
        lookup_sockets: lookup,
    }
}

fn expect_score(actual: f64, expected: &Value, label: &str) {
    match expected.as_str() {
        Some("Infinity") => assert!(actual.is_infinite(), "{label}: got {actual}, want Infinity"),
        Some(other) => panic!("{label}: unexpected score encoding {other}"),
        None => {
            assert!(actual.is_finite(), "{label}: got {actual}, want finite");
            close(actual, expected.as_f64().expect("number"), SNAP_TOL, label);
        }
    }
}

#[test]
fn snap_golden_vectors() {
    let lookup = fixtures();
    let cases = golden("stage-snap.json");
    assert_eq!(cases.len(), 59, "case count changed");

    for case in &cases {
        let name = case["case"].as_str().expect("case name");
        let input = &case["input"];
        let output = &case["output"];

        let pieces = pieces(&input["pieces"]);
        let surface = input.get("surface").map(surface);
        let current_quaternion = input.get("currentQuaternion").map(|q| {
            let n = nums(q);
            DQuat::from_xyzw(n[0], n[1], n[2], n[3])
        });
        let build = |exclude| {
            build_input(
                input,
                &pieces,
                surface.as_ref(),
                current_quaternion,
                &lookup,
                exclude,
            )
        };

        let result = solve_snap(&build(None));

        assert_eq!(
            result.parent_id.as_deref(),
            output["parentId"].as_str(),
            "{name}: parentId"
        );
        match (&result.matched, output["match"].as_object()) {
            (None, None) => {}
            (Some(m), Some(want)) => {
                assert_eq!(
                    m.held_socket,
                    want["heldSocket"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    m.host_socket,
                    want["hostSocket"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    m.host_id.as_deref(),
                    want["hostId"].as_str(),
                    "{name}: hostId"
                );
                assert_eq!(
                    m.host_type.as_str(),
                    want["hostType"].as_str().unwrap(),
                    "{name}: hostType"
                );
            }
            (got, want) => panic!("{name}: match mismatch — got {got:?}, want {want:?}"),
        }
        expect_score(result.score, &output["score"], &format!("{name}: score"));

        // The pose is compared as a composed world matrix — see the module
        // docs on quaternion double cover.
        let world =
            DMat4::from_scale_rotation_translation(DVec3::ONE, result.quaternion, result.position)
                .to_cols_array();
        let want = nums(&output["worldMatrix"]);
        for i in 0..16 {
            close(world[i], want[i], SNAP_TOL, &format!("{name}: world[{i}]"));
        }

        // Runner-up probe: the same input with the winning host excluded, so a
        // port that resolves an exact score tie the other way is caught even
        // when the winner still matches. (Only pass 1 honours `exclude_id` —
        // the surface fallback does not, which is why some surface cases
        // report the same parent again.)
        let Some(want_runner) = output["runnerUp"].as_object() else {
            assert!(
                input.get("excludeId").is_some() || result.parent_id.is_none(),
                "{name}: runnerUp recorded as null but the case has a host to exclude"
            );
            continue;
        };
        let probe = solve_snap(&build(result.parent_id.as_deref()));
        assert_eq!(
            probe.parent_id.as_deref(),
            want_runner["parentId"].as_str(),
            "{name}: runnerUp.parentId"
        );
        assert_eq!(
            probe.matched.as_ref().map(|m| m.host_socket.as_str()),
            want_runner["hostSocket"].as_str(),
            "{name}: runnerUp.hostSocket"
        );
        expect_score(
            probe.score,
            &want_runner["score"],
            &format!("{name}: runnerUp.score"),
        );
    }
}
