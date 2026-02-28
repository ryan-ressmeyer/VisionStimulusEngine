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

/// Test that HostInfo struct is accessible from prelude
#[test]
fn test_host_info_in_prelude() {
    let build = vision_stimulus_engine::host::BuildInfo::from_compile_time();
    assert!(!build.vse_version.is_empty());
}

use vision_stimulus_engine::core::{
    InputEvent, KeyCode, MonitorInfo, MonitorSelection, MouseButton, VideoModeInfo, WindowMode,
};

#[test]
fn test_window_mode_default() {
    let mode = WindowMode::default();
    assert!(matches!(mode, WindowMode::Windowed));
}

#[test]
fn test_monitor_selection_default() {
    let sel = MonitorSelection::default();
    assert!(matches!(sel, MonitorSelection::Primary));
}

#[test]
fn test_mouse_button_variants() {
    let left = MouseButton::Left;
    let right = MouseButton::Right;
    let middle = MouseButton::Middle;
    let other = MouseButton::Other(4);
    // Ensure they're distinct via Debug
    assert_ne!(format!("{:?}", left), format!("{:?}", right));
    assert_ne!(format!("{:?}", middle), format!("{:?}", other));
}

#[test]
fn test_video_mode_info_fields() {
    let mode = VideoModeInfo {
        width: 1920,
        height: 1080,
        refresh_rate_hz: 144.0,
        bit_depth: 32,
    };
    assert_eq!(mode.width, 1920);
    assert_eq!(mode.refresh_rate_hz, 144.0);
}

#[test]
fn test_monitor_info_fields() {
    let info = MonitorInfo {
        name: Some("Test Monitor".into()),
        index: 0,
        width: 2560,
        height: 1440,
        refresh_rate_hz: Some(165.0),
        scale_factor: 1.0,
        position: (0, 0),
        video_modes: vec![],
    };
    assert_eq!(info.name.as_deref(), Some("Test Monitor"));
    assert_eq!(info.width, 2560);
}

#[test]
fn test_builder_with_window_mode() {
    let _builder = VSEContext::builder()
        .with_window_mode(WindowMode::ExclusiveFullscreen);
}

#[test]
fn test_builder_with_monitor() {
    let _builder = VSEContext::builder()
        .with_monitor(MonitorSelection::Index(1));
}

#[test]
fn test_builder_with_cursor_visible() {
    let _builder = VSEContext::builder()
        .with_cursor_visible(false);
}

#[test]
fn test_builder_fullscreen_chain() {
    let _builder = VSEContext::builder()
        .with_window_mode(WindowMode::BorderlessFullscreen)
        .with_monitor(MonitorSelection::Name("ASUS".into()))
        .with_cursor_visible(false)
        .with_window_size(1920, 1080);
}

#[test]
fn test_keycode_reexport() {
    let _key = KeyCode::Escape;
    let _space = KeyCode::Space;
    let _a = KeyCode::KeyA;
}

#[test]
fn test_mouse_button_equality() {
    assert_eq!(MouseButton::Left, MouseButton::Left);
    assert_ne!(MouseButton::Left, MouseButton::Right);
    assert_eq!(MouseButton::Other(5), MouseButton::Other(5));
    assert_ne!(MouseButton::Other(5), MouseButton::Other(6));
}
