# VisionStimulusEngine: Project Plan & Architecture

## Executive Summary

VisionStimulusEngine (VSE) is a vision science stimulus presentation system built on Vulkan via Rust, designed to provide millisecond-accurate timing precision while offering both high-level abstractions for beginners and low-level graphics API access for advanced users. This document outlines the foundational architecture and implementation roadmap.

## Why Vulkan + Rust?

By choosing Vulkan over OpenGL (which Psychtoolbox and PsychoPy primarily rely on), VSE moves away from the "black box" of driver-side optimizations toward explicit control necessary for:
- Sub-millisecond precision timing
- True reproducibility of stimulus presentation
- Direct hardware access for maximum performance
- Deterministic behavior across sessions and systems

Rust provides:
- Memory safety without garbage collection (critical for timing-sensitive code)
- Zero-cost abstractions allowing high-level API without performance penalties
- Excellent ecosystem (Vulkano, winit, tokio) for graphics and async I/O
- Cross-platform support with predictable behavior

## Four Foundational Pillars

### 1. Core Infrastructure (Vulkano Wrappers)

Before drawing a single pixel, VSE needs a robust "Context" or "Environment" object. Vulkan setup is verbose; the goal is to abstract boilerplate while keeping handles accessible for advanced users.

#### Components:

**Instance & Physical Device Selection**
- Automatic GPU selection logic prioritizing:
  - Discrete GPUs over integrated
  - GPUs with best timing characteristics
  - Support for necessary extensions (VK_KHR_display for bypassing OS compositor)
- Allow manual override for advanced users

**Swapchain & Surface Management**
- Abstract window creation via `winit`
- Support for direct-to-display surfaces
- Multiple presentation modes (FIFO, MAILBOX, IMMEDIATE) with explanations
- Expose swapchain handle for low-level control

**Command Buffer Management**
- Implement a `Frame` abstraction encapsulating:
  - Command buffer allocation and recording
  - Fence and semaphore management
  - Automatic synchronization for basic use cases
  - Manual synchronization primitives exposed for advanced users

#### Proposed Module Structure:
```
src/
  core/
    context.rs        // VSEContext: top-level environment
    device.rs         // GPU selection and initialization
    swapchain.rs      // Swapchain creation and management
    frame.rs          // Frame abstraction with sync primitives
```

### 2. Timing & Synchronization (The "Heartbeat")

Millisecond precision is non-negotiable. VSE must handle the presentation engine differently than standard game engines.

#### Components:

**Waitable Swapchains**
- Implement support for VK_GOOGLE_display_timing or similar extensions
- Get exact timestamps of when frames hit the screen (not just submission time)
- Fallback strategies when extensions unavailable

**High-Resolution Scheduling**
- Dedicated timing loop using `std::time::Instant`
- Spin-wait capability for critical timing sections
- Configurable vsync behavior

