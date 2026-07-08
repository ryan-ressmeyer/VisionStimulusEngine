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
- early timing infrastructure with CPU estimates and Vulkan display-timing support

## Timing roadmap

Precise visual stimulus timing is the core reason this project exists. For many neuroscience experiments, the important timestamp is not when the CPU submitted a frame or when the GPU finished rendering it. The important timestamp is when display scanout began, because that is the best available proxy for when photons from that frame could reach the eye.

The roadmap is to support a tiered timing system:

1. Use CPU timestamps as a development fallback.
2. Use `VK_GOOGLE_display_timing` where available to query refresh cycles, retrieve past presentation timing, and schedule presentation times.
3. Move toward `VK_EXT_present_timing` as driver support and Rust bindings mature.

`VK_EXT_present_timing` is especially relevant because it is designed to expose presentation timing and scheduling in a more standard, cross-platform way. If the extension becomes broadly available through Mesa, NVIDIA drivers, `ash`, and `vulkano`, VSE should be able to rely less on CPU-side waiting and more on hardware-supported frame scheduling and timestamp feedback.

The project will report timing fallbacks explicitly. A run that only has CPU estimates should not look equivalent to a run with hardware scanout timestamps.

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
