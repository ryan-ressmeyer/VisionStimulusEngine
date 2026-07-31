//! Input handling, mouse trace, and reaction-time capture.
//!
//! Consolidates PsychDemos `KbDemo`, `KbQueueDemo`, and `MouseTraceDemo1`–`3`:
//! keyboard edge detection, live mouse tracking, and a simple reaction-time
//! task.
//!
//! Two things are shown at once:
//!   * **Mouse trace** — the recent mouse path is drawn as a fading dot trail.
//!   * **Reaction time** — press **Space** to arm a trial; after a random delay
//!     a white disc appears; press **Space** again as fast as you can. The RT
//!     (disc onset → keypress) is measured from the clock and printed. Pressing
//!     before the disc appears is flagged as a false start.
//!
//! RTs use the queued key-event timestamp and convert the disc's measured
//! scanout onset into the host clock through the opt-in clock bridge. See
//! `docs/clock-synchronization.md`. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 14_input_and_rt
//! ```

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use vision_stimulus_engine::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Rt {
    Idle,
    Waiting { show_at: Timestamp },
    Go { shown_at_host: Option<Timestamp> },
}

fn reaction_time_ms(onset: Timestamp, response: Timestamp) -> f64 {
    response.as_micros().saturating_sub(onset.as_micros()) as f64 / 1000.0
}

fn key_down_time(vse: &RenderContext, key: KeyCode) -> Option<Timestamp> {
    vse.input_events().iter().find_map(|event| match event {
        InputEvent::KeyDown {
            key_code,
            timestamp,
            repeat: false,
            ..
        } if *key_code == key => Some(*timestamp),
        _ => None,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("Input & RT demo. Move the mouse to draw a trail.");
    println!("Press Space to arm a reaction-time trial; press it again when the disc appears.");
    println!("Escape to exit.\n");

    let context = VSEContext::builder()
        .with_window_size(900, 600)
        .with_title("VSE - Input & Reaction Time")
        .with_clear_color(0.12, 0.12, 0.14, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_host_clock_bridge()
        .build()?;

    let mut trail: Vec<(f32, f32)> = Vec::new();
    const TRAIL_MAX: usize = 90;

    let mut rng = ChaCha8Rng::seed_from_u64(0x2AFC_1234);
    let mut state = Rt::Idle;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let now = vse.clock().now();
        let space_time = key_down_time(vse, KeyCode::Space);
        let space = space_time.is_some();
        let mut onset_frame_pending = false;

        // --- Mouse trail ---
        let (mx, my) = vse.mouse_position();
        let p = (mx as f32, my as f32);
        if trail.last().map_or(true, |&last| {
            let dx = last.0 - p.0;
            let dy = last.1 - p.1;
            dx * dx + dy * dy > 4.0
        }) {
            trail.push(p);
            if trail.len() > TRAIL_MAX {
                trail.remove(0);
            }
        }
        // Draw the trail as dots fading with age (older = smaller).
        for (i, &(x, y)) in trail.iter().enumerate() {
            let age = i as f32 / TRAIL_MAX as f32;
            vse.draw_circle(
                x,
                y,
                1.5 + 4.0 * age,
                Color::rgba(0.4, 0.8, 1.0, 0.2 + 0.8 * age),
            );
        }

        // --- Reaction-time state machine ---
        match state {
            Rt::Idle => {
                if space {
                    let bridge_ready = vse.timing_source() == TimingSource::CpuEstimate
                        || vse.host_clock_bridge_drift_ppm().is_some();
                    if bridge_ready {
                        let delay_ms = rng.gen_range(500..2000);
                        let show_at = Timestamp::from_micros(now.as_micros() + delay_ms * 1000);
                        state = Rt::Waiting { show_at };
                        println!("armed…");
                    } else {
                        println!("clock bridge is warming up; try again in a moment");
                    }
                }
            }
            Rt::Waiting { show_at } => {
                if space {
                    println!("false start! (pressed before the disc)");
                    state = Rt::Idle;
                } else if now.as_micros() >= show_at.as_micros() {
                    state = Rt::Go {
                        shown_at_host: None,
                    };
                    onset_frame_pending = true;
                }
            }
            Rt::Go {
                shown_at_host: Some(shown_at),
            } => {
                if let Some(response_time) = space_time {
                    println!("RT = {:.1} ms", reaction_time_ms(shown_at, response_time));
                    state = Rt::Idle;
                }
            }
            Rt::Go {
                shown_at_host: None,
            } => {}
        }

        if matches!(state, Rt::Go { .. }) {
            vse.draw_circle(450.0, 300.0, 60.0, Color::WHITE);
        }

        // Small crosshair marking the disc location while idle/waiting.
        if !matches!(state, Rt::Go { .. }) {
            vse.draw_line(440.0, 300.0, 460.0, 300.0, 1.0, Color::grey(0.3));
            vse.draw_line(450.0, 290.0, 450.0, 310.0, 1.0, Color::grey(0.3));
        }

        vse.clear()?;
        let flip = vse.flip(None)?;

        if onset_frame_pending {
            let shown_at_host = match flip.timing_source {
                TimingSource::CpuEstimate => Some(flip.present_time),
                TimingSource::ExtPresentTiming => vse.scanout_to_host(
                    ScanoutTimestamp::from_nanos(flip.present_time.as_micros() * 1_000),
                ),
            };
            if let Some(shown_at_host) = shown_at_host {
                state = Rt::Go {
                    shown_at_host: Some(shown_at_host),
                };
            } else {
                eprintln!("could not convert stimulus onset to the host clock; trial cancelled");
                state = Rt::Idle;
            }
        }
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_time_uses_event_and_onset_timestamps() {
        let onset = Timestamp::from_micros(1_000_000);
        let response = Timestamp::from_micros(1_234_500);
        assert_eq!(reaction_time_ms(onset, response), 234.5);
    }
}
