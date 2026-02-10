//! Core module integration tests
//!
//! Note: These tests require a working Vulkan installation.
//! Tests that require a display will be skipped in headless CI environments.

use vision_stimulus_engine::prelude::*;

/// Test that GPUPreference has sensible defaults
#[test]
fn test_gpu_preference_default() {
    let pref = GPUPreference::default();
    assert_eq!(pref, GPUPreference::Discrete);
}

/// Test that PresentMode has sensible defaults
#[test]
fn test_present_mode_default() {
    let mode = PresentMode::default();
    assert_eq!(mode, PresentMode::Fifo);
}

/// Test builder pattern for VSEContext
///
/// Note: `.build()` creates an EventLoop which must run on the main thread.
/// This test is ignored by default since the test harness uses worker threads.
/// Run manually with: `cargo test -- --ignored test_context_builder_pattern`
#[test]
#[ignore]
fn test_context_builder_pattern() {
    let result = VSEContext::builder()
        .with_window_size(640, 480)
        .with_title("Test Window")
        .with_clear_color(1.0, 0.0, 0.0, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_gpu_preference(GPUPreference::Any)
        .build();

    if let Err(e) = &result {
        eprintln!(
            "Note: Context creation failed (expected in headless CI): {}",
            e
        );
    }
}

/// Test SwapchainConfig defaults
#[test]
fn test_swapchain_config_default() {
    let config = SwapchainConfig::default();
    assert_eq!(config.width, 800);
    assert_eq!(config.height, 600);
    assert_eq!(config.present_mode, PresentMode::Fifo);
    assert_eq!(config.image_count, 2);
}

/// Test that clear color can be specified
#[test]
fn test_clear_color_specification() {
    let _context = VSEContext::builder().with_clear_color(0.5, 0.5, 0.5, 1.0);
    // Builder should accept the color without panicking
}

/// Test window size specification
#[test]
fn test_window_size_specification() {
    let _context = VSEContext::builder().with_window_size(1920, 1080);
    // Builder should accept the size without panicking
}

/// Test title specification
#[test]
fn test_title_specification() {
    let _context = VSEContext::builder().with_title("Custom Title");
    // Builder should accept the title without panicking
}

/// Test chained builder methods
#[test]
fn test_builder_chaining() {
    let _builder = VSEContext::builder()
        .with_window_size(1024, 768)
        .with_title("Chained Builder Test")
        .with_clear_color(0.2, 0.3, 0.4, 1.0)
        .with_present_mode(PresentMode::Mailbox)
        .with_gpu_preference(GPUPreference::Integrated);
    // All methods should chain without issues
}
