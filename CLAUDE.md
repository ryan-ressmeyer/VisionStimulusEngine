# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

VisionStimulusEngine (VSE) is a vision science stimulus presentation system written in Rust using the Vulkan graphics API. The project aims to provide millisecond-accurate timing precision for visual stimulus presentation while allowing both high-level abstractions for beginners and low-level graphics API access for advanced users.

### Core Design Goals

1. **Incremental Learning Curve**: Provide high-level interfaces for beginners while exposing lower-level Vulkan API calls for advanced users
2. **Millisecond-Accurate Timing**: Imperative for vision science experiments measuring neural responses
3. **Full Reproducibility**: Critical for image-computable models of neural responses
4. **Psychtoolbox API Compatibility**: Where possible, mirror Psychtoolbox API design to ease transition for vision scientists

### Target Users

Vision scientists studying primate visual processing who need:
- Precise stimulus timing for neural recording experiments
- Complex and naturalistic stimuli (videos, virtual reality, real-world scenes)
- Full control over graphics hardware for maximum performance
- Reproducible programmatic stimulus generation

## Project Status

This is an early-stage Rust workspace with the core VSE crate plus experimental integration crates under `crates/`. The repository now contains working Vulkan presentation code, timing infrastructure, example experiments, host/session logging, and documentation. Historical planning notes and bundled reference PDFs have been removed from git.

### Render targets

`VSEState` holds a `RenderTarget` that is either `Present` (window/display,
swapchain, present engine, scanout timing) or `Offscreen` (headless). One
`RenderContext` drives both, so an experiment's render closure runs unchanged in
either — which is what makes a session's stimuli regenerable through the same
code path that displayed them.

Headless needs a GPU but no display, so it is also where pixel-level tests and
record-path benchmarks live (`tests/headless_pixels.rs`,
`benches/frame_timing.rs`). Its flip timing is synthesized, not measured, and is
tagged `TimingSource::Offscreen` so regenerated data can never be mistaken for
recorded data. See `docs/guides/headless.md`.

## Development Commands

### Building and Testing
```bash
# Build project
cargo build

# Run tests
cargo test

# Build with release optimizations
cargo build --release

# Run specific test
cargo test <test_name>

# Check code without building
cargo check
```

### Code Quality
```bash
# Format code
cargo fmt

# Lint with clippy
cargo clippy

# Lint with all warnings
cargo clippy -- -W clippy::all
```

## Architecture Considerations

### Graphics Pipeline
- Use Vulkan for direct graphics hardware access and precise timing control
- Consider swapchain timing and presentation modes for frame-accurate stimulus delivery
- Implement timestamp queries for measuring actual presentation times

### API Design Layers
The architecture should support multiple abstraction levels:
- **High-level**: Simple stimulus generation functions (similar to Psychtoolbox)
- **Mid-level**: Configurable rendering pipelines with sensible defaults
- **Low-level**: Direct Vulkan API access for advanced optimization

### Reproducibility
- Deterministic random number generation with seed control
- Frame-by-frame stimulus state logging
- Version-controlled stimulus parameter files

## Key Technical Constraints

1. **Timing Precision**: All timing-critical code paths must be optimized for minimal jitter
2. **GPU Synchronization**: Careful management of CPU-GPU synchronization for accurate frame timing
3. **Cross-platform Support**: Consider portability across Linux (common in research), Windows, and macOS
4. **Scientific Accuracy**: Gamma correction, color calibration, and spatial calibration support

## Clock Model

`docs/timing-conformance.md` is the normative timing contract. Do not introduce stronger guarantees in code comments, examples, or secondary guides.

Keep these distinctions explicit:

- `RenderContext::timing_source()` reports the selected backend; `FlipInfo.timing_source` reports the source of that frame's timestamp.
- A target is requested, while scanout feedback is observed. Neither is photon onset.
- Synchronous EXT flips normally use software pacing plus a hardware target. Buffered targets use the pipelined driver queue without the synchronous pacing wait.
- Present-wait completion is not a scanout timestamp. Prefer per-present `IMAGE_FIRST_PIXEL_OUT`; treat the synchronous calibrated-clock fallback according to the contract.
- Advertised, enabled, observed, and actively characterized capabilities are separate facts and can vary by display path.
- The host-clock bridge is opt-in and stays off the presentation hot path.
- Photodiode measurement on the acquisition clock remains the authority for panel light output.

The corrected 2026-08-03 reference measurement found no known present-timing conformance gap after VSE enabled the required swapchain timing bit. Never revive the earlier ANV-defect claim without new evidence and validation-clean controls.

## Related Projects

The project draws inspiration from:
- **Psychtoolbox** (MATLAB): Widely-used stimulus presentation with timing guarantees
- **PsychoPy** (Python): High-level stimulus generation for psychology experiments
- Custom stimulus engines built in C/C++ for specific lab requirements
