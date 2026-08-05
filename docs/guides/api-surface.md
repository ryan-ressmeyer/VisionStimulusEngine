# API Imports and Audit 8 Migration

VSE's prelude contains types used in ordinary experiment code. Specialized APIs remain public under their domain modules. This keeps wildcard imports predictable without restricting low-level access.

## Prelude scope

The prelude exports runtime configuration, common drawing parameters, standard recording backends, and the timestamps users routinely inspect:

```rust
use vision_stimulus_engine::prelude::*;
```

Import specialized APIs from their canonical modules:

```rust
use vision_stimulus_engine::core::{
    absolute_scheduling_verdict, ExternalFramePolicy, InputEvent, SchedulingTrial,
};
use vision_stimulus_engine::data::{DataError, DataWriter};
use vision_stimulus_engine::drawing::{
    FrameRecorder, PipelineBuildCtx, PipelineError, RecordCtx, RegisteredPipeline,
    StimulusPipeline,
};
use vision_stimulus_engine::timing::CalibrationSample;
```

Builders returned by `VSEContext::builder()`, `HeadlessContext::builder()`, and `ExperimentSession::builder()` usually do not need named imports. Public builder types remain available from `core` and `data` when a signature must name them.

Audit 8 removes these names from the prelude without removing their canonical exports:

| Module | Names removed from `prelude` |
|---|---|
| `core` | `absolute_scheduling_verdict`, `AcquisitionMethod`, `ConfirmedFrame`, `DeviceSelector`, `DisplayBackend`, `ExternalFramePolicy`, `HeadlessContextBuilder`, `InputEvent`, `MonitorInfo`, `NamedKey`, `ScanoutFeedback`, `SchedulingTrial`, `SwapchainConfig`, `SwapchainManager`, `VSEContextBuilder`, `VideoModeInfo` |
| `data` | `DataError`, `DataWriter`, `ExperimentSessionBuilder` |
| `drawing` | `FrameRecorder`, `PipelineBuildCtx`, `PipelineError`, `RecordCtx`, `RegisteredPipeline`, `StimulusPipeline` |
| `timing` | `CalibrationSample` |

## Source migration

Audit 8 makes three compile-time changes.

1. Replace `vse.close()` with `vse.request_exit()`. The runtime still finishes the current callback and drains submitted buffered frames.
2. Replace `vse.set_clear(color)` with `vse.set_clear_color(r, g, b, a)`. When the value already exists as a `Color`, destructure it first: `let [r, g, b, a] = color.to_array();`.
3. Add explicit domain imports for names removed from the prelude. Their canonical module paths and behavior did not change.

`RenderContext` remains available as both `vision_stimulus_engine::core::RenderContext` and `vision_stimulus_engine::prelude::RenderContext`. The canonical domain path is `core::RenderContext`; the prelude export is the ordinary experiment convenience path.

## Implementation file boundaries

Audit 8 divides `RenderContext` implementation blocks by recording, external-frame handoff, timing, drawing, custom pipelines, host capture, and input/display control. The public type and method paths remain unchanged.

Two larger splits are deferred because they touch timing-critical implementation code:

- `drawing/renderer.rs` should separate device-free render planning, built-in pipeline construction, command recording, and texture/cache ownership.
- `core/present_timing_ext.rs` should separate Vulkan ABI declarations and layout guards from capability probing, raw device creation, and conformance helpers.

`core/state.rs`, `core/flip.rs`, and `core/event_loop.rs` remain cohesive runtime-domain files. Splitting them now would add module coupling without changing their responsibilities.

The `RenderContext` split adds no runtime dispatch, allocation, synchronization, or presentation-path branch. It does not change whole-crate compilation boundaries. Smaller modules may reduce incremental invalidation, but clean-build time should remain effectively unchanged.
