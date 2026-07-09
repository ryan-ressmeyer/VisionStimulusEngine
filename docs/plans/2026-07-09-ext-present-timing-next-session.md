# Pickup Prompt — VK_EXT_present_timing: Calibration Subsystem + Raw Present

**Branch:** `feat/ext-present-timing` (not committed; ~20 files changed + new modules)
**Prereqs read:** `docs/plans/2026-07-09-ext-present-timing-design.md` (design + resolved
decisions), `docs/clock-synchronization.md` (the clock model — read this first), `docs/timing.md`.

This is where a prior session left off. The **foundation is built and hardware-verified**; two
subsystems remain to make `FlipInfo.present_time` a true calibrated hardware scanout timestamp
and to schedule presents in hardware.

---

## Where we are (built + verified on this machine: Intel MTL / ANV / Mesa 26.1.4 / i915)

Done and working — **do not redo**:

- **Raw device creation + vulkano adoption** (`src/core/present_timing_ext.rs::create_device_with_present_timing`).
  vulkano 0.35 can't express these Vulkan 1.4 extensions, so we `vkCreateDevice` raw (feature
  structs chained via pNext, sub-features discovered with `vkGetPhysicalDeviceFeatures2`) and
  adopt via `Device::from_handle`. Verified: `Timing backend: ExtPresentTiming (scheduling=true,
  present_wait2=true)`. Enabled extensions: swapchain, dynamic_rendering, ext_present_timing,
  khr_present_id2, khr_present_wait2, ext_calibrated_timestamps.
- **Hand-rolled FFI** (`src/core/present_timing_ext.rs`): rev-3 structs/enums/constants/fn-pointer
  loader + capability probe, ABI-guarded by `const` size asserts. `#![allow(dead_code)]` because
  it's a complete binding surface; some structs await the raw present path.
- **ExtPresentTimingProvider** (`src/timing/provider.rs`): loads fns, sizes the past-timing ring
  (`vkSetSwapchainPresentTimingQueueSizeEXT`), and serves the **real driver refresh** (verified
  16666µs/60Hz via `vkGetSwapchainTimingPropertiesEXT`). Currently `record_present_time` returns
  `clock.now()` (CPU fence time) — the scanout+calibration path was intentionally deferred to
  this session.
- **GOOGLE fully removed.** `TimingSource` = `ExtPresentTiming | CpuEstimate`.
- **`FlipInfo` already carries** `present_id: u64`, `target_time: Option<Timestamp>`,
  `on_target: bool` (+ CSV/Parquet schema). Populated with placeholders today (present_id=0);
  the raw present will fill them for real.
- **`Clock` calibration anchor** (`src/timing/clock.rs`): captures absolute `CLOCK_MONOTONIC` at
  epoch; `to_monotonic_nanos` / `from_monotonic_nanos` convert VSE `Timestamp` ↔ monotonic ns.
  Kept precisely for the calibration subsystem below.
- **Timing capabilities probe** (`src/host/` — `TimingCapabilities` in `HostInfo`): reports
  present-timing family support, calibrateable time domains, and a measured best-of-8
  `Device↔CLOCK_MONOTONIC` `maxDeviation` (~12–40µs here). Confirmed `PRESENT_STAGE_LOCAL` **is
  calibrateable** on this hardware. Verify with `examples/06_host_info` (windowed, no TTY).
- **Swapchain-recreation notification** (`TimingProvider::on_swapchain_recreated`, routed through
  `VSEState::recreate_swapchain`) — fixes a stale-handle UAF that segfaulted the first run.

---

## Goal of this session

1. **Runtime calibration subsystem** — bridge the driver's opaque `PRESENT_STAGE_LOCAL` present
   clock to `CLOCK_MONOTONIC`/VSE `Clock` via `VK_KHR_calibrated_timestamps`.
2. **Raw present path** — replace vulkano's `then_swapchain_present` with a raw
   `vkQueuePresentKHR` carrying `VkPresentTimingsInfoEXT` (scheduling + stage queries) and
   `VkPresentId2KHR` (correlation), so the driver actually records per-frame scanout timing.

End state: `FlipInfo.present_time` = true `IMAGE_FIRST_PIXEL_OUT` scanout time on the VSE clock;
`present_id` correlated; `target_time`/`on_target` meaningful for scheduled flips; buffered mode
correlates by present-id instead of "latest".

---

## Subsystem A — Runtime calibration

