//! Metacontrast Masking Demo
//!
//! Replicates the Type-B (U-shaped) metacontrast masking paradigm of
//! Trußner, Albrecht & Mattler (2025, *Behavior Research Methods*), itself
//! adapted from Albrecht & Mattler. A briefly-flashed target (a filled
//! **square** or **diamond**, ~1.5°) is presented at fixation, then a larger
//! mask (~2.6°) with a **star-shaped cutout** surrounds it — contours
//! adjacent but non-overlapping — at a variable stimulus-onset asynchrony
//! (SOA). Because the masking phenomenon is *driven by SOA*, this demo
//! exercises VSE's millisecond-accurate flip scheduling, and the recorded
//! scanout onset times validate it.
//!
//! Task: 2AFC — report whether the target was a SQUARE (Left arrow) or a
//! DIAMOND (Right arrow).
//!
//! Timeline per trial (60 Hz, ~16.67 ms/frame): fixation 500 ms → target
//! 1 frame (~17 ms; paper 20 ms) → blank (SOA−1 frames) → mask 7 frames
//! (~117 ms; paper 120 ms) → response. SOA is swept 1–8 frames
//! (~17–133 ms; paper 20–120 ms). A ~1.5 s warmup (discarded frames) precedes
//! trial 1 so the present pipeline and calibrated scanout clock settle before
//! any measured onset — the timing grid's t=0 is anchored after it.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 14_metacontrast_masking
//! ```

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use vision_stimulus_engine::prelude::*;

// ===========================================================================
// Experiment logic (pure, unit-tested)
// ===========================================================================

/// The target's shape — the 2AFC discriminandum. The mask's star-shaped
/// cutout fits both, so the mask carries no shape cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Square,
    Diamond,
}

/// One trial: a target shape and the SOA (in refresh frames) between target
/// onset and mask onset.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Trial {
    soa_frames: u32,
    target_shape: Shape,
}

/// Build a balanced, deterministically-shuffled trial list: every
/// (SOA, shape) pair appears exactly `reps` times, in an order fixed by
/// `seed` (reproducibility is a core VSE design goal).
fn build_trials(soas: &[u32], reps: usize, seed: u64) -> Vec<Trial> {
    let mut trials = Vec::with_capacity(soas.len() * 2 * reps);
    for &soa in soas {
        for _ in 0..reps {
            for target_shape in [Shape::Square, Shape::Diamond] {
                trials.push(Trial {
                    soa_frames: soa,
                    target_shape,
                });
            }
        }
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    trials.shuffle(&mut rng);
    trials
}

/// Aggregate per-trial `(soa_frames, correct)` outcomes into the masking
/// function: proportion correct per SOA, sorted ascending by SOA.
fn masking_curve(results: &[(u32, bool)]) -> Vec<(u32, f64)> {
    use std::collections::BTreeMap;
    // soa -> (n_correct, n_total); BTreeMap keeps SOAs sorted ascending.
    let mut per_soa: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for &(soa, correct) in results {
        let entry = per_soa.entry(soa).or_insert((0, 0));
        entry.1 += 1;
        if correct {
            entry.0 += 1;
        }
    }
    per_soa
        .into_iter()
        .map(|(soa, (n_correct, n_total))| (soa, n_correct as f64 / n_total as f64))
        .collect()
}

// ===========================================================================
// Stimulus geometry & rasterization (pure, unit-tested)
//
// Coordinates are pixel offsets (dx, dy) from the shape centre. A square of
// "half-extent" h spans [-h, h] per axis; a diamond of half-extent h is the
// L1 ball |dx| + |dy| <= h. The mask is an outer square with a star-shaped
// hole (the union of a square cutout and a diamond cutout) punched out.
// ===========================================================================

/// Is offset (dx, dy) inside an axis-aligned square of the given half-extent?
fn square_contains(dx: f32, dy: f32, half: f32) -> bool {
    dx.abs() <= half && dy.abs() <= half
}

/// Is offset (dx, dy) inside a diamond (L1 ball) of the given half-extent?
fn diamond_contains(dx: f32, dy: f32, half: f32) -> bool {
    dx.abs() + dy.abs() <= half
}

/// Is offset (dx, dy) part of the mask — inside the outer square but outside
/// the star-shaped cutout (union of a square and a diamond cutout)?
fn mask_contains(
    dx: f32,
    dy: f32,
    outer_half: f32,
    cut_square_half: f32,
    cut_diamond_half: f32,
) -> bool {
    let inside_outer = square_contains(dx, dy, outer_half);
    let inside_cutout =
        square_contains(dx, dy, cut_square_half) || diamond_contains(dx, dy, cut_diamond_half);
    inside_outer && !inside_cutout
}

/// Rasterize a `size`×`size` RGBA texture: pixels for which `contains` holds
/// get `color`; the rest are fully transparent. `contains` receives pixel
/// offsets from the canvas centre.
fn rasterize(size: u32, color: [u8; 4], contains: impl Fn(f32, f32) -> bool) -> Vec<u8> {
    let centre = (size as f32 - 1.0) / 2.0;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            if contains(x as f32 - centre, y as f32 - centre) {
                let i = ((y * size + x) * 4) as usize;
                buf[i..i + 4].copy_from_slice(&color);
            }
        }
    }
    buf
}

