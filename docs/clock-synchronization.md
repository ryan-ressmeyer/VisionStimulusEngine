# Clock synchronization

VSE keeps displayed timing in the display's present-stage-local clock. The normative presentation and fallback contract is [Timing conformance](timing-conformance.md). This document describes the optional bridge between scanout time and the host clock.

## Clock roles

| Clock | Role |
|---|---|
| Present-stage-local | Native domain for scanout timing and EXT target requests. |
| `CLOCK_MONOTONIC` | Host input, network, and process event timestamps. |
| GPU device clock | GPU timestamp-query domain; useful for diagnostics but not a scanout timestamp. |
| Acquisition clock | Ephys or DAQ timebase, usually on separate hardware. |

VSE establishes a session-relative scanout epoch from the present-stage-local clock. `FlipInfo.present_time` is in that domain only when `FlipInfo.timing_source == TimingSource::ExtPresentTiming`.

CPU estimates remain in the host-clock domain. Do not subtract them from scanout timestamps.

## Acquisition alignment

The acquisition system should observe a physical event. Place a photodiode over a stimulus patch and feed it to the acquisition ADC. The diode records panel output in the acquisition clock without requiring VSE to estimate that clock.

`IMAGE_FIRST_PIXEL_OUT` identifies scanout begin, not emitted light. Pixel response, display processing, rolling scanout, and backlight timing remain outside the software timestamp.

## Why a bridge is optional

Host events arrive in `CLOCK_MONOTONIC`. Experiments that need to compare a key press or network event with scanout can enable the bridge:

```rust,ignore
let context = VSEContext::builder()
    .with_host_clock_bridge()
    .build()?;
```

The bridge is not required when display timing stays in scanout time and a photodiode provides acquisition alignment. It is not part of presentation scheduling.

## Calibration samples

`VK_KHR_calibrated_timestamps` can sample the present-stage-local clock and `CLOCK_MONOTONIC` close together. One sample contains:

- `stage_ns`, an absolute present-stage-local reading;
- `mono_ns`, an absolute host-clock reading;
- `max_deviation_ns`, the driver's bound on the separation between those reads.

The instantaneous offset is:

```text
offset = mono_ns - stage_ns
```

The clocks run from different oscillators, so a fixed offset accumulates error. VSE's `HostClockBridge` fits offset and relative drift over a sliding window. Sampling is rate-limited and occurs outside the presentation hot path.

## Public conversions

After the bridge has warmed up and the scanout epoch exists:

- `host_to_scanout(timestamp)` maps a host event into session-relative scanout time;
- `scanout_to_host(timestamp)` maps scanout time into VSE's host-clock timeline;
- `host_clock_bridge_drift_ppm()` reports the current fitted drift;
- `sample_present_calibration()` exposes raw paired samples for diagnostics.

These methods return `None` when the EXT backend, calibrated timestamp support, scanout epoch, or warmed bridge is unavailable. Headless sessions always return `None` for scanout-clock operations.

## Error terms

### Calibration read deviation

`max_deviation_ns` bounds how far apart the paired clock reads may have occurred. It is a sampling bound, not a panel-timing error and not necessarily the error of a scanout feedback timestamp.

### Inter-sample drift

Relative oscillator drift accumulates between samples. The sliding fit tracks it rather than freezing a single offset.

### Presentation evidence

A successful present wait is not itself a scanout timestamp. The Vulkan specification permits implementation-dependent delay and replacement behavior. Per-present `IMAGE_FIRST_PIXEL_OUT` feedback provides stronger evidence. The synchronous fallback samples the present-stage clock after a successful wait and inherits that wait's uncertainty.

### Panel behavior

Panel light output dominates the end-to-end uncertainty that software cannot observe. Measure it with a photodiode.

## Reference measurement

On the Intel Meteor Lake / ANV / Mesa 26.1 reference system measured in 2026-07:

- present-stage-local had a distinct epoch from `CLOCK_MONOTONIC`;
- relative drift was approximately 1.97 ppm over 120 seconds;
- paired-read noise was one-sided, so a lower-envelope fit was more stable than an average.

These measurements motivated the sliding lower-envelope model. They are not portable performance guarantees. Repeat `examples/10_present_timing_internals drift` on the recording path after hardware or driver changes.

## Driver and path characterization

Advertised clock domains and timing features are claims. Record the surface capabilities, observe nonzero feedback, and run disruptive scheduling characterization separately when required. The corrected reference-path history and current measurement policy are in [Timing conformance](timing-conformance.md#capability-and-conformance-evidence).

Primary references:

- [VK_EXT_present_timing](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_present_timing.html)
- [VkPresentTimingInfoEXT](https://docs.vulkan.org/refpages/latest/refpages/source/VkPresentTimingInfoEXT.html)
- [VK_KHR_present_wait2](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_present_wait2.html)
- [vkGetCalibratedTimestampsKHR](https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetCalibratedTimestampsKHR.html)
