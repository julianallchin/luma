//! Microbench for the eval IR realtime decode path. Hand-builds a representative
//! single-annotation graph (spatial NormalizedIndex + temporal Sine + a few
//! elementwise ops over the n axis) and measures eval() at t=1 across n.
//!
//!   cargo run --release --bin bench_eval
//!
//! eval() includes assemble() (building the per-frame UniverseState), so these
//! are end-to-end realtime-frame costs, not just op compute.

use luma_lib::eval::ops::math::{BinOp, MathOp, UnaryOp};
use luma_lib::eval::ops::signals::SignalOp;
use luma_lib::eval::ops::spatial::SpatialOp;
use luma_lib::eval::{
    eval, Arena, Op, OpKind, OutputBinding, Phase, Plan, ResidentContext, SlotSpec,
};
use std::time::Instant;

/// 6-op representative annotation: index -> *0.5, + sin(t), abs -> dimmer.
fn make_plan(n: u32) -> Plan {
    let slots = vec![
        SlotSpec { n, c: 1 },    // 0 NormalizedIndex (per-primitive)
        SlotSpec { n: 1, c: 1 }, // 1 Scalar
        SlotSpec { n: 1, c: 1 }, // 2 Sine (temporal, global)
        SlotSpec { n, c: 1 },    // 3 index * scalar
        SlotSpec { n, c: 1 },    // 4 + sine
        SlotSpec { n, c: 1 },    // 5 abs -> dimmer
    ];
    let ops = vec![
        Op {
            kind: OpKind::Spatial(SpatialOp::NormalizedIndex),
            inputs: vec![],
            out: 0,
            phase: Phase::Prologue,
        },
        Op {
            kind: OpKind::Math(MathOp::Scalar(0.5)),
            inputs: vec![],
            out: 1,
            phase: Phase::Prologue,
        },
        Op {
            kind: OpKind::Signal(SignalOp::Sine { freq: 0.5 }),
            inputs: vec![],
            out: 2,
            phase: Phase::Kernel,
        },
        Op {
            kind: OpKind::Math(MathOp::Binary(BinOp::Mul)),
            inputs: vec![0, 1],
            out: 3,
            phase: Phase::Kernel,
        },
        Op {
            kind: OpKind::Math(MathOp::Binary(BinOp::Add)),
            inputs: vec![3, 2],
            out: 4,
            phase: Phase::Kernel,
        },
        Op {
            kind: OpKind::Math(MathOp::Unary(UnaryOp::Abs)),
            inputs: vec![4],
            out: 5,
            phase: Phase::Kernel,
        },
    ];
    Plan {
        ops,
        slots,
        n,
        primitive_ids: (0..n).map(|i| format!("f{i}:0")).collect(),
        outputs: OutputBinding {
            dimmer: Some(5),
            ..Default::default()
        },
        ctx: ResidentContext::default(),
        prologue_baked: Vec::new(),
    }
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
    println!("eval IR realtime decode — one representative 6-op annotation graph at t=1");
    println!("(eval includes assemble: per-frame UniverseState HashMap build)\n");
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
