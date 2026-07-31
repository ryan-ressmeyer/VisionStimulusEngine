//! Adaptive staircase driving a 2AFC contrast-detection experiment.
//!
//! Consolidates PsychDemos `MinExpEntStairDemo` and the example-experiment
//! structure (`PsychExampleExperiments`) into one self-contained task, and
//! generalizes the bespoke experiment in `18_metacontrast_masking`.
//!
//! Task: on each trial a low-contrast grating patch appears in one of two
//! apertures (left or right, chosen at random); the other aperture is blank.
//! The observer reports the side with the grating:
//!   * **F** — left,  **J** — right,  **Escape** — quit.
//!
//! A 2-down-1-up transformed staircase adjusts the grating contrast (two
//! correct in a row → harder, one wrong → easier), converging on the ~70.7%
//! detection threshold. The step size halves at the first few reversals. Every
//! trial is logged to `staircase_2afc/` via the session writer, and the
//! threshold estimate (mean of the last reversals) is printed at the end.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 17_staircase_2afc
//! ```

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;
use vision_stimulus_engine::prelude::*;

/// A transformed up/down staircase on a bounded level (here: log-spaced
/// contrast). `n_down` corrects in a row step down (harder); one wrong steps up.
struct Staircase {
    level: f32,    // current contrast in (0, 1]
    step: f32,     // multiplicative step factor (>1)
    min_step: f32, // step floor after halving at reversals
    n_down: u32,
    consecutive_correct: u32,
    last_dir: i32, // -1 down, +1 up, 0 none
    reversals: Vec<f32>,
    min_level: f32,
    max_level: f32,
}

impl Staircase {
    fn new(start: f32) -> Self {
        Self {
            level: start,
            step: 1.4,
            min_step: 1.1,
            n_down: 2,
            consecutive_correct: 0,
            last_dir: 0,
            reversals: Vec::new(),
            min_level: 0.005,
            max_level: 1.0,
        }
    }

    /// Update after a trial; returns the direction stepped (-1/0/+1).
    fn update(&mut self, correct: bool) -> i32 {
        let dir = if correct {
            self.consecutive_correct += 1;
            if self.consecutive_correct >= self.n_down {
                self.consecutive_correct = 0;
                -1 // step down: lower contrast, harder
            } else {
                0
            }
        } else {
            self.consecutive_correct = 0;
            1 // step up: raise contrast, easier
        };

        if dir != 0 {
            // A reversal: direction changed sign vs. the previous non-zero step.
            if self.last_dir != 0 && dir != self.last_dir {
                self.reversals.push(self.level);
                // Narrow the step toward min_step at each reversal.
                self.step = (self.step * 0.85).max(self.min_step);
            }
            self.last_dir = dir;
            let factor = if dir < 0 { 1.0 / self.step } else { self.step };
            self.level = (self.level * factor).clamp(self.min_level, self.max_level);
        }
        dir
    }

    /// Threshold estimate: geometric mean of the last `k` reversal levels.
    fn threshold(&self, k: usize) -> Option<f32> {
        if self.reversals.len() < 2 {
            return None;
        }
        let tail = &self.reversals[self
            .reversals
            .len()
            .min(self.reversals.len().saturating_sub(k))..];
        let logsum: f32 = tail.iter().map(|v| v.ln()).sum();
        Some((logsum / tail.len() as f32).exp())
    }
}

struct ResponseOutcome {
    presented_contrast: f32,
    reversal_count: usize,
}

