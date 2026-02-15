# Phase 1: Foundation - Implementation Guide

## Overview

Phase 1 establishes the foundational Vulkan infrastructure for VisionStimulusEngine. This phase focuses on getting a minimal but robust rendering system working with proper initialization, window management, and clean shutdown.

**Goal:** Get Vulkan initialization and basic rendering working

**Success Criteria:**
- A window opens with a configurable background color
- The application runs at stable frame rate (60 Hz)
- Clean shutdown with no resource leaks
- All Vulkan objects properly initialized and destroyed
- Event loop responds to window close events
- Code is well-documented and follows Rust best practices

## Initial Project Structure

### Step 1: Initialize Rust Project

```bash
# Create new library project
cargo new --lib vision_stimulus_engine
cd vision_stimulus_engine

# Create necessary directories
mkdir -p src/core
mkdir -p examples
mkdir -p tests
mkdir -p benches
mkdir -p shaders
```

### Step 2: Project Directory Layout

```
VisionStimulusEngine/
├── Cargo.toml              # Workspace manifest
├── Cargo.lock
├── README.md
├── CLAUDE.md
├── LICENSE
│
├── planning/               # Existing planning documents
├── references/             # Existing academic references
│
├── src/
│   ├── lib.rs              # Library root (Phase 1 focus)
│   │
│   └── core/               # Core Vulkan infrastructure (Phase 1)
│       ├── mod.rs
│       ├── context.rs      # VSEContext - top-level environment
│       ├── device.rs       # GPU selection and initialization
│       ├── swapchain.rs    # Swapchain creation and management
│       └── frame.rs        # Frame abstraction with sync primitives
│
├── examples/
│   └── 00_clear_color.rs   # Phase 1 milestone example
│
└── tests/
    └── core_tests.rs       # Basic integration tests
```

## Dependencies

### Cargo.toml Configuration

```toml
[package]
name = "vision_stimulus_engine"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"  # Minimum Rust version

[dependencies]
# Vulkan abstraction - Core rendering
vulkano = "0.34"

# Windowing and event handling
winit = "0.29"

# Error handling
anyhow = "1.0"
thiserror = "1.0"

# Logging (helps with debugging Vulkan initialization)
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
# Testing utilities
criterion = "0.5"

[profile.dev]
# Faster builds during development
opt-level = 1

[profile.release]
# Maximum optimization for timing-critical code
opt-level = 3
lto = true
codegen-units = 1
```

### Dependency Rationale

- **vulkano (0.34)**: Safe Rust wrapper around Vulkan API. Provides type-safe abstractions while maintaining access to low-level Vulkan functionality.
- **winit (0.29)**: Cross-platform window creation and event handling. Standard choice for Rust graphics applications.
- **anyhow**: Convenient error handling for application code.
- **thiserror**: Derive macro for custom error types in library code.
- **tracing**: Structured logging that's more flexible than simple println! debugging.

## Core Module Implementation

### 1. `src/lib.rs` - Library Root

**Purpose:** Re-export public API and provide top-level documentation.

```rust
//! VisionStimulusEngine (VSE)
//!
//! A vision science stimulus presentation system built on Vulkan,
//! designed for millisecond-accurate timing precision.
//!
//! # Quick Start
//!
//! ```no_run
//! use vision_stimulus_engine::core::VSEContext;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let context = VSEContext::new()?;
//! # Ok(())
//! # }
//! ```

// Re-export core types for easy access
pub mod core;

// Re-export commonly used types
pub mod prelude {
    pub use crate::core::{VSEContext, VSEContextBuilder};
}
```

### 2. `src/core/mod.rs` - Core Module

**Purpose:** Organize core Vulkan infrastructure modules.

```rust
//! Core Vulkan infrastructure
//!
//! This module contains the fundamental Vulkan initialization and
//! management code, abstracted for ease of use while maintaining
//! access to underlying Vulkan objects.

mod context;
mod device;
mod swapchain;
mod frame;

// Public API exports
pub use context::{VSEContext, VSEContextBuilder};
pub use device::{DeviceSelector, GPUPreference};
pub use swapchain::{SwapchainConfig, PresentMode};
pub use frame::Frame;
```

### 3. `src/core/device.rs` - GPU Selection

**Purpose:** Encapsulate Vulkan instance creation and physical device selection.

**Key Types:**
```rust
/// Preference for GPU selection
pub enum GPUPreference {
    /// Prefer discrete GPU (dedicated graphics card)
    Discrete,
    /// Prefer integrated GPU
    Integrated,
    /// Use first available GPU
    Any,
}

/// Device selector handles Vulkan instance and physical device selection
pub struct DeviceSelector {
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
}
```

**Key Methods:**
```rust
impl DeviceSelector {
    /// Create new device selector with specified GPU preference
    pub fn new(preference: GPUPreference) -> Result<Self, DeviceError>;

    /// Get the Vulkan instance
    pub fn instance(&self) -> &Arc<Instance>;

    /// Get the selected physical device
    pub fn physical_device(&self) -> &Arc<PhysicalDevice>;

    /// Create a logical device with necessary queues
    pub fn create_device(&self) -> Result<(Arc<Device>, Arc<Queue>), DeviceError>;
}
```

**Implementation Details:**
- Initialize Vulkan instance with appropriate validation layers (debug builds)
- Enumerate physical devices
- Score devices based on preference (discrete vs integrated)
- Select device with best timing characteristics when possible
- Create logical device with graphics queue family

### 4. `src/core/swapchain.rs` - Swapchain Management

**Purpose:** Abstract swapchain creation and management for double/triple buffering.

**Key Types:**
```rust
/// Swapchain configuration options
pub struct SwapchainConfig {
    pub width: u32,
    pub height: u32,
    pub present_mode: PresentMode,
    pub image_count: u32,  // Number of swapchain images
}

/// Presentation mode (affects timing behavior)
pub enum PresentMode {
    /// VSync - wait for vertical blank (best for timing precision)
    Fifo,
    /// No VSync - immediate presentation (may tear)
    Immediate,
    /// Mailbox - low latency with no tearing
    Mailbox,
}

/// Manages swapchain and associated resources
pub struct SwapchainManager {
    swapchain: Arc<Swapchain>,
    images: Vec<Arc<Image>>,
}
```

**Key Methods:**
```rust
impl SwapchainManager {
    /// Create new swapchain manager
    pub fn new(
        device: Arc<Device>,
        surface: Arc<Surface>,
        config: SwapchainConfig,
    ) -> Result<Self, SwapchainError>;

    /// Get swapchain images
    pub fn images(&self) -> &[Arc<Image>];

    /// Acquire next image for rendering
    pub fn acquire_next_image(
        &mut self,
    ) -> Result<(usize, SwapchainAcquireFuture), SwapchainError>;

    /// Recreate swapchain (e.g., after window resize)
    pub fn recreate(&mut self, new_config: SwapchainConfig) -> Result<(), SwapchainError>;
}
```

### 5. `src/core/frame.rs` - Frame Abstraction

**Purpose:** Encapsulate per-frame rendering state and synchronization.

**Key Types:**
```rust
/// Represents a single frame of rendering
pub struct Frame {
    image_index: usize,
    command_buffer: Arc<PrimaryAutoCommandBuffer>,
}

/// Frame builder for recording commands
pub struct FrameBuilder {
    device: Arc<Device>,
    queue: Arc<Queue>,
}
```

**Key Methods:**
```rust
impl FrameBuilder {
    /// Create new frame builder
    pub fn new(device: Arc<Device>, queue: Arc<Queue>) -> Self;

    /// Begin recording a new frame
    pub fn begin(&self) -> Result<Frame, FrameError>;
}

impl Frame {
    /// Execute frame commands and present to screen
    pub fn present(
        self,
        swapchain: &SwapchainManager,
    ) -> Result<(), FrameError>;
}
```

### 6. `src/core/context.rs` - VSEContext (Top-Level API)

**Purpose:** Provide the main entry point for VSE, managing all Vulkan resources.

**Key Type:**
```rust
/// Main VisionStimulusEngine context
///
/// This is the primary interface for creating windows and managing
/// the rendering environment.
pub struct VSEContext {
    // Internal Vulkan objects
    device_selector: DeviceSelector,
    device: Arc<Device>,
    queue: Arc<Queue>,
    surface: Arc<Surface>,
    window: Arc<Window>,
    swapchain: SwapchainManager,
    event_loop: Option<EventLoop<()>>,

    // Configuration
    clear_color: [f32; 4],
    should_close: bool,
}

