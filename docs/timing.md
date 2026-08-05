# Timing in VisionStimulusEngine

VSE is designed to preserve frame identity and report the strongest timing evidence available for each frame. The normative contract is [Timing conformance](timing-conformance.md). This page introduces the model without redefining its guarantees.

## The relevant events

A frame passes through several events:

1. the CPU submits rendering work;
2. the GPU finishes rendering;
3. the presentation engine accepts or applies the request;
4. the display controller begins scanout;
5. the panel emits light.

These events are not interchangeable. VSE prefers `IMAGE_FIRST_PIXEL_OUT`, which identifies scanout begin. A GPU fence supplies only a host-side completion estimate. Panel output requires physical measurement.

## Clock model

Displayed timing lives in the display's present-stage-local clock. VSE establishes a session-relative scanout epoch and keeps scheduling and scanout observations in that domain. Host-originated events use `CLOCK_MONOTONIC`; an opt-in calibration bridge maps those events to or from scanout time.

A photodiode connected to the acquisition system remains the standard way to relate stimulus light output to an ephys or DAQ clock. See [Clock synchronization](clock-synchronization.md).

## Timing backends and per-frame evidence

VSE selects one backend for a displayed session:

- `VK_EXT_present_timing`, with present-id2 correlation and present-wait2 where available;
- CPU estimation when the EXT family is unavailable.

The selected backend and a frame's timestamp source are separate facts. `RenderContext::timing_source()` reports the backend. `FlipInfo.timing_source` reports whether that frame's `present_time` is scanout-domain, a CPU estimate, or an offscreen synthetic value.

VSE records fallback provenance rather than presenting every numeric timestamp as equivalent evidence.

## Scheduling

A target is a requested earliest presentation time, not a guarantee. Synchronous EXT flips normally combine software pacing with a driver target. Buffered flips submit targets into the pipelined driver queue without the synchronous pacing wait. Hardware enforcement must be characterized on the display path used for collection.

## Display and offscreen paths

A compositor-mediated window can provide present-timing feedback while the compositor still controls final presentation. VSE direct display removes that compositor from the swapchain path. It does not measure panel light output.

Headless rendering performs no presentation. Its frame times are synthesized and tagged `TimingSource::Offscreen` for regeneration and pixel testing.

## Next steps

- Read [Timing conformance](timing-conformance.md) before interpreting timing data.
- Choose a runtime with [Choosing a runtime](guides/runtimes.md).
- Characterize a rig with examples 10–13 in the [example curriculum](../examples/README.md).
- Use a photodiode for experiments whose analysis depends on photon onset.