**Mechanism** (see `docs/clock-synchronization.md` §3): sample `PRESENT_STAGE_LOCAL` and
`CLOCK_MONOTONIC` together via `vkGetCalibratedTimestampsKHR`, compute
`offset = monotonic_ns − present_stage_ns`, re-sample to track drift.

Steps:
1. **Add the missing FFI struct** to `present_timing_ext.rs`: `VkSwapchainCalibratedTimestampInfoEXT`
   (`sType` **1000208009** — the `STYPE_SWAPCHAIN_CALIBRATED_TIMESTAMP_INFO_EXT` const and the
   struct are NOT yet declared; header layout: `{ sType, *const pNext, VkSwapchainKHR swapchain,
   VkPresentStageFlagsEXT presentStage, uint64_t timeDomainId }`). Add a `const` size assert.
2. Query the swapchain's present-stage-local `timeDomainId` from
   `vkGetSwapchainTimeDomainPropertiesEXT` (the provider already reads the domain list in
   `log_offered_time_domains` — extend it to also return the id).
3. Build the calibration sampler using ash's `ext::calibrated_timestamps::Device` (already used
   in `src/host/capture.rs`). Chain `VkSwapchainCalibratedTimestampInfoEXT` into
   `vk::CalibratedTimestampInfoEXT.p_next` for the present-stage entry (ash struct fields are
   public — set `p_next` raw), and a plain `CLOCK_MONOTONIC` entry.
4. `offset` state + re-sample cadence (start simple: re-sample every ~1s or every N flips; the
   ~20ppm drift math is in the clock-sync doc). Store offset in the provider.
5. `present_stage_ns → Timestamp`: `clock.from_monotonic_nanos(stage_ns + offset)`.

---

## Subsystem B — Raw present

**Why raw:** vulkano 0.35's present has no pNext hook, and the feedback query
(`vkGetPastPresentationTimingEXT`) returns **nothing** unless the present attached
`VkPresentTimingsInfoEXT` with `presentStageQueries` (learned the hard way — feedback was empty
in every windowed/direct run so far). Present-id2 correlation likewise requires attaching
`VkPresentId2KHR` at present.

**Present pNext chain to attach to `VkPresentInfoKHR`:**
- `VkPresentId2KHR { swapchainCount:1, pPresentIds:&present_id }` — `present_id = frame_number+1`
  (u64, monotonic, >0).
- `VkPresentTimingsInfoEXT { swapchainCount:1, pTimingInfos:&VkPresentTimingInfoEXT }` where
  `VkPresentTimingInfoEXT { flags, targetTime, timeDomainId, presentStageQueries, targetTimeDomainPresentStage }`.
  - `presentStageQueries = IMAGE_FIRST_PIXEL_OUT | IMAGE_FIRST_PIXEL_VISIBLE` (request those
    timestamps).
  - For an unscheduled flip: `targetTime=0`. For a scheduled flip: convert the CPU target →
    present-stage-local via the calibration offset, set `timeDomainId` = present-stage-local id,
    `flags` per absolute/relative (`presentAtAbsoluteTime` feature is enabled).
  - All the structs/constants/fn-pointers exist in `present_timing_ext.rs` EXCEPT `vkQueuePresentKHR`
    itself (use ash 0.38's `khr::swapchain::Device::queue_present` — build `vk::PresentInfoKHR`,
    set its public `p_next` to the chain head; ash passes it through).

**The hard part — semaphores.** vulkano's `then_swapchain_present` owns the wait semaphore, and
`QueueGuard::present_unchecked` won't take a pNext. So the EXT path needs a manual frame loop:
- `vkAcquireNextImageKHR` (ash `khr::swapchain::Device`) signalling a VSE-owned
  `vulkano::sync::Semaphore` (pass its `.handle()`), get image index.
- `QueueGuard::submit` (vulkano low-level, in `queue.with(...)`) with a `SubmitInfo` that waits on
  the acquire semaphore at `COLOR_ATTACHMENT_OUTPUT`, executes the renderer's
  `PrimaryAutoCommandBuffer` (via `CommandBufferSubmitInfo`), signals a VSE-owned render-finished
  semaphore + a `Fence`.
- Raw `vkQueuePresentKHR` waiting on the render-finished semaphore, with the pNext chain above.
- Keep the vulkano `Semaphore`/`Fence` objects alive (RAII); reuse per-frame-in-flight.

