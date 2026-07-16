# VSE Timing Roadmap

VSE's primary timing path is `VK_EXT_present_timing` with `VK_KHR_present_id2` correlation and `VK_KHR_present_wait2` pacing. The engine keeps display timing in the scanout-clock domain. `CLOCK_MONOTONIC` bridging is opt-in and used for host-originated events, not for the presentation hot path.

## Current implementation

- `ExtPresentTiming` is the preferred backend when the driver advertises the required extension family.
- `CpuEstimate` is the loud fallback when hardware present timing is unavailable.
- `VK_GOOGLE_display_timing` has been removed.
- `FlipInfo.present_time` is scanout-domain under `ExtPresentTiming` and host-clock fence time under `CpuEstimate`.
- `FlipInfo.present_id`, `target_time`, and `on_target` record present-id correlation and scheduled-present provenance.
- `HostInfo.timing` records advertised extension support plus observed driver behavior, including whether scanout feedback is populated and whether absolute scheduling is enforced.

## Driver conformance work

`VK_EXT_present_timing` is new enough that advertised support may not mean full implementation. On the reference Intel MTL / ANV / Mesa 26.1 stack, present-id2 and present-wait2 work, but past-presentation stage timestamps are zero and absolute `targetTime` scheduling is not enforced. VSE detects those behaviors and falls back to calibrated scanout-clock sampling plus software pacing.

Track future driver and ecosystem changes in `docs/upstream-watch.md`.