// ===========================================================================
// Experiment parameters (60 Hz reference; realized timing is measured, not
// assumed). Paper values in comments; frame counts are the 60 Hz mapping.
// ===========================================================================

/// SOA sweep in refresh frames: ~17–133 ms at 60 Hz (paper: 20–120 ms).
const SOA_FRAMES: [u32; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
/// Repetitions per (SOA × shape) cell → 8 × 2 × 12 = 192 trials.
const REPS_PER_CELL: usize = 12;
/// Fixed seed → reproducible trial order.
const SEED: u64 = 0x5EED_C0FF_EE00_0001;
/// Fallback refresh if the monitor doesn't report one.
const REFRESH_HZ_FALLBACK: f64 = 60.0;

const WARMUP_FRAMES: u32 = 90; // ~1.5 s of discarded frames before trial 1
const FIX_FRAMES: u32 = 30; // ~500 ms fixation (paper: 500 ms)
const MASK_FRAMES: u32 = 7; // ~117 ms mask (paper: 120 ms)
const ITI_FRAMES: u32 = 36; // ~600 ms inter-trial interval

// Stimulus geometry, in texture pixels (drawn at CANVAS·SCALE on screen).
// Ratios follow the paper: target ~1.5°, mask ~2.6°, tiny contour gap.
const CANVAS: u32 = 128;
// The square and diamond targets share the same *diameter* (max extent): the
// square's corner-to-corner distance equals the diamond's tip-to-tip
// distance, so SQUARE_HALF = DIAMOND_HALF/√2. Their union is then an
// 8-pointed star — square corners poke out diagonally, diamond tips axially —
// which is the shape of the mask's cutout.
const DIAMOND_HALF: f32 = 30.0; // diamond half-extent (L1 tip reach)
const SQUARE_HALF: f32 = DIAMOND_HALF / std::f32::consts::SQRT_2; // ≈ 21.2 half-side
const MASK_OUTER_HALF: f32 = 50.0; // outer square half-side (~2.6° vs target 1.5°)
const CONTOUR_GAP: f32 = 2.0; // gap between target contour and mask cutout
const SCALE: f32 = 3.0; // on-screen magnification of the CANVAS texture

const FIX_ARM: f32 = 9.0; // fixation cross half-length (px)
const FIX_THICK: f32 = 2.0; // fixation cross half-thickness (px)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Discarded frames at session start: warm the present pipeline and let
    /// the calibrated scanout clock converge before any measured onset. The
    /// timing grid's t=0 is anchored to the first frame *after* warmup, not
    /// the cold texture-upload frame.
    Warmup,
    Fixation,
    Target,
    Blank,
    Mask,
    Response,
    Iti,
}

/// One completed trial's record — everything the analysis needs, including
/// the *actual* scanout onset times so realized SOA can be checked against
/// the requested SOA.
struct TrialResult {
    trial: usize,
    soa_frames: u32,
    target_shape: Shape,
    response: Shape,
    correct: bool,
    target_onset_us: u64,
    mask_onset_us: u64,
    realized_soa_us: u64,
    target_on_target: bool,
    mask_on_target: bool,
}

struct ExperimentState {
    trials: Vec<Trial>,
    results: Vec<TrialResult>,
    tex_square: Option<TextureHandle>,
    tex_diamond: Option<TextureHandle>,
    tex_mask: Option<TextureHandle>,
    refresh_us: u64,
    trial_idx: usize,
    phase: Phase,
    phase_frame: u32,
    /// Scanout `t=0`: the present time of the very first flip. Every
    /// subsequent flip is scheduled at `epoch + frame_counter·refresh`.
    epoch_us: Option<u64>,
    frame_counter: u64,
    target_onset: Option<Timestamp>,
    mask_onset: Option<Timestamp>,
    target_on_target: bool,
    mask_on_target: bool,
    started: bool,
}

