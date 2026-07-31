//! Random-dot kinematogram (RDK) with adjustable motion coherence.
//!
//! A random-dot motion stimulus is a workhorse of visual and decision
//! neuroscience. Psychtoolbox has no single RDK demo (it is usually built from
//! `DotDemo` plus custom coherence logic); this consolidates that into one
//! demo built on VSE's instanced `draw_dots` primitive.
//!
//! Each frame, a fraction of the dots (the *coherence*) steps in a common
//! signal direction; the rest step in independent random directions. Dots that
//! leave the circular aperture respawn at a random position, keeping density uniform.
//!
//! Controls:
//!   * Up / Down    — coherence ± 5%
//!   * Left / Right — rotate signal direction ± 15°
//!   * Space        — toggle pause
//!   * Escape       — exit
//!
//! The current coherence and direction are printed to stdout when they change.
//! The dot walk is seeded, so a given key sequence reproduces exactly.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 16_rdk_motion
//! ```

use std::f32::consts::TAU;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use vision_stimulus_engine::prelude::*;

struct Rdk {
    center: (f32, f32),
    radius: f32,
    speed: f32,
    dots: Vec<(f32, f32)>,
    rng: ChaCha8Rng,
}

impl Rdk {
    fn new(center: (f32, f32), radius: f32, n: usize, speed: f32) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(0x0D07_5EED);
        let dots = (0..n)
            .map(|_| random_in_disc(&mut rng, center, radius))
            .collect();
        Self {
            center,
            radius,
            speed,
            dots,
            rng,
        }
    }

    fn step(&mut self, coherence: f32, direction: f32) {
        let (sdx, sdy) = (direction.cos() * self.speed, direction.sin() * self.speed);
        // Collect into a temp to avoid borrowing self.rng and self.dots at once.
        for i in 0..self.dots.len() {
            let signal = self.rng.gen::<f32>() < coherence;
            let (dx, dy) = if signal {
                (sdx, sdy)
            } else {
                let a = self.rng.gen_range(0.0..TAU);
                (a.cos() * self.speed, a.sin() * self.speed)
            };
            let (mut x, mut y) = self.dots[i];
            x += dx;
            y += dy;
            // Respawn dots that leave the circular aperture.
            let ddx = x - self.center.0;
            let ddy = y - self.center.1;
            if ddx * ddx + ddy * ddy > self.radius * self.radius {
                let (nx, ny) = random_in_disc(&mut self.rng, self.center, self.radius);
                x = nx;
                y = ny;
            }
            self.dots[i] = (x, y);
        }
    }
}

fn random_in_disc(rng: &mut ChaCha8Rng, center: (f32, f32), radius: f32) -> (f32, f32) {
    let a = rng.gen_range(0.0..TAU);
    let r = radius * rng.gen::<f32>().sqrt();
    (center.0 + r * a.cos(), center.1 + r * a.sin())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    let context = VSEContext::builder()
        .with_window_size(800, 800)
        .with_title("VSE - Random Dot Kinematogram")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut rdk = Rdk::new((400.0, 400.0), 300.0, 300, 3.0);
    let mut coherence: f32 = 0.5;
    let mut direction: f32 = 0.0; // radians, 0 = rightward
    let mut paused = false;

    println!(
        "RDK: coherence {:.0}%, direction {:.0}deg",
        coherence * 100.0,
        direction.to_degrees()
    );

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let mut changed = false;
        if vse.key_just_pressed(KeyCode::ArrowUp) {
            coherence = (coherence + 0.05).min(1.0);
            changed = true;
        }
        if vse.key_just_pressed(KeyCode::ArrowDown) {
            coherence = (coherence - 0.05).max(0.0);
            changed = true;
        }
        if vse.key_just_pressed(KeyCode::ArrowLeft) {
            direction -= 15.0_f32.to_radians();
            changed = true;
        }
        if vse.key_just_pressed(KeyCode::ArrowRight) {
            direction += 15.0_f32.to_radians();
            changed = true;
        }
        if vse.key_just_pressed(KeyCode::Space) {
            paused = !paused;
        }
        if changed {
            println!(
                "RDK: coherence {:.0}%, direction {:.0}deg",
                coherence * 100.0,
                direction.to_degrees()
            );
        }

        if !paused {
            rdk.step(coherence, direction);
        }

        // Aperture outline + dots.
        vse.draw_arc(
            rdk.center.0,
            rdk.center.1,
            rdk.radius,
            0.0,
            TAU,
            2.0,
            Color::grey(0.35),
        );
        vse.draw_dots(&rdk.dots, 3.0, Color::WHITE);

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