/// Builder for VSEContext with sensible defaults
pub struct VSEContextBuilder {
    window_width: u32,
    window_height: u32,
    window_title: String,
    gpu_preference: GPUPreference,
    present_mode: PresentMode,
    clear_color: [f32; 4],
}
```

**Key Methods:**
```rust
impl VSEContext {
    /// Create new VSE context with default settings
    pub fn new() -> Result<Self, VSEError>;

    /// Create builder for custom configuration
    pub fn builder() -> VSEContextBuilder;

    /// Run the main event loop
    pub fn run<F>(mut self, mut render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut Self) -> Result<(), VSEError> + 'static;

    /// Clear the screen with the configured clear color
    pub fn clear(&mut self) -> Result<(), VSEError>;

    /// Present the current frame to the screen
    pub fn flip(&mut self) -> Result<(), VSEError>;

    /// Check if window should close
    pub fn should_close(&self) -> bool;

    /// Set clear color (RGBA, 0.0-1.0 range)
    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32, a: f32);

    // Advanced: expose underlying Vulkan objects for power users
    pub fn device(&self) -> &Arc<Device>;
    pub fn queue(&self) -> &Arc<Queue>;
    pub fn swapchain(&self) -> &SwapchainManager;
}

impl VSEContextBuilder {
    /// Set window dimensions
    pub fn with_window_size(mut self, width: u32, height: u32) -> Self;

    /// Set window title
    pub fn with_title(mut self, title: impl Into<String>) -> Self;

    /// Set GPU preference
    pub fn with_gpu_preference(mut self, preference: GPUPreference) -> Self;

    /// Set presentation mode
    pub fn with_present_mode(mut self, mode: PresentMode) -> Self;

    /// Set initial clear color
    pub fn with_clear_color(mut self, r: f32, g: f32, b: f32, a: f32) -> Self;

    /// Build the context
    pub fn build(self) -> Result<VSEContext, VSEError>;
}
```

## Error Handling

### Custom Error Types

Each module should define its own error type using `thiserror`:

```rust
// In device.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DeviceError {
    #[error("No suitable Vulkan device found")]
    NoDeviceFound,

    #[error("Failed to create Vulkan instance: {0}")]
    InstanceCreationFailed(String),

    #[error("Vulkan error: {0}")]
    VulkanError(#[from] vulkano::VulkanError),
}

