# Timing conformance

This document is the normative timing contract for VisionStimulusEngine (VSE). Other guides describe APIs and workflows, but they do not establish stronger guarantees than this document.

VSE can preserve frame identity, schedule presentation requests, and report measured timing evidence. It cannot guarantee photon onset from software alone. Treat every timing result according to its source, display path, and recorded runtime capabilities.

## Terms

| Term | Meaning |
|---|---|
| **Backend** | The session-level implementation selected by VSE: `VK_EXT_present_timing`, CPU estimation, or offscreen rendering. |
| **Timestamp source** | The mechanism that produced one frame's `present_time`. This is recorded in `FlipInfo.timing_source`. |
| **Target** | A requested earliest presentation time. A target is an instruction, not evidence that presentation occurred then. |
| **Submission completion** | The GPU fence for the submitted rendering work signaled. This does not prove scanout. |
| **Presentation-engine completion** | `vkWaitForPresent2KHR` reported that the request took effect in the presentation engine or was replaced. The Vulkan specification defines no precise relationship between completion of this wait and presentation to the user. |
| **Scanout timestamp** | A nonzero `IMAGE_FIRST_PIXEL_OUT` time reported for a specific `present_id`, rebased to the session's scanout epoch. On the synchronous EXT path, VSE may instead sample the calibrated present-stage-local clock after a successful present wait. |
| **Photon onset** | Light emitted by the panel. Scanout begin precedes and does not measure panel response, backlight behavior, or pixel visibility. Use a photodiode when photon onset matters. |
| **Confirmed frame** | A runtime-owned submission has been retired and paired with its payload and best available `FlipInfo`. “Confirmed” does not by itself mean scanout-confirmed; inspect `timing_source`. |

## Guaranteed engine invariants

The following are properties of VSE's implementation rather than claims about a driver, compositor, panel, or experiment rig.

1. Each successful synchronous `flip()` returns one `FlipInfo` for that frame.
2. Each successful buffered render callback produces one submission. VSE preserves FIFO payload/submission pairing and drains submitted frames during clean shutdown.
3. `FlipInfo.timing_source` identifies the source of that frame's `present_time`:
   - `ExtPresentTiming` means a scanout-domain timestamp;
   - `CpuEstimate` means a host `CLOCK_MONOTONIC` observation after completion or fallback processing;
   - `Offscreen` means a synthesized timestamp for regeneration.
4. `RenderContext::timing_source()` reports the selected session backend. It can remain `ExtPresentTiming` while an individual fallback receipt reports `CpuEstimate`.
5. A nonzero EXT `present_id` correlates the request with present-timing feedback. Zero means no EXT present identifier was assigned.
6. VSE records the requested `target_time` separately from the observed or estimated `present_time`.
7. VSE never converts scanout timestamps to the host clock on the presentation hot path. The host-clock bridge is opt-in.
8. VSE remains the sole swapchain acquisition, presentation, and `FlipInfo` authority when an external renderer supplies pixels.
9. VSE composites an external underlay first and base-VSE 2D draws afterward in call order.
10. Headless timestamps are always tagged `Offscreen` and cannot be mistaken for measured display timestamps by a reader that checks `timing_source`.

These invariants do not establish that the operating system displayed a frame, that a target was honored, or that panel light changed at `present_time`.

## Timestamp provenance

`submit_time` is always a host-clock timestamp. It cannot be subtracted directly from an `ExtPresentTiming` `present_time`. `timing_source` classifies `present_time`, not `submit_time`.

### `ExtPresentTiming`

`ExtPresentTiming` on a `FlipInfo` means `present_time` is in the session's scanout-clock domain, measured in microseconds since the session scanout epoch.

VSE obtains that value in one of two ways:

- a nonzero `IMAGE_FIRST_PIXEL_OUT` record correlated by `present_id`; or
- on the synchronous path, a calibrated present-stage-local clock sample taken after a successful present wait when per-present feedback is unavailable.

