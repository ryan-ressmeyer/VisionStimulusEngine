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

This is an early-stage project. The repository currently contains:
- Planning documents in `planning/`
- Academic references (PDFs) in `references/` related to vision science and stimulus presentation

## Development Commands

*Note: This section will be populated once the Rust project structure is initialized.*

### Building and Testing
```bash
# Build project (once Cargo.toml exists)
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

## Related Projects

The project draws inspiration from:
- **Psychtoolbox** (MATLAB): Widely-used stimulus presentation with timing guarantees
- **PsychoPy** (Python): High-level stimulus generation for psychology experiments
- Custom stimulus engines built in C/C++ for specific lab requirements