impl ExperimentState {
    fn new(trials: Vec<Trial>) -> Self {
        Self {
            trials,
            results: Vec::new(),
            tex_square: None,
            tex_diamond: None,
            tex_mask: None,
            refresh_us: (1_000_000.0 / REFRESH_HZ_FALLBACK).round() as u64,
            trial_idx: 0,
            phase: Phase::Warmup,
            phase_frame: 0,
            epoch_us: None,
            frame_counter: 0,
            target_onset: None,
            mask_onset: None,
            target_on_target: true,
            mask_on_target: true,
            started: false,
        }
    }

    /// Rasterize the three stimulus textures and read the true refresh period.
    fn init(&mut self, vse: &mut RenderContext) -> Result<(), VSEError> {
        let white = [255u8, 255, 255, 255];
        let cut_s = SQUARE_HALF + CONTOUR_GAP;
        let cut_d = DIAMOND_HALF + CONTOUR_GAP;
        let square = rasterize(CANVAS, white, |dx, dy| square_contains(dx, dy, SQUARE_HALF));
        let diamond = rasterize(CANVAS, white, |dx, dy| {
            diamond_contains(dx, dy, DIAMOND_HALF)
        });
        let mask = rasterize(CANVAS, white, |dx, dy| {
            mask_contains(dx, dy, MASK_OUTER_HALF, cut_s, cut_d)
        });
        self.tex_square = Some(vse.load_texture_rgba(CANVAS, CANVAS, &square)?);
        self.tex_diamond = Some(vse.load_texture_rgba(CANVAS, CANVAS, &diamond)?);
        self.tex_mask = Some(vse.load_texture_rgba(CANVAS, CANVAS, &mask)?);

        let hz = vse
            .primary_monitor()
            .and_then(|m| m.refresh_rate_hz)
            .unwrap_or(REFRESH_HZ_FALLBACK);
        self.refresh_us = (1_000_000.0 / hz).round() as u64;
        println!(
            "Display: {:.2} Hz (refresh {} µs) · timing source: {}",
            hz,
            self.refresh_us,
            vse.timing_source()
        );
        println!();
        Ok(())
    }

    /// The trial at the current cursor, or `None` once every trial is done.
    fn current_trial(&self) -> Option<Trial> {
        self.trials.get(self.trial_idx).copied()
    }