The second mechanism estimates scanout time from the present-stage clock at the end of the wait. It is scanout-domain evidence, but it is not a per-present stage timestamp and inherits the implementation-dependent relationship between `vkWaitForPresent2KHR` completion and presentation.

### `CpuEstimate`

`CpuEstimate` means `present_time` is a host-clock timestamp. It can occur because the EXT backend was unavailable or because a particular EXT frame lacked usable scanout evidence.

A CPU estimate confirms neither scanout nor target attainment. It must not be compared directly with a scanout-domain target or scanout timestamp.

### `Offscreen`

`Offscreen` means no presentation occurred. `present_time` is synthesized as `frame_number × nominal_frame_interval`. `submit_time` measures regeneration work on the host clock.

## Requested and observed presentation timing

`flip(Some(target))` and `BufferedFrame::at(target, payload)` request that presentation not occur before a target. On the EXT path, VSE sends an absolute `VkPresentTimingInfoEXT::targetTime` when the scanout epoch and time-domain identifier are available. Vulkan requires implementations to attempt alignment; the extension does not provide a strict physical-timing guarantee.

Interpret scheduling fields together:

| Evidence | Interpretation |
|---|---|
| `target_time = None` | No explicit target was requested. |
| `target_time = Some(_)`, `timing_source = ExtPresentTiming` | A target was requested and `present_time` is comparable with it. `on_target` reports whether the observed scanout-domain value was at or after the target. |
| `target_time = Some(_)`, `timing_source = CpuEstimate` | A target was requested but this frame has no comparable scanout observation. `on_target` is not evidentiary. |
| `absolute_scheduling_enforced = Some(true)` | A separate, disruptive characterization observed this display path holding unpaced multi-vblank requests to their targets. |
| `absolute_scheduling_enforced = None` | Hardware enforcement was not characterized. Extension advertisement does not fill this gap. |

`on_target` does not prove driver enforcement when synchronous software pacing was enabled. The application may have submitted near the target before the driver saw the request.

## Runtime semantics

### Synchronous displayed runtime

`run()` and `run_with_setup()` use synchronous `flip()` calls.

For a scheduled EXT frame, VSE normally:

1. converts the session-relative target to the present-stage-local domain;
2. waits in software until the target's preceding refresh interval;
3. submits the same target to the driver;
4. waits for rendering completion;
5. uses present-wait where enabled;
6. retrieves per-present feedback or samples the present-stage clock;
7. returns the best supported receipt.

Software pacing reduces dependence on driver target enforcement. It does not turn a target into proof of scanout. `set_software_present_pacing(false)` exists only for characterization and must remain enabled during experiments.

On the CPU backend, a scheduled synchronous flip waits against the host clock before submission and returns `CpuEstimate`.

### Buffered displayed runtime

`run_buffered()` and `run_buffered_with_state()` pipeline rendering and confirmation. With depth one, frame N+1 has already been submitted when confirmation for frame N reaches user code. A confirmation-driven change first affects frame N+2.

Buffered scheduling does not use the synchronous software-pacing wait. VSE submits available EXT targets into the pipelined driver queue. Therefore:

- buffered target timing depends on driver and display-path behavior;
- `absolute_scheduling_enforced` must be characterized on the path used for collection when buffered targets matter;
- per-present `IMAGE_FIRST_PIXEL_OUT` feedback is the only scanout timestamp for a buffered frame;
- absent feedback produces a `CpuEstimate` receipt.

A buffered confirmation means the submission and payload were retired together. Its timing strength comes from `confirmed.flip_info.timing_source`, not from the `ConfirmedFrame` type name.

### Headless runtime

`run_headless()` and `run_headless_with_setup()` render offscreen and block for readback.

Headless behavior has these invariants:

- no window, swapchain, compositor, display, vblank, scanout clock, or input;
- no buffered runtime;
- no waiting for a supplied target;
- synchronous readback after GPU completion;
- `TimingSource::Offscreen` for every frame;
- support for the same drawing commands, registered `StimulusPipeline`s, and compatible external-image rings used by displayed rendering.

Headless regeneration can reproduce bytes on the same machine, driver, format, extent, assets, and deterministic experiment state. Vulkan rasterization and floating-point behavior do not establish byte identity across drivers or GPU vendors.

## Display paths

### Compositor-mediated window

Wayland, X11, borderless fullscreen, and most platform “exclusive fullscreen” APIs can involve a compositor or window-system presentation engine. VSE can request and receive present timing on these paths, but it does not own the final display schedule.

Do not infer compositor bypass from fullscreen state or `ExtPresentTiming`. Record the backend and surface capabilities, and characterize the actual path.

### Direct display

`WindowMode::DirectDisplay` uses `VK_KHR_display` and attempts to acquire a physical display without a compositor in the presentation path. This removes compositor mediation from VSE's swapchain path. It does not guarantee driver target enforcement, panel latency, an awake display, or photon timing.

Direct display is the preferred path for timing-critical data collection after characterization. `ExclusiveFullscreen` is a separate window-system mode and is not synonymous with VSE direct display.

### Offscreen target

A headless target has no display path. Its timing data describes regeneration, not presentation.

## External-frame semantics

An external producer renders pixels but never owns presentation timing. VSE imports the image, waits according to the ring's synchronization mode, composites it, and presents or reads back the result.

`ExternalFramePolicy::FrameLocked` preserves submitted frame identity. The producer must queue the intended slot before the corresponding flip. A slow producer can delay submission and cause a measured miss; VSE does not silently substitute an older frame.

`ExternalFramePolicy::LatestReadyHoldLast` preserves the VSE flip opportunity instead. VSE displays the newest queued producer frame or repeats the pinned frame. A repeat is a content-stream event, not by itself a presentation-timing failure. Record producer frame identifiers and repeat/drop decisions when analysis needs exact scene identity.

GPU semaphore synchronization establishes that producer rendering completed before VSE samples the image. It does not establish when the composite reached scanout.

## `StimulusPipeline` timing obligations

VSE registers a `StimulusPipeline` by running `build` immediately and stores the returned GPU resources. `record` runs while VSE records that frame's active 2D pass.

The engine guarantees typed handle provenance, call-order dispatch, and no hidden pipeline construction during `draw_with`. Experiment code must register pipelines during `run_with_setup`, `run_buffered_with_state` initialization, or headless setup. Calling registration, loading assets, allocating unpredictable resources, or compiling shaders from a per-frame callback can miss a deadline; VSE does not prevent every such call.

A pipeline's `record` implementation must not begin or end VSE's render pass or transition the target image. Its own allocations, descriptor updates, and shader behavior remain the implementer's timing responsibility.

## Misses, skips, and recording

`missed` and `missed_count` are interval-based diagnostics. VSE compares consecutive best-available timestamps with the expected refresh interval and marks durations above 1.5 intervals. They are strongest when based on consecutive scanout feedback and weaker when based on CPU observations. They do not prove that a compositor or panel displayed every intermediate frame.

A skipped frame indicates that VSE submitted no present, commonly because of minimization or swapchain recreation. Skipped receipts use zero timestamps and no present identifier.

Every recorded row carries timing fields even when user payload data is absent. Readers must inspect `timing_source`, `target_time`, display-path metadata, and `HostInfo.timing`; a populated numeric field alone is not a conformance result.

## Capability and conformance evidence

VSE keeps several evidence levels separate:

1. **Advertised device support** records extension and feature availability.
2. **Advertised surface support** records per-surface timing and target capabilities.
3. **Enabled support** records which features and swapchain opt-ins VSE requested successfully.
4. **Passive runtime observation** records whether nonzero feedback appeared.
5. **Active characterization** measures hardware target enforcement with software pacing disabled.
6. **External validation** uses a photodiode or equivalent sensor to measure panel output.