// In context.rs
#[derive(Error, Debug)]
pub enum VSEError {
    #[error("Device error: {0}")]
    Device(#[from] DeviceError),

    #[error("Swapchain error: {0}")]
    Swapchain(#[from] SwapchainError),

    #[error("Frame error: {0}")]
    Frame(#[from] FrameError),

    #[error("Window error: {0}")]
    Window(String),
}
```

## Phase 1 Milestone Example

### `examples/00_clear_color.rs`

**Purpose:** Demonstrate basic window creation and clear color rendering.

```rust
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create VSE context with custom settings
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE Phase 1 - Clear Color")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)  // Grey background
        .build()?;

    // Run event loop
    context.run(|vse| {
        // Clear screen and present
        vse.clear()?;
        vse.flip()?;

        Ok(())
    })?;

    Ok(())
}
```

**Expected Behavior:**
- Opens an 800x600 window with grey background
- Window remains open until closed by user
- Runs at stable frame rate with no flicker
- Clean shutdown with no errors

## Implementation Order

### Recommended Step-by-Step Approach

1. **Initialize Project Structure**
   - Create directories
   - Set up Cargo.toml with dependencies
   - Create initial module files

2. **Implement Device Selection (`device.rs`)**
   - Vulkan instance creation
   - Physical device enumeration and selection
   - Logical device creation
   - Test: Can create a device and print GPU name

3. **Implement Window and Surface (`context.rs` partial)**
   - Create winit window
   - Create Vulkan surface from window
   - Test: Window opens and responds to close event

4. **Implement Swapchain (`swapchain.rs`)**
   - Swapchain creation with surface
   - Image acquisition
   - Test: Can acquire and release swapchain images

5. **Implement Frame Management (`frame.rs`)**
   - Command buffer allocation
   - Basic clear command recording
   - Present operation
   - Test: Can clear screen to a color

6. **Complete VSEContext Integration (`context.rs`)**
   - Integrate all components
   - Implement event loop
   - Implement builder pattern
   - Test: Full example runs

7. **Testing and Validation**
   - Run example for extended period (10 minutes)
   - Monitor for resource leaks (use `valgrind` on Linux)
   - Verify clean shutdown
   - Check frame timing consistency

## Testing Strategy

### Unit Tests (`tests/core_tests.rs`)

```rust
#[test]
fn test_device_selection() {
    let selector = DeviceSelector::new(GPUPreference::Any);
    assert!(selector.is_ok());
}

#[test]
fn test_context_creation() {
    let context = VSEContext::new();
    assert!(context.is_ok());
}

#[test]
fn test_builder_pattern() {
    let context = VSEContext::builder()
        .with_window_size(640, 480)
        .with_title("Test Window")
        .build();
    assert!(context.is_ok());
}
```

### Integration Test (Manual)

Run the clear color example:
```bash
cargo run --example 00_clear_color
```

**Validation checklist:**
- [ ] Window opens without errors
- [ ] Clear color is displayed correctly
- [ ] Window can be closed cleanly
- [ ] No error messages in console
- [ ] No memory leaks (check with system monitor)

## Performance Targets for Phase 1

While Phase 2 will focus on timing precision, Phase 1 should establish baseline performance:

- **Startup Time:** < 2 seconds to first frame
- **Frame Rate:** Stable 60 FPS on modern hardware
- **CPU Usage:** < 5% on modern CPU (idle event loop)
- **Memory Usage:** < 100 MB for basic window

## Common Issues and Solutions

### Issue: "Failed to create Vulkan instance"
**Solution:** Ensure Vulkan drivers are installed. On Linux, install `vulkan-tools` package and verify with `vulkaninfo`.

### Issue: "No suitable device found"
**Solution:** GPU must support Vulkan 1.0+. Update graphics drivers.

### Issue: Window immediately closes
**Solution:** Check event loop implementation - ensure it's properly handling events.

### Issue: Validation layer errors
**Solution:** Review Vulkan object creation and destruction order. Ensure proper lifetime management.

## Documentation Requirements

Each public API must have:
- Doc comment explaining purpose
- Example usage in doc comment
- Parameter descriptions
- Return value description
- Error conditions

Example:
```rust
/// Clear the screen with the configured clear color.
///
/// This records a clear command to the current frame's command buffer.
/// The actual clear operation happens during [`flip()`](Self::flip).
///
/// # Examples
///
/// ```no_run
/// # use vision_stimulus_engine::prelude::*;
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut vse = VSEContext::new()?;
/// vse.set_clear_color(1.0, 0.0, 0.0, 1.0);  // Red
/// vse.clear()?;
/// vse.flip()?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns `VSEError::Frame` if command buffer recording fails.
pub fn clear(&mut self) -> Result<(), VSEError> {
    // Implementation
}
```

## Next Steps After Phase 1

Once Phase 1 is complete and validated:

1. **Phase 2:** Implement timing infrastructure with frame logging
2. **Phase 2:** Add VSync synchronization and timing validation
3. **Phase 3:** Implement rectangle drawing primitives
4. **Phase 3:** Complete the "Calibration Square" milestone

## Success Checklist

Phase 1 is complete when:

- [ ] `cargo build` succeeds without warnings
- [ ] `cargo test` passes all tests
- [ ] `cargo clippy` shows no warnings
- [ ] `examples/00_clear_color.rs` runs successfully
- [ ] Example runs for 10+ minutes without crashes
- [ ] Window can be resized without errors
- [ ] Clean shutdown verified (no resource leaks)
- [ ] All public APIs have documentation
- [ ] Code follows Rust best practices (proper ownership, no unsafe unless necessary)

## Resources

### Vulkano Documentation
- [Vulkano Guide](https://vulkano.rs/guide/introduction)
- [Vulkano API Docs](https://docs.rs/vulkano/)

### Winit Documentation
- [Winit Guide](https://docs.rs/winit/)

### Vulkan Specification
- [Vulkan 1.0 Spec](https://www.khronos.org/registry/vulkan/specs/1.0/html/)
- [Vulkan Tutorial](https://vulkan-tutorial.com/) (C++ but concepts apply)

### Rust Graphics Ecosystem
- [Learn Wgpu](https://sotrh.github.io/learn-wgpu/) (Alternative API, similar concepts)
- [Awesome Rust Graphics](https://github.com/rib/awesome-rust-graphics)