    fn target_tex(&self, shape: Shape) -> TextureHandle {
        match shape {
            Shape::Square => self.tex_square.unwrap(),
            Shape::Diamond => self.tex_diamond.unwrap(),
        }
    }

    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_frame = 0;
    }

    fn reset_trial(&mut self) {
        self.target_onset = None;
        self.mask_onset = None;
        self.target_on_target = true;
        self.mask_on_target = true;
    }

    fn record(&mut self, trial: Trial, response: Shape) {
        let t = self.target_onset.map(|x| x.as_micros()).unwrap_or(0);
        let m = self.mask_onset.map(|x| x.as_micros()).unwrap_or(0);
        let correct = response == trial.target_shape;
        let realized_soa_us = m.saturating_sub(t);
        println!(
            "  trial {:>3}/{}  SOA {} fr  target={:<7?} resp={:<7?} {}  (realized {:.1} ms)",
            self.results.len() + 1,
            self.trials.len(),
            trial.soa_frames,
            trial.target_shape,
            response,
            if correct { "✓" } else { "✗" },
            realized_soa_us as f64 / 1000.0,
        );
        self.results.push(TrialResult {
            trial: self.trial_idx,
            soa_frames: trial.soa_frames,
            target_shape: trial.target_shape,
            response,
            correct,
            target_onset_us: t,
            mask_onset_us: m,
            realized_soa_us,
            target_on_target: self.target_on_target,
            mask_on_target: self.mask_on_target,
        });
    }

    /// One vsync tick: draw the current phase's frame, schedule the flip one
    /// refresh after the last actual scanout, and advance the state machine.
    fn frame(&mut self, vse: &mut RenderContext) -> Result<(), VSEError> {
        if !self.started {
            self.init(vse)?;
            self.started = true;
        }

        if vse.key_just_pressed(KeyCode::Escape) {
            println!(
                "\nEsc — ending early after {} completed trials.",
                self.results.len()
            );
            self.finalize();
            vse.request_exit();
            return Ok(());
        }

        // Once the cursor is past the last trial the session is over. This
        // also guards the loop against any extra tick after completion.
        let Some(trial) = self.current_trial() else {
            vse.request_exit();
            return Ok(());
        };
        let blank_frames = trial.soa_frames.saturating_sub(1);
        let (w, h) = vse.window_size();

        // Draw this frame: fixation is always present; the target and mask
        // are drawn only during their phases.
        draw_fixation(vse, w, h);
        match self.phase {
            Phase::Target => draw_centered(vse, w, h, self.target_tex(trial.target_shape)),
            Phase::Mask => draw_centered(vse, w, h, self.tex_mask.unwrap()),
            _ => {}
        }
        vse.clear()?;

        // Warmup frames present immediately and do not start the grid. Every
        // other frame is scheduled on the absolute scanout grid t0 + k·T; the
        // first post-warmup flip (no epoch yet) defines t0 from a settled
        // frame, not the cold texture-upload frame.
        let in_warmup = self.phase == Phase::Warmup;
        let target_time = if in_warmup {
            None
        } else {
            self.epoch_us
                .map(|e| Timestamp::from_micros(e + self.frame_counter * self.refresh_us))
        };
        let info = vse.flip(target_time)?;
        if !in_warmup {
            if self.epoch_us.is_none() {
                self.epoch_us = Some(info.present_time.as_micros());
            }
            self.frame_counter += 1;
        }

        // Record the actual scanout onset of the target and mask.
        if self.phase == Phase::Target && self.phase_frame == 0 {
            self.target_onset = Some(info.present_time);
            self.target_on_target = info.on_target;
        }
        if self.phase == Phase::Mask && self.phase_frame == 0 {
            self.mask_onset = Some(info.present_time);
            self.mask_on_target = info.on_target;
        }

        self.phase_frame += 1;

        match self.phase {
            Phase::Warmup => {
                if self.phase_frame >= WARMUP_FRAMES {
                    self.enter(Phase::Fixation);
                }
            }
            Phase::Fixation => {
                if self.phase_frame >= FIX_FRAMES {
                    self.enter(Phase::Target);
                }
            }
            Phase::Target => {
                // Target shows for exactly one frame.
                if blank_frames > 0 {
                    self.enter(Phase::Blank);
                } else {
                    self.enter(Phase::Mask);
                }
            }
            Phase::Blank => {
                if self.phase_frame >= blank_frames {
                    self.enter(Phase::Mask);
                }
            }
            Phase::Mask => {
                if self.phase_frame >= MASK_FRAMES {
                    self.enter(Phase::Response);
                }
            }
            Phase::Response => {
                let response = if vse.key_just_pressed(KeyCode::ArrowLeft) {
                    Some(Shape::Square)
                } else if vse.key_just_pressed(KeyCode::ArrowRight) {
                    Some(Shape::Diamond)
                } else {
                    None
                };
                if let Some(response) = response {
                    self.record(trial, response);
                    self.enter(Phase::Iti);
                }
            }
            Phase::Iti => {
                if self.phase_frame >= ITI_FRAMES {
                    self.trial_idx += 1;
                    if self.trial_idx >= self.trials.len() {
                        println!("\nAll {} trials complete.", self.results.len());
                        self.finalize();
                        vse.request_exit();
                        return Ok(());
                    }
                    self.reset_trial();
                    self.enter(Phase::Fixation);
                }
            }
        }

        Ok(())
    }

    /// Print the masking function and write a per-trial CSV.
    fn finalize(&self) {
        if self.results.is_empty() {
            println!("No completed trials — nothing to write.");
            return;
        }

        let pairs: Vec<(u32, bool)> = self
            .results
            .iter()
            .map(|r| (r.soa_frames, r.correct))
            .collect();
        let curve = masking_curve(&pairs);
        println!("\nMasking function (proportion correct by SOA — expect a U-shaped dip):");
        for (soa, acc) in &curve {
            let ms = *soa as f64 * self.refresh_us as f64 / 1000.0;
            let bar = "█".repeat((acc * 40.0).round() as usize);
            println!("  SOA {soa:>2} fr (~{ms:>5.1} ms)  {acc:.2}  {bar}");
        }

        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = format!("metacontrast_{secs}.csv");
        let mut csv = String::new();
        csv.push_str(
            "trial,soa_frames,requested_soa_ms,target_shape,response,correct,\
             target_onset_us,mask_onset_us,realized_soa_ms,target_on_target,mask_on_target\n",
        );
        for r in &self.results {
            let req_ms = r.soa_frames as f64 * self.refresh_us as f64 / 1000.0;
            let realized_ms = r.realized_soa_us as f64 / 1000.0;
            let _ = writeln!(
                csv,
                "{},{},{:.3},{:?},{:?},{},{},{},{:.3},{},{}",
                r.trial,
                r.soa_frames,
                req_ms,
                r.target_shape,
                r.response,
                r.correct as u8,
                r.target_onset_us,
                r.mask_onset_us,
                realized_ms,
                r.target_on_target as u8,
                r.mask_on_target as u8,
            );
        }
        match std::fs::write(&path, csv) {
            Ok(()) => {
                let shown = std::fs::canonicalize(&path)
                    .map(|p| p.display().to_string())
                    .unwrap_or(path);
                println!("\nPer-trial data written to {shown}");
            }
            Err(e) => println!("\nFailed to write CSV: {e}"),
        }
    }
}