Only the last two levels establish behavior beyond extension advertisement. Active scheduling characterization disrupts normal frame cadence and is not run automatically.

`HostInfo.timing` records advertised and observed properties. Capture it after enough warm-up frames for passive feedback observation. Capture it again after an explicit scheduling characterization if that verdict is needed in the session record.

## Reference-path measurements

The following observations are measurements, not portable guarantees.

On Intel Meteor Lake with ANV/Mesa 26.1 in the 2026-08-03 characterization:

- present-id2 and present-wait2 operated on the tested paths;
- `IMAGE_FIRST_PIXEL_OUT` feedback populated after VSE enabled `VK_SWAPCHAIN_CREATE_PRESENT_TIMING_BIT_EXT`;
- direct display held all tested unpaced three-vblank targets;
- windowed enforcement was intermittent under compositor mediation;
- the present-stage-local and host clocks drifted by approximately 2 ppm in the measured window.

Earlier reports that ANV returned only zero stage timestamps and ignored every target were invalid. VSE had omitted the swapchain present-timing opt-in. Validation VUIDs identified the misuse. Current documentation must not repeat those reports as driver limitations.

Re-run characterization after changing any GPU, driver, kernel, compositor, display connection, present mode, monitor, refresh behavior, or direct-display acquisition path.

## Known limitations and required validation

- Scanout begin is not photon onset. Validate critical rigs with a photodiode recorded on the acquisition clock.
- A sleeping or blanked display can stop producing scanout feedback without a Vulkan error. Disable screen blanking and monitor runtime feedback.
- Variable-refresh behavior can change timing properties during a session.
- Validation layers add presentation-path overhead. Use them for diagnosis, then disable them before collecting timing data. Record forced-layer environment variables in `HostInfo`.
- CPU estimates and scanout timestamps use different epochs and must never be subtracted directly.
- Host input timestamps require the opt-in host-clock bridge before comparison with scanout time.
- External producers can preserve presentation cadence while repeating content. Record both timing and content identity.
- Direct display removes the compositor but does not validate panel electronics.

## Downstream migration notes

Audit 9 makes `FlipInfo.timing_source` strictly per-frame. Existing analysis code must stop treating `ExtPresentTiming` as a session-wide label or inferring the backend from the first recorded row. Use `HostInfo.timing` and runtime metadata for session capability, then group or filter frame data by each row's source.

Other terminology migrations apply to documentation and analysis:

- use **selected backend** for `RenderContext::timing_source()`;
- use **timestamp source** for `FlipInfo.timing_source`;
- use **target request** until comparable scanout evidence exists;
- use **retired** or **payload-correlated** for a buffered confirmation unless its receipt is scanout-domain;
- use **direct display** only for `WindowMode::DirectDisplay`, not generic exclusive fullscreen;
- use **scanout begin** rather than photon onset for `IMAGE_FIRST_PIXEL_OUT`.

Readers must treat a source transition as a clock-domain boundary. VSE does not compute a missed-frame interval across that boundary. Data pipelines should follow the same rule.

## Maintenance rules

When timing behavior changes:

1. update this document first;
2. add or change tests for engine invariants before implementation;
3. keep measured hardware results dated and separate from guarantees;
4. link other guides here instead of restating the contract;
5. verify required Vulkan feature and swapchain opt-ins with validation enabled;
6. rerun behavior probes with validation disabled;
7. record requested, advertised, enabled, observed, and externally measured facts separately.

Related implementation guides:

- [Clock synchronization](clock-synchronization.md)
- [Choosing a runtime](guides/runtimes.md)
- [Buffered flips](guides/buffered_flips.md)
- [Headless rendering](guides/headless.md)
- [External rendering timing](guides/external_rendering_timing.md)
- [Display backends](guides/display_backends.md)
- [Experiment data schema](guides/experiment_data_schema.md)
