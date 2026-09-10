//! Measure the real canonical playback path, including fixture output assembly.
use luma_lib::eval::{compile::compile_pattern, eval, Arena, Plan, ResidentContext};
use std::time::Instant;
fn make_plan(n: u32) -> Plan {
    let graph = serde_json::from_value(serde_json::json!({
        "nodes":[
            {"id":"index","typeId":"get_attribute","params":{"attribute":"normalized_index"}},
            {"id":"scale","typeId":"scalar","params":{"value":0.5}},
            {"id":"sine","typeId":"sine_wave","params":{"frequency":0.5}},
            {"id":"mul","typeId":"math","params":{"operation":"multiply"}},
            {"id":"add","typeId":"math","params":{"operation":"add"}},
            {"id":"out","typeId":"apply_dimmer","params":{}}
        ],"edges":[
            {"id":"1","fromNode":"index","fromPort":"out","toNode":"mul","toPort":"a"},
            {"id":"2","fromNode":"scale","fromPort":"out","toNode":"mul","toPort":"b"},
            {"id":"3","fromNode":"mul","fromPort":"out","toNode":"add","toPort":"a"},
            {"id":"4","fromNode":"sine","fromPort":"out","toNode":"add","toPort":"b"},
            {"id":"5","fromNode":"add","fromPort":"out","toNode":"out","toPort":"signal"}
        ],"args":[]
    }))
    .unwrap();
    compile_pattern(
        &graph,
        &Default::default(),
        ResidentContext {
            positions: (0..n).map(|i| [i as f32, 0., 0.]).collect(),
            span: (0., 60.),
            ..Default::default()
        },
        (0..n).map(|i| format!("f{i}:0")).collect(),
    )
    .unwrap()
}
fn bench(n: u32, iters: u32) -> f64 {
    let plan = make_plan(n);
    let mut arena = Arena::default();
    let times = [12.34_f32];
    for _ in 0..500 {
        std::hint::black_box(eval(&plan, &times, &mut arena));
    }
    let start = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(eval(&plan, &times, &mut arena));
    }
    start.elapsed().as_secs_f64() / iters as f64
}

fn main() {
    println!("Canonical tensor realtime decode — spatial and temporal composition at t=1");
    println!("(Includes fixture output: per-frame UniverseState HashMap build)\n");
    println!(
        "{:>9} {:>12} {:>14} {:>16}",
        "n", "us/frame", "FPS", "x 44Hz budget"
    );
    for &n in &[16u32, 46, 256, 1024, 4096, 16384, 100_000] {
        let iters = (40_000_000u64 / (n as u64).max(1)).clamp(300, 200_000) as u32;
        let spf = bench(n, iters);
        let us = spf * 1e6;
        let fps = 1.0 / spf;
        let budget = (1.0 / 44.0) / spf; // headroom over the 22.7ms DMX frame
        println!("{:>9} {:>12.3} {:>14.0} {:>16.0}", n, us, fps, budget);
    }
}
