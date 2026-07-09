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

- **Spec status**: Released in Vulkan 1.4.335 (November 2025), after ~5 years of development. This is now a finalized `EXT`, not a provisional draft.
- **Driver support**: Shipping in stable **Mesa 26.1** (RADV, NVK, ANV, and smaller drivers; both X11 and Wayland) and the **NVIDIA 595** series (beta as of early 2026). **Verified on jiji**: Intel Meteor Lake (ANV), Mesa 26.1.4, `VK_EXT_present_timing` revision 3, alongside `VK_KHR_present_id2` / `present_wait2`.
- **Provides**: Everything GOOGLE_display_timing does, plus:
  - Hardware scanout timestamps (more precise than driver-reported)
  - Driver-managed frame scheduling (CPU freed immediately after submit, no spin-wait needed)
  - Better VRR (variable refresh rate) support
  - Cross-platform (not limited to Linux/Android)
- **Rust bindings**: Raw bindings already exist on `ash`'s git `master` (generated from the updated `vk.xml`) — `VkPresentTimingInfoEXT`, `VkPhysicalDevicePresentTimingFeaturesEXT`, `VkPastPresentationTimingEXT`, the `ERROR_PRESENT_TIMING_QUEUE_FULL_EXT` variant, and the `vkGetPastPresentationTimingEXT` command pointer. They are **not** in any published crates.io release (latest is 0.38.0+1.3.281, April 2024, Vulkan 1.3.281), and there is no ergonomic `extensions/ext` wrapper yet. Adopting it means pinning `ash` to git (or a future release) and driving it through the raw function pointers.
- **VSE status**: Stubbed in TimingProvider trait. Both the driver and the raw `ash` bindings are available now; implementation is a matter of pinning `ash` to git and wiring the calls.

### Comparison

| Capability | CpuEstimate | GOOGLE_display_timing | EXT_present_timing |
|---|---|---|---|
| Present timestamps | CPU clock after fence | Driver-reported display time | Hardware scanout time |
| Refresh rate query | Auto-detect from frames | `vkGetRefreshCycleDuration` | `vkGetRefreshCycleDuration` |
| Scheduled presents | CPU spin-wait | `VkPresentTimeGOOGLE` | Hardware-scheduled |
| CPU cost of scheduling | High (spin loop) | Low | Near zero |
| Platform support | Universal | Linux, Android | Cross-platform (shipping) |

## Implementation Plan

1. **Now**: TimingProvider trait with CpuEstimate and GoogleDisplayTiming backends
2. **Next**: Pin `ash` to git (`master` carries the raw present_timing bindings) and add an ExtPresentTimingProvider implementation via the raw function pointers
3. **When vulkano updates to ash 0.39+**: Consider moving extension calls through vulkano if it adds native support

## Contributing to Upstream

We intend to help bring VK_EXT_present_timing support to the Rust Vulkan ecosystem:
- Track ash Vulkan 1.4 support progress: https://github.com/ash-rs/ash
- Track vulkano extension support: https://github.com/vulkano-rs/vulkano
- Contribute bindings or testing when development branches become available
