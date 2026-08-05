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
- `vse-3d` for controlled scientific 3D and `vse-bevy` for Bevy-rendered scenes, both connected through the external-frame seam

## Timing model

VSE records the strongest timing evidence available for each frame. The preferred displayed backend uses `VK_EXT_present_timing`; individual receipts distinguish scanout-domain timestamps from CPU estimates. Headless receipts use synthesized `Offscreen` timing.

Presentation targets are requests rather than proof of execution. Synchronous and buffered runtimes also use different pacing strategies. Display path, observed feedback, and explicit hardware characterization must accompany timing data. Scanout begin remains distinct from photon onset, which requires physical measurement.

Read the normative [timing-conformance contract](docs/timing-conformance.md) before interpreting `FlipInfo` or recorded timing columns.

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
cargo run --example 00_hello_flip
cargo run --example 01_primitives_gallery
cargo run --example 03_gabor_field
```

Some examples depend on display configuration, fullscreen behavior, or Linux-specific direct-display/input access. Expect those paths to require more machine-specific setup than the basic windowed examples.

## Guides

- [Timing conformance](docs/timing-conformance.md)
- [API imports and Audit 8 migration](docs/guides/api-surface.md)
- [Choosing a runtime](docs/guides/runtimes.md)
- [Buffered flips](docs/guides/buffered_flips.md)
- [Headless rendering](docs/guides/headless.md)
- [Data recording](docs/guides/data_recording.md)

## License

MIT
