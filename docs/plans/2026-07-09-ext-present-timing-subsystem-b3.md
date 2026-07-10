# Pickup Prompt — VK_EXT_present_timing: Subsystem B3 (Scanout-native present_time + scheduling + direct-display)

**Branch:** `feat/ext-present-timing`. **B1 committed at `8eded9f`, B2 at `e2adf20`.** Tree is clean
and green at B2 (111 lib tests + 19 + 12 pass).
**Read first, in order:** `CLAUDE.md` "Clock Model" · `docs/clock-synchronization.md` ·
`docs/plans/2026-07-09-ext-present-timing-subsystem-b.md` (the full B plan) · this file.

## Where we are (done + HW-verified on jiji: Intel MTL / ANV / Mesa, Wayland)

- **B1 (`8eded9f`):** raw `vkQueuePresentKHR` on the **synchronous `flip()`** path, attaching
  `VkPresentId2KHR` + `VkPresentTimingsInfoEXT`, plus the `vkGetPastPresentationTimingEXT` feedback
  read. New `src/core/present_engine.rs` (`PresentEngine`: raw acquire/submit/present + a
  `image_count+1` sync-object ring). Pure TDD'd seams in `present_timing_ext.rs`: `PresentChain`
  (heap-pinned pNext chain, requests all 4 stages via `REQUESTED_PRESENT_STAGES`) +
  `ScanoutFeedback`/`feedback_from_record`. `provider.query_scanouts()` (two-call idiom, stage
  cap 8). `examples/11_raw_present_feedback` PASSES. `FlipInfo.present_id` is now the real
  `VkPresentId2`.
- **B2 (`e2adf20`):** raw non-blocking present on the **buffered** path
  (`flip_with_payload_ext`, `run_buffered`). `PresentOutcome` returns the slot `Arc<Fence>`;
  `EngineInFlight` wraps it as the `InFlightFuture`; `PresentEngine::ensure_ring` grows the ring to
  `images+1`. Present-id correlation via `VSEState::scanout_by_present_id` (fed by
  `ingest_scanout_feedback`, pruned by `take_scanout_for`). `examples/12_buffered_present_id`
  PASSES depth 1/2/3.
- **`present_time` is STILL CPU fence time on both paths.** B3 makes it scanout-native.

## Two decisions already made (carry these forward)

1. **Schema:** `FlipInfo.present_time` (type stays `Timestamp`) becomes **scanout-domain µs** under
   `ExtPresentTiming` (rebase the driver's `IMAGE_FIRST_PIXEL_OUT` present-stage-local ns via the
   session `ScanoutClock`, then `.as_micros()`), and stays **host CPU µs** under `CpuEstimate`.
   The **only** domain guard is `timing_source` — do NOT add a new field. Data writers/examples
   read `present_time.as_micros()` and keep working. Update docs to match (they already say
   present_time is scanout).
2. **present-wait2 → do it PROPERLY with a raw swapchain.** See next section.

## The present-wait2 finding (the crux of B3)

`vkWaitForPresent2KHR` is the clean primitive to block until a specific present is displayed, so
synchronous `flip()` can read *this frame's* real scanout time (the feedback record lands ~1 vblank
after present). **present_wait2 is enabled** on this HW (`examples/06_host_info` → `present_wait2:
true`), the FFI struct/sType/fn-ptr are all correct — **but calling it segfaults inside
`libvulkan_intel.so`.** Backtrace bottoms out 3 frames into the driver from
`ExtPresentTimingProvider::wait_for_present`. **Inference (not yet proven):** present-wait2 requires
per-swapchain opt-in that vulkano's swapchain doesn't provide — the header defines
`VkSurfaceCapabilitiesPresentWait2KHR` (a surface-capability struct), implying the swapchain must be
created with present-wait2 support explicitly enabled. present_id2, by contrast, needs **no**
creation-time opt-in and already works.

**Decision: create the swapchain ourselves** with raw `vkCreateSwapchainKHR` (chaining the
present-wait2 / present-id2 opt-in) + `vulkano::swapchain::Swapchain::from_handle` (confirmed present
in vulkano 0.35.2; the design doc covers this seam). This mirrors the raw-`vkCreateDevice` work
already in `present_timing_ext.rs`.

## B3 decomposition (checkpoints — stop/verify at each)

**B3-raw-swapchain (the big, exploratory piece — do first).**
- **Confirm the requirement:** query `VkGetPhysicalDeviceSurfaceCapabilities2KHR` chaining
  `VkSurfaceCapabilitiesPresentWait2KHR` to learn what present-wait2 needs; read the
  VK_KHR_present_wait2 proposal. Determine the exact swapchain-creation opt-in struct(s) (likely a
  `VkSwapchainPresentModesCreateInfoKHR` / a present-wait2 enable — nail this down before coding).
- **FFI + creation:** hand-roll `VkSwapchainCreateInfoKHR` with the opt-in chained, call raw
  `vkCreateSwapchainKHR`, adopt via `Swapchain::from_handle`. Home: extend `SwapchainManager`
  (`src/core/swapchain.rs`, currently 100% vulkano-owned) — probably a raw-create path used when the
  device has present-wait2, else the existing `Swapchain::new`.
- **Recreation:** route `SwapchainManager::create_swapchain` (called on every resize/OUT_OF_DATE)
  through the raw path too, or the opt-in is lost after the first resize.
- **Verify:** `vkWaitForPresent2KHR(present_id)` no longer segfaults; returns SUCCESS; existing
  windowed examples (11/12) still PASS with the raw swapchain.

