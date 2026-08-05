# VSE timing roadmap

The implemented timing contract is defined in [Timing conformance](timing-conformance.md). This file tracks remaining engineering work and must not be used as a capability reference.

## Current implementation

- The preferred displayed backend uses `VK_EXT_present_timing` with present-id2 correlation and present-wait2 where available.
- Individual `FlipInfo` receipts distinguish scanout timestamps from CPU fallbacks.
- Synchronous scheduled flips combine software pacing with an EXT target request; buffered scheduled frames use the pipelined target path.
- Headless timing is synthesized and tagged `Offscreen`.
- `HostInfo.timing` records advertised surface/device support separately from observed feedback and explicit scheduling characterization.
- `VK_GOOGLE_display_timing` is not used.

## Maintenance work

- Re-characterize supported display paths after driver, kernel, compositor, or hardware changes.
- Keep Vulkan 1.4 hand-written bindings synchronized with Vulkan-Headers until `ash` and `vulkano` expose the required types.
- Replace dated measurements rather than accumulating contradictory hardware narratives.
- Add a richer recorded representation if future analyses need an explicit unknown state for target attainment rather than deriving it from `timing_source`.

Track dependency changes in [Upstream watch list](upstream-watch.md).