fn apply_response(staircase: &mut Staircase, correct: bool) -> ResponseOutcome {
    let presented_contrast = staircase.level;
    staircase.update(correct);
    ResponseOutcome {
        presented_contrast,
        reversal_count: staircase.reversals.len(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Fixation,
    Stimulus,
    Response,
}

#[derive(Serialize)]
struct TrialRecord {
    trial: u32,
    contrast: f32,
    target_left: bool,
    response_left: bool,
    correct: bool,
    reversal_count: usize,
}

const MAX_REVERSALS: usize = 12;
const FIX_FRAMES: u64 = 30;
const STIM_FRAMES: u64 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("2AFC contrast detection. Report the side with the grating:");
    println!("  F = left,  J = right,  Escape = quit\n");

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("staircase_2afc/"))
        .build()?;

    let context = VSEContext::builder()
        .with_window_size(1000, 600)
        .with_title("VSE - 2AFC Staircase")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_session(session)
        .build()?;

    let mut rng = ChaCha8Rng::seed_from_u64(0x5741_2C21);
    let mut staircase = Staircase::new(0.5);
    let mut phase = Phase::Fixation;
    let mut phase_frame: u64 = 0;
    let mut trial: u32 = 0;
    let mut target_left = rng.gen::<bool>();
    let mut finished = false;

    // Aperture geometry: two discs, left and right of center.
    let left_c = (300.0_f32, 300.0_f32);
    let right_c = (700.0_f32, 300.0_f32);
    let ap_r = 130.0_f32;
    let patch = 180.0_f32; // grating patch side length

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        // Draw the two aperture outlines and a central fixation cross always.
        for c in [left_c, right_c] {
            vse.draw_arc(
                c.0,
                c.1,
                ap_r,
                0.0,
                std::f32::consts::TAU,
                2.0,
                Color::grey(0.35),
            );
        }
        vse.draw_line(490.0, 300.0, 510.0, 300.0, 2.0, Color::BLACK);
        vse.draw_line(500.0, 290.0, 500.0, 310.0, 2.0, Color::BLACK);

        if !finished {
            match phase {
                Phase::Fixation => {
                    if phase_frame >= FIX_FRAMES {
                        phase = Phase::Stimulus;
                        phase_frame = 0;
                    }
                }
                Phase::Stimulus => {
                    // Draw the target grating in one aperture.
                    let c = if target_left { left_c } else { right_c };
                    let params = GratingParams {
                        frequency: 0.05,
                        orientation: 0.0,
                        phase: 0.0,
                        contrast: staircase.level,
                        background: 0.5,
                        wave: WaveType::Sine,
                    };
                    vse.draw_grating(
                        c.0 - patch / 2.0,
                        c.1 - patch / 2.0,
                        c.0 + patch / 2.0,
                        c.1 + patch / 2.0,
                        &params,
                    );
                    if phase_frame >= STIM_FRAMES {
                        phase = Phase::Response;
                        phase_frame = 0;
                    }
                }
                Phase::Response => {
                    let response_left = if vse.key_just_pressed(KeyCode::KeyF) {
                        Some(true)
                    } else if vse.key_just_pressed(KeyCode::KeyJ) {
                        Some(false)
                    } else {
                        None
                    };

                    if let Some(resp_left) = response_left {
                        let correct = resp_left == target_left;
                        let outcome = apply_response(&mut staircase, correct);

                        let rec = TrialRecord {
                            trial,
                            contrast: outcome.presented_contrast,
                            target_left,
                            response_left: resp_left,
                            correct,
                            reversal_count: outcome.reversal_count,
                        };
                        vse.record_annotation("trial", &rec)?;

                        println!(
                            "trial {:>3}  contrast {:.4}  {}  reversals {}",
                            trial,
                            rec.contrast,
                            if correct { "correct" } else { "wrong  " },
                            staircase.reversals.len()
                        );

                        trial += 1;
                        target_left = rng.gen::<bool>();
                        phase = Phase::Fixation;
                        phase_frame = 0;

                        if staircase.reversals.len() >= MAX_REVERSALS {
                            finished = true;
                            let thr = staircase.threshold(6).unwrap_or(staircase.level);
                            println!(
                                "\nDone: {} trials, {} reversals.",
                                trial,
                                staircase.reversals.len()
                            );
                            println!("Threshold estimate (last 6 reversals): contrast {:.4}", thr);
                            println!("Press Escape to exit.");
                        }
                    }
                }
            }
        }

        vse.clear()?;
        vse.flip(None)?;
        phase_frame += 1;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_outcome_keeps_the_contrast_that_was_presented() {
        let mut staircase = Staircase::new(0.5);
        let outcome = apply_response(&mut staircase, false);

        assert_eq!(outcome.presented_contrast, 0.5);
        assert!(staircase.level > outcome.presented_contrast);
    }
}
