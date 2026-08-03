//! Record-path microbenchmarks.
//!
//! These measure the CPU cost of turning queued draw commands into a command
//! buffer — the path where the pooled vertex arena replaced a per-draw
//! `Buffer::from_iter`. That change was argued on allocation counts and never
//! measured; these benchmarks are the measurement.
//!
//! # Reading the numbers
//!
//! Each iteration is one full headless flip: record, submit, fence-wait, and
//! copy the image back. Submit and readback are real costs but roughly constant
//! at the 64×64 extent used here, so **the meaningful signal is how the time
//! scales with draw count**, not the absolute value. Per-draw buffer allocation
//! would show up as a steepening slope; suballocation from the arena should
//! grow far slower than the draw count.
//!
//! `record/empty_flip` anchors the fixed cost, but treat it as an
//! order-of-magnitude reference rather than a precise subtrahend: the
//! submit/fence path is noisy enough that the floor can measure *above* the
//! one-draw case.
//!
//! Measured on Intel MTL / ANV, `cargo bench -- --quick`: 1 → 265 µs,
//! 64 → 358 µs, 512 → 540 µs, 2048 → 588 µs. Quadrupling the draw count from
//! 512 to 2048 costs under 10% more time, which is the arena behaving as
//! claimed — a per-draw `Buffer::from_iter` could not scale that way. Absolute
//! numbers are not comparable across machines, drivers, or GPUs.
//!
//! ```bash
//! cargo bench --bench frame_timing
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use vision_stimulus_engine::core::HeadlessContext;
use vision_stimulus_engine::prelude::*;

/// Small on purpose: the fixed submit/readback cost per iteration grows with
/// the frame area, and it is the draw-count slope we want to see.
const EXTENT: u32 = 64;

fn headless(suite: PipelineSuite) -> HeadlessContext {
    VSEContext::builder()
        .with_headless(EXTENT, EXTENT)
        .with_pipelines(suite)
        .build_headless()
        .expect("benchmarks need a Vulkan device (no display required)")
}

/// One flip that queues `count` flat-colour rects.
///
/// Consecutive flats coalesce into a single vertex upload, so this measures the
/// cost of filling the arena rather than of many separate allocations.
fn bench_flat_rects(c: &mut Criterion) {
    let mut ctx = headless(PipelineSuite::minimal());
    let mut group = c.benchmark_group("record/flat_rects");

    for count in [1usize, 64, 512, 2048] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                ctx.run_headless(
                    |_frame| Ok(()),
                    |vse| {
                        for i in 0..count {
                            let x = (i % 60) as f32;
                            let y = ((i / 60) % 60) as f32;
                            vse.draw_rect(x, y, x + 3.0, y + 3.0, Color::WHITE);
                        }
                        vse.flip(None)?;
                        vse.request_exit();
                        Ok(())
                    },
                )
                .expect("headless run")
            })
        });
    }
    group.finish();
}

/// One flip that queues `count` dot *instances* in a single draw.
///
/// The dot path uploads an instance buffer from the same arena, so this is its
/// second consumer.
fn bench_dot_instances(c: &mut Criterion) {
    let mut ctx = headless(PipelineSuite::minimal().with(BuiltinPipeline::Dot));
    let mut group = c.benchmark_group("record/dot_instances");

    for count in [16usize, 256, 4096] {
        let positions: Vec<(f32, f32)> = (0..count)
            .map(|i| ((i % 60) as f32, ((i / 60) % 60) as f32))
            .collect();
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &positions,
            |b, positions| {
                b.iter(|| {
                    ctx.run_headless(
                        |_frame| Ok(()),
                        |vse| {
                            vse.draw_dots(positions, 2.0, Color::WHITE);
                            vse.flip(None)?;
                            vse.request_exit();
                            Ok(())
                        },
                    )
                    .expect("headless run")
                })
            },
        );
    }
    group.finish();
}

/// The floor: a flip with nothing queued. Subtract it from the others to
/// separate the fixed submit/readback cost from the record cost.
fn bench_empty_flip(c: &mut Criterion) {
    let mut ctx = headless(PipelineSuite::minimal());
    c.bench_function("record/empty_flip", |b| {
        b.iter(|| {
            ctx.run_headless(
                |_frame| Ok(()),
                |vse| {
                    vse.flip(None)?;
                    vse.request_exit();
                    Ok(())
                },
            )
            .expect("headless run")
        })
    });
}

criterion_group!(
    benches,
    bench_empty_flip,
    bench_flat_rects,
    bench_dot_instances
);
criterion_main!(benches);