/// Draw a small central fixation cross in stimulus colour.
fn draw_fixation(vse: &mut RenderContext, w: u32, h: u32) {
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    vse.draw_rect(
        cx - FIX_ARM,
        cy - FIX_THICK,
        cx + FIX_ARM,
        cy + FIX_THICK,
        Color::WHITE,
    );
    vse.draw_rect(
        cx - FIX_THICK,
        cy - FIX_ARM,
        cx + FIX_THICK,
        cy + FIX_ARM,
        Color::WHITE,
    );
}

/// Draw a CANVAS texture centred on the display at CANVAS·SCALE pixels.
fn draw_centered(vse: &mut RenderContext, w: u32, h: u32, tex: TextureHandle) {
    let size = CANVAS as f32 * SCALE;
    let left = w as f32 / 2.0 - size / 2.0;
    let top = h as f32 / 2.0 - size / 2.0;
    vse.draw_texture(tex, left, top, left + size, top + size);
}

/// Save a single stimulus texture as a white-on-black PNG for inspection.
fn save_shape(
    path: &str,
    contains: impl Fn(f32, f32) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rgba = rasterize(CANVAS, [255, 255, 255, 255], contains);
    let mut rgb = vec![0u8; (CANVAS * CANVAS * 3) as usize];
    for i in 0..(CANVAS * CANVAS) as usize {
        let v = if rgba[i * 4 + 3] > 0 { 255 } else { 0 };
        rgb[i * 3] = v;
        rgb[i * 3 + 1] = v;
        rgb[i * 3 + 2] = v;
    }
    image::save_buffer(path, &rgb, CANVAS, CANVAS, image::ColorType::Rgb8)?;
    println!("wrote {path}");
    Ok(())
}