**B3-scanout-present_time.** With present-wait2 working:
- `flip_ext` (sync): after `submit_and_present`, `wait_for_present(present_id, ~250ms)` → drain
  `query_scanouts` → `present_time = scanout_present_time(first_pixel_out_ns)` (rebase via
  `ScanoutClock`, → `Timestamp::from_micros`). Fall back to fence + CPU if it times out.
- `build_confirmed_flip` (buffered): key on `present_id` — the frame confirmed `depth+1` later has
  its scanout available in `scanout_by_present_id`; set `present_time` from it, missed-detection
  from scanout deltas (the guarded-consecutive logic is already drafted in the stash).
- **Note:** most of this is DRAFTED in `git stash@{0}` ("WIP B3…") — `scanout_present_time` helper,
  the `build_confirmed_flip` rework, `provider.wait_for_present`, and example-11 present_time
  tracking. **`git stash show -p stash@{0}`** to recover; it does NOT compile (it calls an
  `await_scanout` poll helper that was never written — that whole poll approach is superseded by
  present-wait2, so drop it and use `wait_for_present` instead).
- **Verify (windowed):** `present_time` monotonic with ~16.67 ms deltas; correlates with feedback.

**B3-scheduling.** Scheduled flips become trivially scanout-native (the reframe payoff):
- `PresentChain` gains a `scheduled(present_id, target_time_ns, time_domain_id)` ctor setting
  `PresentTimingInfoEXT.targetTime` (absolute scanout ns) + `time_domain_id`
  (`provider.present_stage_domain_id`), flags=0 (absolute; `present_at_absolute_time` feature is
  enabled). Engine `submit_and_present` takes an optional target.
- `flip(Some(target))` / `flip_with_payload(Some(target))` on EXT: interpret `target` as
  scanout-domain µs (consistent with present_time), convert to absolute ns
  (`scanout_clock.epoch_stage_ns + target_us*1000`), set as `targetTime`. **Delete the CPU spin**
  (`wait_for_target`) on the EXT path. Compute `on_target` from confirmed scanout ≥ target.
- **Verify:** scheduled flip lands at/after target; `on_target` true.

**B3-direct-display (the real test).** Run on a spare TTY via `scratchpad/run_direct_display.sh`
(needs the `input` group; **exit via Escape only — SIGINT bricks the VT**). Verify real
`IMAGE_FIRST_PIXEL_OUT` at true vblank cadence (windowed is compositor-jittered ~42–51 Hz — that is
PRE-EXISTING, not a bug), monotonic `present_id`, scheduled `on_target` true. Success = a
`frames.csv`/parquet with `timing_source=ExtPresentTiming`, monotonic `present_id`, `present_time`
tracking vblank; ideally a photodiode cross-check.

## Gotchas (cost real time — read before starting)

- **`CARGO_INCREMENTAL=0` always** for `cargo run --example`.
- **`vkGetPastPresentationTimingEXT` is a destructive dequeue** — drain once per frame and cache
  (`VSEState::recent_scanouts` + `scanout_by_present_id`). Double-draining returns nothing.
- **Feedback lags ~1 vblank** past present — that is *why* present-wait2 (or the abandoned poll) is
  needed on the sync path; the buffered path gets it for free `depth+1` frames later.
- **The fence-reset lives in `submit_and_present`, not `acquire_next`** (a B2 deadlock fix) — an
  acquire failure must leave the fence signalled or `wait_idle` deadlocks. Keep it that way.
- **Wayland returns `OUT_OF_DATE` on the first frame** (surface resizes post-creation) — both flip
  paths recreate + skip; the raw-swapchain path must handle this too.
- **No system Khronos validation layer** on jiji (only a Steam copy, Vulkan header 1.3.284 — too old,
  emits false-positive `VkDeviceCreateInfo-pNext-pNext` on the 1.4 feature structs). Manifest at
  `scratchpad/VkLayer_khronos_validation.json`; `VK_LAYER_PATH=<scratchpad>
  VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`. It cannot meaningfully validate the 1.4 path.

## Key files

- `src/core/swapchain.rs` — `SwapchainManager` (add raw `vkCreateSwapchainKHR` path here)
- `src/core/present_timing_ext.rs` — FFI (add swapchain-create opt-in struct(s); `PresentChain::scheduled`)
- `src/timing/provider.rs` — `wait_for_present` (in stash); `present_stage_domain_id` already stored
- `src/core/present_engine.rs` — `submit_and_present` (add optional target)
- `src/core/context.rs` — `flip_ext` / `flip_with_payload_ext` / `build_confirmed_flip`;
  `scanout_present_time` (in stash)
- `src/timing/flip_info.rs` — `present_time` semantics doc (now scanout-domain on EXT)

## Verification plan

- Unit/schema: `CARGO_INCREMENTAL=0 cargo test`.
- Windowed smoke: `examples/11` (sync) + `examples/12` (buffered) — present_time now scanout µs,
  monotonic, ~vblank deltas; present_id monotonic + correlates.
- Direct-display (the real test): `scratchpad/run_direct_display.sh` from a spare TTY.

## Recovering the stash

`git stash list` → `stash@{0}` = "WIP B3…". `git stash show -p stash@{0}` to view;
`git stash apply stash@{0}` to restore (then fix: replace the `await_scanout` poll call in
`flip_ext` with `wait_for_present` once the raw swapchain makes it safe). It touches
`examples/11_raw_present_feedback.rs`, `src/core/context.rs`, `src/timing/provider.rs`.