Integrate as an alternate path in `RenderContext::flip` and `flip_with_payload`
(`src/core/context.rs` ~1700 and ~1857): when the provider is `ExtPresentTimingProvider`, take
the raw path; otherwise keep today's vulkano present. Consider moving the raw present into the
provider or a `PresentEngine` so `context.rs` stays clean. Then `build_confirmed_flip`
(`context.rs:483`) and the buffered loop key on `present_id` for confirmation.

**Verify present_id2 doesn't need a swapchain-creation-time struct.** We create the swapchain via
vulkano. present_id v1 needs none; present_id2 *should* be the same, but confirm against the spec
/ validation layers. If it does need one, escalate to raw swapchain creation + `Swapchain::from_handle`
(confirmed present in vulkano 0.35.2; the design doc covers this seam).

---

## Gotchas / lessons (read before starting — these cost real time)

- **Incremental build is broken on this tree.** `cargo run`/`build` with incremental produces
  `undefined hidden symbol: anon.*.llvm` link errors after heavy churn. **Always use
  `CARGO_INCREMENTAL=0`** (or `rm -rf target/debug/incremental`). Tests/`cargo check --lib` are
  fine; it bites `cargo run --example`.
- **Direct display needs the `input` group.** Without it evdev finds no devices → Escape does
  nothing → you must Ctrl+C. Run `sudo usermod -aG input $USER` and re-login first.
- **SIGINT does not restore the VT** (bricks the TTY). The clean Escape-exit path restores fine
  (i915). Either always exit via Escape, or (nice-to-have) fix SIGINT to run the restoration in
  `context.rs` ~944. Validation script: `scratchpad/run_direct_display.sh`.
- **No `CLOCK_MONOTONIC` swapchain domain here.** ANV offers only `PRESENT_STAGE_LOCAL`
  (`[1000208000]`) in both windowed and direct display. This is expected — calibration is the
  intended path. Do not reintroduce a native-MONOTONIC dependency.
- **`vkGetPastPresentationTimingEXT` is a two-call idiom** with a nested per-record stage array;
  the prior (now-removed) `query_scanouts` in git history shows a working version (fixed stage
  capacity of 8, prefer `IMAGE_FIRST_PIXEL_OUT`). Re-add it for the confirmation path.
- **vulkano can't see the 1.4 extensions** — `supported_extensions()` drops them; probe raw
  (`probe_support`). Handles are `repr(transparent)` u64, so raw handles wrap into vulkano's
  ash-0.38 types cleanly (this is why hand-rolled FFI beat pulling ash-git).
- **ABI layouts** are transcribed from `Vulkan-Headers vulkan_core.h` (spec v3), guarded by
  `const` size asserts. Keep that discipline for the new struct.

---

## Verification plan

- **Unit/schema:** `CARGO_INCREMENTAL=0 cargo test` (129 pass currently).
- **Windowed smoke:** `examples/06_host_info` (probe) and `examples/01_timing_validation`
  (source=ExtPresentTiming, refresh). Windowed will exercise present_id2 + feedback correlation
  even though scanout timestamps there are compositor-mediated.
- **Direct display (the real test):** from a spare TTY (Ctrl+Alt+F3, after joining `input`),
  run `scratchpad/run_direct_display.sh`. Confirm: real per-frame `IMAGE_FIRST_PIXEL_OUT` scanout
  times, calibrated onto the CPU clock, present_id correlation, and — for a scheduled flip —
  `on_target` true. This is where calibrated scanout timing can actually be validated.
- **Success = ** a recorded run whose `frames.csv`/parquet shows `timing_source=ExtPresentTiming`
  with `present_id` monotonic and `present_time` values that track vblank at the display's real
  cadence (cross-check against `refresh` and, ideally, a photodiode).

## Key files

- `src/core/present_timing_ext.rs` — FFI (add `SwapchainCalibratedTimestampInfoEXT`; raw present helpers)
- `src/timing/provider.rs` — `ExtPresentTimingProvider` (calibration state; re-add scanout query)
- `src/core/context.rs` — `flip`/`flip_with_payload`/`build_confirmed_flip` (raw present integration)
- `src/core/swapchain.rs` — possible home for the raw present/submit helper
- `src/timing/clock.rs` — `from_monotonic_nanos` (calibration conversion)
- `src/host/capture.rs` — calibrated-timestamps usage pattern to copy
- `docs/clock-synchronization.md` — the model; keep it in sync with the implementation