/// `--preview`: rasterize the three stimuli to PNGs (no display needed) so the
/// geometry can be checked before running a session.
fn write_preview() -> Result<(), Box<dyn std::error::Error>> {
    let cut_s = SQUARE_HALF + CONTOUR_GAP;
    let cut_d = DIAMOND_HALF + CONTOUR_GAP;
    save_shape("stimulus_square.png", |dx, dy| {
        square_contains(dx, dy, SQUARE_HALF)
    })?;
    save_shape("stimulus_diamond.png", |dx, dy| {
        diamond_contains(dx, dy, DIAMOND_HALF)
    })?;
    save_shape("stimulus_mask.png", |dx, dy| {
        mask_contains(dx, dy, MASK_OUTER_HALF, cut_s, cut_d)
    })?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    if std::env::args().any(|a| a == "--preview") {
        return write_preview();
    }

    let trials = build_trials(&SOA_FRAMES, REPS_PER_CELL, SEED);
    let n_trials = trials.len();

    println!("VSE — Metacontrast Masking Demo");
    println!("================================");
    println!("Replication of Trußner, Albrecht & Mattler (2025), Behav Res Methods.");
    println!();
    println!("Fixate the central cross. Each trial: a target flashes, then a mask.");
    println!("Report the TARGET shape:");
    println!("    [<-]  Left arrow  = SQUARE");
    println!("    [->]  Right arrow = DIAMOND");
    println!("No time pressure. Press Esc to stop early (data so far is saved).");
    println!();
    println!("{n_trials} trials. Give the window focus to begin.");
    println!();

    let context = VSEContext::builder()
        .with_window_size(1000, 750)
        .with_title("VSE - Metacontrast Masking")
        .with_clear_color(0.0, 0.0, 0.0, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut state = ExperimentState::new(trials);
    // The callback calls vse.request_exit() when the session ends; the run
    // loop then returns Ok cleanly.
    context.run(move |vse| state.frame(vse))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_order_is_balanced() {
        let soas = [1u32, 2, 4, 8];
        let reps = 5;
        let trials = build_trials(&soas, reps, 42);

        assert_eq!(trials.len(), soas.len() * 2 * reps);
        for &soa in &soas {
            for shape in [Shape::Square, Shape::Diamond] {
                let n = trials
                    .iter()
                    .filter(|t| t.soa_frames == soa && t.target_shape == shape)
                    .count();
                assert_eq!(
                    n, reps,
                    "expected {reps} trials for soa={soa} shape={shape:?}"
                );
            }
        }
    }

    #[test]
    fn trial_order_is_deterministic_by_seed() {
        let soas = [1u32, 2, 4, 8];
        let a = build_trials(&soas, 5, 42);
        let b = build_trials(&soas, 5, 42);
        assert_eq!(a, b, "same seed must produce identical trial order");

        let c = build_trials(&soas, 5, 43);
        assert_ne!(a, c, "different seed should produce a different order");
    }

    #[test]
    fn current_trial_is_none_once_all_trials_done() {
        // Reproduces the boundary that panicked: a loop tick with the cursor
        // advanced to len() must not index out of bounds.
        let mut st = ExperimentState::new(build_trials(&[1, 2], 1, 7));
        let n = st.trials.len();
        st.trial_idx = n - 1;
        assert!(
            st.current_trial().is_some(),
            "last trial is still available"
        );
        st.trial_idx = n;
        assert!(
            st.current_trial().is_none(),
            "no trial past the end — must return None, not panic"
        );
    }

    #[test]
    fn masking_curve_computes_accuracy_per_soa_sorted() {
        // soa=2 → 1 correct of 2 = 0.5; soa=4 → 2 of 2 = 1.0; output sorted by soa.
        let results = [(4u32, true), (2, true), (4, true), (2, false)];
        let curve = masking_curve(&results);
        assert_eq!(curve, vec![(2, 0.5), (4, 1.0)]);
    }

    #[test]
    fn square_covers_its_bounding_box_including_corners() {
        assert!(square_contains(0.0, 0.0, 10.0), "centre is inside");
        assert!(
            square_contains(10.0, 10.0, 10.0),
            "corner is inside a square"
        );
        assert!(
            !square_contains(11.0, 0.0, 10.0),
            "just outside an edge is out"
        );
    }

    #[test]
    fn diamond_excludes_bounding_box_corners() {
        assert!(diamond_contains(0.0, 0.0, 10.0), "centre is inside");
        assert!(diamond_contains(10.0, 0.0, 10.0), "on-axis tip is inside");
        assert!(
            !diamond_contains(10.0, 10.0, 10.0),
            "|dx|+|dy|=20 > 10 is outside"
        );
        assert!(
            !diamond_contains(6.0, 6.0, 10.0),
            "|dx|+|dy|=12 > 10 is outside"
        );
    }

    #[test]
    fn mask_is_a_frame_with_a_star_hole() {
        let (outer, cs, cd) = (50.0, 32.0, 32.0);
        assert!(
            !mask_contains(0.0, 0.0, outer, cs, cd),
            "centre is in the cutout"
        );
        assert!(
            !mask_contains(30.0, 0.0, outer, cs, cd),
            "inside square cutout"
        );
        assert!(
            mask_contains(40.0, 0.0, outer, cs, cd),
            "in the frame: outer, not cut"
        );
        assert!(
            !mask_contains(60.0, 0.0, outer, cs, cd),
            "beyond the outer edge is out"
        );
    }

    #[test]
    fn rasterize_sets_alpha_by_membership() {
        let size = 21u32;
        let white = [255u8, 255, 255, 255];
        let buf = rasterize(size, white, |dx, dy| diamond_contains(dx, dy, 10.0));

        // centre pixel (10,10) is inside → opaque
        let centre = ((10 * size + 10) * 4) as usize;
        assert_eq!(buf[centre + 3], 255, "centre alpha opaque");
        // bounding-box corner (0,0): dx=dy=-10 → |−10|+|−10|=20 > 10 → transparent
        assert_eq!(buf[3], 0, "corner alpha transparent");
    }
}
