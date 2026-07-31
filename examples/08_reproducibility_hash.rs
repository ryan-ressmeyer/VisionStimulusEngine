//! Reproducibility — prove that stimuli are identical across runs.
//!
//! Full reproducibility is a core VSE design goal (and has no Psychtoolbox
//! demo equivalent). This example generates a deterministic sequence of stimuli
//! — Gabor patches at stepped orientations plus seeded white-noise frames —
//! hashes each generated pixel buffer (FNV-1a, 64-bit), and prints the hashes.
//!
//! Run it twice and compare the printed hashes: they are byte-for-byte
//! identical, because every stimulus is a pure function of its parameters and
//! seed. The same guarantee is what lets an experiment be reconstructed frame
//! by frame for image-computable modeling.
//!
//! The generated Gabors are also displayed so you can see what was hashed.
//! Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! # Identical output across runs:
//! cargo run --release --example 08_reproducibility_hash | tee run1.txt
//! cargo run --release --example 08_reproducibility_hash | tee run2.txt
//! diff run1.txt run2.txt   # (ignoring the per-run timing lines)
//! ```

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use vision_stimulus_engine::prelude::*;

/// FNV-1a 64-bit hash of a byte buffer.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Deterministic seeded white noise (RGBA) — a pure function of `seed`.
fn white_noise(seed: u64, size: u32) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut data = vec![0u8; (size * size * 4) as usize];
    for px in data.chunks_exact_mut(4) {
        let v: u8 = rng.gen();
        px[0] = v;
        px[1] = v;
        px[2] = v;
        px[3] = 255;
    }
    data
}

const N_GABORS: usize = 6;

fn gabor_params(i: usize) -> GaborParams {
    GaborParams {
        size: 160,
        frequency: 0.05,
        orientation: i as f32 * std::f32::consts::PI / N_GABORS as f32,
        phase: 0.0,
        sigma: 30.0,
        aspect_ratio: 1.0,
        contrast: 1.0,
        background: 0.5,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    // --- Compute and report hashes (the reproducibility evidence). ---
    println!("Reproducibility hashes (identical on every run):");
    println!("  {:<18} {:>18}", "stimulus", "fnv1a-64");
    for i in 0..N_GABORS {
        let pixels = gabor_params(i).generate();
        println!(
            "  gabor[{i}] ori={:>4.0}deg   {:016x}",
            gabor_params(i).orientation.to_degrees(),
            fnv1a(&pixels)
        );
    }
    for seed in 0..4u64 {
        let pixels = white_noise(seed, 128);
        println!("  noise[seed={seed}]        {:016x}", fnv1a(&pixels));
    }
    println!();
    println!("Displaying the hashed Gabors. Press Escape to exit.");

    let context = VSEContext::builder()
        .with_window_size(1040, 260)
        .with_title("VSE - Reproducibility")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // Upload the Gabor textures once.
    let mut handles: Vec<TextureHandle> = Vec::new();

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        if handles.is_empty() {
            for i in 0..N_GABORS {
                handles.push(vse.create_gabor(&gabor_params(i))?);
            }
        }

        for (i, h) in handles.iter().enumerate() {
            let x = 20.0 + i as f32 * 170.0;
            let y = 50.0;
            vse.draw_texture(*h, x, y, x + 160.0, y + 160.0);
        }

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