**Flip Logging**
- Built-in system recording:
  - Requested presentation time
  - Actual presentation time
  - Missed flip count (similar to Psychtoolbox's vbl timestamps)
  - Frame duration statistics
- Export to CSV/JSON for post-hoc analysis
- Critical for validating stimulus integrity

#### Proposed Module Structure:
```
src/
  timing/
    scheduler.rs      // High-precision timing loop
    flip_logger.rs    // Frame timing recording and analysis
    vsync.rs          // VSync and presentation timing
```

### 3. The "PTB-Lite" High-Level API

To satisfy Goal 4 (Psychtoolbox API compatibility), VSE needs a layer that feels like familiar `Screen()` commands.

#### API Mapping:

| VSE Function (Rust) | PTB Equivalent | Description |
|---------------------|----------------|-------------|
| `vse.open_window()` | `Screen('OpenWindow')` | Initializes Vulkan, creates swapchain |
| `vse.draw_texture()` | `Screen('DrawTexture')` | Binds descriptor set, records draw command |
| `vse.flip()` | `Screen('Flip')` | Submits command buffer, waits for V-Sync |
| `vse.load_image()` | `Screen('MakeTexture')` | Uploads pixel data to GPU buffer/image |
| `vse.draw_rect()` | `Screen('FillRect')` | Draws filled rectangle |
| `vse.draw_line()` | `Screen('DrawLine')` | Draws line primitive |
| `vse.close_window()` | `Screen('Close')` | Cleanup and shutdown |

#### Design Principles:

- **Progressive Disclosure**: Simple calls work out-of-the-box, optional parameters expose advanced features
- **Builder Pattern**: Complex configurations use method chaining
- **Type Safety**: Leverage Rust's type system to prevent common errors at compile time

#### Proposed Module Structure:
```
src/
  api/
    window.rs         // Window management (OpenWindow, Close)
    drawing.rs        // Drawing commands (DrawTexture, FillRect, etc.)
    textures.rs       // Texture loading and management
    screen.rs         // Main Screen-like interface
```

### 4. Primitives & Shaders (The Stimulus Library)

Vision science relies on mathematically defined stimuli. Instead of loading assets, stimuli are "drawn" via fragment shaders.

#### Built-in Shader Library:

**Gratings**
- Sine/Square wave gratings
- Controllable parameters:
  - Spatial frequency
  - Phase
  - Orientation
  - Contrast
  - Envelope (Gaussian, rectangular, etc.)

**Gabor Patches**
- Gaussian-windowed sinusoidal gratings
- Applying mask within fragment shader for efficiency

**Random Dot Kinematograms (RDK)**
- Compute shaders to calculate dot positions on GPU
- Supports thousands of dots without CPU bottlenecks
- Coherence control for motion perception studies

**Noise Patterns**
- White noise
- Pink/1/f noise
- Binary noise

**Plaid Patterns**
- Multiple superimposed gratings
- Component vs. pattern motion studies

#### Uniform Buffer Management:

- Clean abstraction for passing parameters (contrast, frequency, position) to shaders
- Per-frame parameter updates without pipeline rebuilds
- Validation of parameter ranges

#### Proposed Module Structure:
```
src/
  stimuli/
    gratings.rs       // Grating stimulus
    gabor.rs          // Gabor patch
    rdk.rs            // Random dot kinematogram
    noise.rs          // Noise patterns
    plaids.rs         // Plaid patterns

  shaders/
    gratings.frag     // Grating fragment shader
    gabor.frag        // Gabor fragment shader
    rdk.comp          // RDK compute shader
    rdk.vert/.frag    // RDK rendering shaders
    // ... etc
```

## Project Directory Structure

```
VisionStimulusEngine/
├── Cargo.toml              // Main workspace manifest
├── Cargo.lock
├── README.md
├── CLAUDE.md               // Claude Code guidance
├── LICENSE
│
├── planning/               // Project planning documents
│   ├── Introduction.md
│   └── ProjectPlan.md      // This document
│
├── references/             // Academic papers and references
│
├── src/                    // Main library source code
│   ├── lib.rs              // Library root, re-exports public API
│   │
│   ├── core/               // Core Vulkan infrastructure
│   │   ├── mod.rs
│   │   ├── context.rs      // VSEContext
│   │   ├── device.rs       // GPU selection
│   │   ├── swapchain.rs    // Swapchain management
│   │   └── frame.rs        // Frame abstraction
│   │
│   ├── timing/             // Timing and synchronization
│   │   ├── mod.rs
│   │   ├── scheduler.rs
│   │   ├── flip_logger.rs
│   │   └── vsync.rs
│   │
│   ├── api/                // High-level PTB-like API
│   │   ├── mod.rs
│   │   ├── window.rs
│   │   ├── drawing.rs
│   │   ├── textures.rs
│   │   └── screen.rs
│   │
│   ├── stimuli/            // Stimulus generation
│   │   ├── mod.rs
│   │   ├── gratings.rs
│   │   ├── gabor.rs
│   │   ├── rdk.rs
│   │   ├── noise.rs
│   │   └── plaids.rs
│   │
│   └── utils/              // Utilities
│       ├── mod.rs
│       ├── color.rs        // Color space conversions, gamma
│       ├── math.rs         // Common math operations
│       └── logging.rs      // Logging infrastructure
│
├── shaders/                // GLSL shader source files
│   ├── build.rs            // Shader compilation script
│   ├── common/             // Shared shader code
│   ├── gratings.frag
│   ├── gabor.frag
│   ├── rdk.comp
│   ├── rdk.vert
│   └── rdk.frag
│
├── examples/               // Example programs
│   ├── 01_calibration_square.rs   // First milestone
│   ├── 02_simple_grating.rs
│   ├── 03_gabor_patch.rs
│   └── 04_random_dots.rs
│
├── tests/                  // Integration tests
│   ├── timing_tests.rs     // Timing precision validation
│   └── rendering_tests.rs  // Rendering correctness
│
└── benches/                // Performance benchmarks
    ├── frame_timing.rs
    └── stimulus_gen.rs
```

## First Milestone: "The Calibration Square"

The "Hello World" for VSE should demonstrate core functionality and timing precision.

### Requirements:

1. Initialize a Vulkan window using VSE
2. Display a 100x100 pixel white square on grey background
3. Flip the square's color every 60 frames
4. Log the exact microsecond timestamp of every flip to CSV
5. Report timing statistics (mean, std, missed frames)

### Success Criteria:

- Runs at 60 Hz with < 1ms timing jitter
- Zero missed frames over 10-minute test
- CSV log contains accurate timestamps
- Code is < 100 lines using high-level API
- Advanced users can access underlying Vulkan objects

### Example Usage (Target API):

```rust
use vision_stimulus_engine::prelude::*;

fn main() -> Result<()> {
    // High-level API: simple and clean
    let mut vse = VSE::builder()
        .with_window_size(1920, 1080)
        .with_vsync(true)
        .with_flip_logging("calibration_square.csv")
        .build()?;

    let grey = Color::rgb(0.5, 0.5, 0.5);
    let white = Color::rgb(1.0, 1.0, 1.0);
    let black = Color::rgb(0.0, 0.0, 0.0);

    let mut frame_count = 0;
    let mut square_white = true;

    loop {
        vse.clear(grey);

        let color = if square_white { white } else { black };
        vse.draw_rect(100, 100, 200, 200, color);

        let flip_info = vse.flip()?;

        frame_count += 1;
        if frame_count >= 60 {
            square_white = !square_white;
            frame_count = 0;
        }

        if vse.should_close() {
            break;
        }
    }

    vse.print_timing_stats();
    Ok(())
}
```

## Implementation Roadmap

### Phase 1: Foundation

**Goal:** Get Vulkan initialization and basic rendering working

- [ ] Initialize Rust project with Cargo
- [ ] Add dependencies (vulkano, winit, tokio)
- [ ] Implement core module (context, device, swapchain)
- [ ] Basic window creation and clear color
- [ ] Simple event loop with clean shutdown

### Phase 2: Timing Infrastructure

**Goal:** Achieve millisecond-accurate timing

- [ ] Implement flip logger with timestamp recording
- [ ] Add VSync synchronization
- [ ] Implement high-resolution timing loop
- [ ] Add timing statistics calculation
- [ ] Validate timing precision with external tools

### Phase 3: Basic Drawing 

**Goal:** Draw simple primitives

- [ ] Implement rectangle drawing
- [ ] Implement circle drawing
- [ ] Implement line drawing
- [ ] Implement gabor patch drawing
- [ ] Implement texture loading and rendering
- [ ] Add color management
- [ ] Complete calibration square example

### Phase 4: Shader-Based Stimuli

**Goal:** Implement mathematically-defined stimuli

- [ ] Set up shader compilation pipeline
- [ ] Implement grating stimulus with shader
- [ ] Implement Gabor patch
- [ ] Add uniform buffer management
- [ ] Implement parameter validation

### Phase 5: Advanced Stimuli

**Goal:** Complex stimulus generation

- [ ] Implement RDK with compute shaders
- [ ] Add noise patterns
- [ ] Add plaid patterns
- [ ] Optimize performance for thousands of elements

### Phase 6: Polish & Documentation

**Goal:** Production-ready release

- [ ] Comprehensive API documentation
- [ ] Tutorial examples covering common use cases
- [ ] Performance benchmarks
- [ ] Cross-platform testing (Linux, Windows, macOS)
- [ ] Migration guide from Psychtoolbox

## Key Technical Considerations

### Timing Precision Strategy

1. **GPU Timestamp Queries:** Use `vkCmdWriteTimestamp` to measure actual GPU execution times
2. **Present Timing Extension:** Leverage `VK_GOOGLE_display_timing` when available
3. **Busy-Wait Option:** Allow spin-wait for final microseconds before critical operations
4. **Real-Time Thread Priority:** Option to request real-time scheduling (platform-dependent)

### Reproducibility Strategy

1. **Deterministic RNG:** Use seedable PRNG (e.g., `rand_chacha`) with explicit seed control
2. **Frame-by-Frame Logging:** Record all stimulus parameters for each frame
3. **Version Pinning:** Lock Vulkan driver version requirements in documentation
4. **Shader Compilation:** Include pre-compiled SPIR-V to avoid driver compiler differences

### Cross-Platform Support

**Linux** (Primary target for research)
- Direct-to-display support via VK_KHR_display
- Real-time scheduling via `sched_setscheduler`
- Excellent Vulkan driver support (NVIDIA proprietary, AMD RADV)

**Windows**
- Standard windowed mode
- Consider NVIDIA Nsight for timing analysis
- Test with various driver versions

**macOS**
- Vulkan via MoltenVK (Metal translation layer)
- May have timing limitations vs. native Vulkan
- Document any platform-specific caveats

### Performance Targets

- **Frame Rate:** Stable 60/120/144 Hz (depending on monitor)
- **Jitter:** < 0.5 ms standard deviation at 60 Hz
- **Missed Frames:** Zero over extended runs (hours)
- **Startup Time:** < 1 second to first frame
- **Parameter Updates:** < 100 µs to modify stimulus parameters

## Dependencies

### Core Dependencies

```toml
[dependencies]
# Vulkan abstraction
vulkano = "0.34"
vulkano-shaders = "0.34"

# Windowing
winit = "0.29"

# Math
nalgebra = "0.32"
glam = "0.24"

# Random number generation
rand = "0.8"
rand_chacha = "0.3"

# Serialization for logging
serde = { version = "1.0", features = ["derive"] }
csv = "1.3"

# Error handling
anyhow = "1.0"
thiserror = "1.0"

# Time
chrono = "0.4"
```

### Optional Dependencies

```toml
[dependencies]
# Advanced logging
tracing = "0.1"
tracing-subscriber = "0.3"
```

## Testing Strategy

### Unit Tests
- Test stimulus parameter validation
- Test color space conversions
- Test timing calculations

### Integration Tests
- Test full window creation and rendering pipeline
- Test timing precision over extended runs

### Benchmarks
- Frame generation performance
- Stimulus parameter update latency
- Command buffer recording overhead

## Documentation Strategy

### Code Documentation
- Rustdoc comments for all public APIs
- Examples in doc comments
- Link to relevant Psychtoolbox equivalents

### User Guides
- Getting Started tutorial
- Stimulus Library guide
- Timing and Synchronization best practices
- Hardware Integration guide
- Migration from Psychtoolbox

### API Reference
- Generated from Rustdoc
- Organized by module
- Searchable online

## Success Metrics

VSE will be considered successful when:

1. **Timing:** Achieves < 0.5ms jitter at 60 Hz on reference hardware
2. **Reproducibility:** Same stimulus code produces identical output across runs
3. **Usability:** Vision scientist can generate first stimulus in < 30 minutes
4. **Performance:** Handles complex stimuli (thousands of elements) at 60+ Hz
5. **Adoption:** At least one research lab successfully runs experiments using VSE

## Next Steps

1. Initialize Rust project structure
2. Set up CI/CD pipeline
3. Implement core Vulkan initialization (Phase 1)
4. Build calibration square example (First Milestone)
5. Validate timing precision with external measurement tools
6. Iterate based on feedback from vision science community

## Conclusion

VisionStimulusEngine represents a significant step forward in vision science stimulus presentation. By leveraging Vulkan's explicit control and Rust's safety guarantees, VSE can deliver both the precision required for neural recording experiments and the flexibility needed for complex, naturalistic stimuli. The staged implementation approach balances immediate usability with long-term architectural goals, ensuring that early adopters can begin using VSE while advanced features are still in development.
