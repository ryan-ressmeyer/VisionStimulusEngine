# VSE Timing Roadmap

## Current State (Phase 2)

VSE captures two CPU timestamps per frame via `std::time::Instant`:
- **submit_time**: just before command buffer submission
- **present_time**: after GPU fence signals (frame queued for display)

This gives microsecond CPU-clock resolution but does not reflect actual display scanout time. The gap between fence signal and photon emission is unknown and variable.

## Vulkan Timing Extensions

### VK_GOOGLE_display_timing

- **Spec status**: Stable, widely implemented
- **Driver support**: Mesa (Linux), Android drivers
- **Provides**:
  - `vkGetRefreshCycleDurationGOOGLE` — monitor refresh period in nanoseconds
  - `vkGetPastPresentationTimingGOOGLE` — actual presentation timestamps for completed frames
  - `VkPresentTimeGOOGLE` — schedule a present for a specific target time
- **Rust bindings**: Available in ash 0.38+
- **VSE status**: Implemented via ash 0.38 raw function calls

### VK_EXT_present_timing

- **Spec status**: Merged into Vulkan spec, late 2025
- **Driver support**: Mesa 26.1+ (in development), NVIDIA beta drivers
- **Provides**: Everything GOOGLE_display_timing does, plus:
  - Hardware scanout timestamps (more precise than driver-reported)
  - Driver-managed frame scheduling (CPU freed immediately after submit, no spin-wait needed)
  - Better VRR (variable refresh rate) support
  - Cross-platform (not limited to Linux/Android)
- **Rust bindings**: Not yet available. Expected in ash 0.39+ (Vulkan 1.4 support)
- **VSE status**: Stubbed in TimingProvider trait. Will be implemented when bindings land.

### Comparison

| Capability | CpuEstimate | GOOGLE_display_timing | EXT_present_timing |
|---|---|---|---|
| Present timestamps | CPU clock after fence | Driver-reported display time | Hardware scanout time |
| Refresh rate query | Auto-detect from frames | `vkGetRefreshCycleDuration` | `vkGetRefreshCycleDuration` |
| Scheduled presents | CPU spin-wait | `VkPresentTimeGOOGLE` | Hardware-scheduled |
| CPU cost of scheduling | High (spin loop) | Low | Near zero |
| Platform support | Universal | Linux, Android | Cross-platform (future) |

## Implementation Plan

1. **Now**: TimingProvider trait with CpuEstimate and GoogleDisplayTiming backends
2. **When ash 0.39 ships**: Add ExtPresentTimingProvider implementation
3. **When vulkano updates to ash 0.39+**: Consider moving extension calls through vulkano if it adds native support

## Contributing to Upstream

We intend to help bring VK_EXT_present_timing support to the Rust Vulkan ecosystem:
- Track ash Vulkan 1.4 support progress: https://github.com/ash-rs/ash
- Track vulkano extension support: https://github.com/vulkano-rs/vulkano
- Contribute bindings or testing when development branches become available
