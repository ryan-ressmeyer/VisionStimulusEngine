# VisionStimulusEngine

VisionStimulusEngine is a work-in-progress stimulus presentation engine for visual neuroscience experiments. It is written in Rust and built around Vulkan because the project needs direct access to display timing, presentation control, and graphics hardware behavior.

This is a personal research project by Ryan Ressmeyer. The goal is to build a stimulus system that connects visual stimuli to neural responses, especially where millisecond-scale timing and frame-by-frame reproducibility matter. The project draws on the practical history of Psychtoolbox and PsychoPy, but it is not trying to be a drop-in replacement for either one.

The repository is early and unstable. APIs will change, examples may move, and some pieces are present as scaffolding for later timing work. Treat the code as an active prototype rather than a finished library.

## Intended use case

The target use case is vision science, especially experiments where the exact timing and content of each frame need to be known after the fact. Examples include electrophysiology, calcium imaging, psychophysics, and model-comparison work where stimulus reconstruction must be precise enough to align with neural data.

The long-term aim is to support both high-level experiment code and low-level access for users who need to inspect or control the graphics pipeline. A beginner should be able to draw calibrated stimuli without learning Vulkan first. An advanced user should still be able to reach the timing and rendering details when an experiment demands it.

## Current status

The project currently includes:

- a Rust crate using `vulkano`, `winit`, and `ash`
- basic rendering and drawing abstractions
- example programs for clear colors, timing validation, calibration squares, Gabors, scheduled flips, image scaling, and fullscreen/direct-display work
- host and session logging utilities
- timing infrastructure built around scanout-clock presentation, with CPU estimates as a loud fallback
- experimental Bevy/external-frame integration crates for 3D-rendered stimuli

## Timing model

Precise visual stimulus timing is the core reason this project exists. For many neuroscience experiments, the important timestamp is not when the CPU submitted a frame or when the GPU finished rendering it. The important timestamp is when display scanout began, because that is the best available proxy for when photons from that frame could reach the eye.

VSE keeps display timing in the scanout-clock domain. The preferred backend is `VK_EXT_present_timing` with `VK_KHR_present_id2` correlation and `VK_KHR_present_wait2` pacing. `VK_GOOGLE_display_timing` was the earlier Linux/Android path and has been removed from VSE. CPU timestamps remain as a fallback for development and for drivers that do not provide usable present-timing behavior.

Driver support is checked behaviorally, not trusted from extension strings alone. On the current Intel ANV/Mesa reference stack, present-id and present-wait work, but past-presentation scanout timestamps are zero and absolute scheduled presentation is not enforced. VSE records those facts in host/session metadata, warns when it falls back, samples the calibrated scanout clock after present-wait when hardware feedback is missing, and uses software pacing when target-time scheduling is not enforced.

The project reports timing fallbacks explicitly. A run that only has CPU estimates should not look equivalent to a run with scanout-domain timing.

## Design direction

VSE is being shaped around a few constraints from visual neuroscience:

- frame timing should be measurable, not assumed
- stimulus generation should be reproducible from saved parameters and seeds
- calibration metadata should travel with experimental data
- high-level APIs should not hide timing failures
- low-level Vulkan access should remain available when needed

The project is forward-looking. Some code exists to support current experiments and examples; other parts are placeholders for a more complete timing and calibration stack.

## Running examples

Once Rust and Vulkan drivers are available on the host machine, examples can be run with Cargo:

```bash
cargo run --example 00_clear_color
cargo run --example 01_timing_validation
cargo run --example 03_gabor_demo
```

Some examples depend on display configuration, fullscreen behavior, or Linux-specific direct-display/input access. Expect those paths to require more machine-specific setup than the basic windowed examples.

## License

MIT
