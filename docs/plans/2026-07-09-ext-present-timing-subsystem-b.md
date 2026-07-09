# Pickup Prompt — VK_EXT_present_timing: Subsystem B (Raw Present + Feedback)

**Branch:** `feat/ext-present-timing` (committed through Subsystem A at `58faec6`).
**Read first, in order:** `CLAUDE.md` "Clock Model" · `docs/clock-synchronization.md` (the reframed
model) · `docs/plans/2026-07-09-ext-present-timing-design.md` (has a superseding note at top) ·
`docs/plans/2026-07-09-ext-present-timing-next-session.md` (older; **its Subsystem A framing is
superseded** by the reframe — read Subsystem B and the gotchas there, ignore its "calibrate
present_time onto the CPU clock" framing).

## Where we are (done + verified on Intel MTL / ANV / Mesa 26.1.4)

- **Foundation done:** raw `vkCreateDevice` + `Device::from_handle`, hand-rolled FFI
  (`src/core/present_timing_ext.rs`, rev-3 structs incl. `PresentId2KHR`, `PresentTimingsInfoEXT`,
  `PresentTimingInfoEXT`, `PastPresentationTiming*`, size-asserted), `ExtPresentTimingProvider`
  (real refresh, present-stage sampler), GOOGLE removed, `FlipInfo` has
  `present_id`/`target_time`/`on_target` (placeholders today).
- **Clock model REFRAMED:** the **scanout clock is primary**. Subsystem A built the scanout-clock
  layer + opt-in host bridge: `ScanoutTimestamp`/`ScanoutClock` (`clock.rs`), `HostClockBridge`
  (`timing/bridge.rs`), `ctx.scanout_now()` / `host_to_scanout()` / `scanout_to_host()`,
  `.with_host_clock_bridge()`. HW-verified (`examples/10_host_clock_bridge`). 137 tests pass.
- **Measured:** `PRESENT_STAGE_LOCAL` is a separate clock (~29,714 s offset), drift stable ~2 ppm,
  read noise one-sided → lower-envelope estimator. See `docs/clock-synchronization.md`.
- **Still CPU fence time:** `ExtPresentTimingProvider::record_present_time` returns `clock.now()`;
  `FlipInfo.present_time` is host time. **B makes it real scanout time.**

## Goal of B

Replace vulkano's present with a raw `vkQueuePresentKHR` that (a) attaches
`VkPresentTimingsInfoEXT` (so the driver actually records scanout timing — feedback is **empty**
without it) + `VkPresentId2KHR` (correlation), and (b) reads back real per-frame
`IMAGE_FIRST_PIXEL_OUT` scanout times via `vkGetPastPresentationTimingEXT`. Then
`FlipInfo.present_time` becomes a **scanout-native `ScanoutTimestamp`** (rebased via the
`ScanoutClock` A established), `present_id` correlates frames exactly, and buffered mode keys on
present-id instead of "latest."

## Decomposition (checkpoints — stop/verify at each)

**B1 — Raw present on synchronous `flip()` only, windowed.** Retires the central risk.
- Manual frame loop replacing `SwapchainManager::present`'s `then_swapchain_present`:
  `vkAcquireNextImageKHR` (ash `khr::swapchain::Device`) signalling a VSE-owned
  `vulkano::sync::Semaphore` → `QueueGuard::submit(SubmitInfo)` (in `queue.with(...)`) waiting on
  acquire sem at `COLOR_ATTACHMENT_OUTPUT`, running the renderer's `PrimaryAutoCommandBuffer` via
  `CommandBufferSubmitInfo`, signalling a render-finished `Semaphore` + `Fence` → raw
  `vkQueuePresentKHR` (ash `khr::swapchain::Device::queue_present`, set `p_next` to the chain
  head) waiting on render-finished. Keep the sync objects alive (RAII), reuse per frame-in-flight.
- pNext chain: `PresentId2KHR { present_id = frame_number+1 }` +
  `PresentTimingsInfoEXT → PresentTimingInfoEXT { presentStageQueries = IMAGE_FIRST_PIXEL_OUT |
  IMAGE_FIRST_PIXEL_VISIBLE, targetTime = 0 (unscheduled) }`.
- Re-add the feedback query (`vkGetPastPresentationTimingEXT`, two-call idiom, nested per-record
  stage array, fixed stage capacity ~8, prefer `IMAGE_FIRST_PIXEL_OUT`) on the provider — the
  removed `query_scanouts` in git history is a working reference.
- **Verify (windowed):** present succeeds under validation layers (no errors), feedback returns
  non-empty, `present_id` monotonic and correlates. Scanout times here are compositor-mediated —
  fidelity check is B3/direct-display.
- **Likely home:** a `PresentEngine` (or method on the provider) so `context.rs` stays clean;
  keep the vulkano present as the CPU-fallback path (branch on `source() == ExtPresentTiming`).

**B2 — Buffered path + present-id correlation.** Extend the raw path to
`flip_with_payload()`/`run_buffered()` (multiple frames in flight — trickier semaphore reuse);
make `build_confirmed_flip` (`context.rs`) and the buffered loop key on `present_id` instead of
FIFO/"latest." Missed-frame detection from real per-present scanout deltas.

**B3 — Scanout-native `present_time` + direct-display validation.** Convert the driver's
`PRESENT_STAGE_LOCAL` feedback ns → `ScanoutTimestamp` via the session `ScanoutClock`
(`rebase(stage_ns)`); set it as `FlipInfo.present_time` (schema change: present_time becomes
scanout-domain — decide provenance field vs. reuse `timing_source`). Optionally expose a
host-clock value via the opt-in bridge. **Verify on the direct-display TTY** (real
`IMAGE_FIRST_PIXEL_OUT` at vblank cadence; scheduled flip → `on_target` true).

## Gotchas (these cost real time — read before starting)

- **`CARGO_INCREMENTAL=0` always** for `cargo run --example` (incremental link errors on this tree).
- **Feedback is empty** unless the present attached `VkPresentTimingsInfoEXT` with
  `presentStageQueries` — this is why every windowed/direct run so far returned nothing.
- **vulkano's present won't take a pNext** (`QueueGuard::present_unchecked` has no hook) — hence
  the manual submit+present loop. This is the hard part; budget for validation-layer debugging.
- **Verify present_id2 needs no swapchain-creation struct.** present_id v1 needs none; confirm
  present_id2 is the same against validation. If it does, escalate to raw `vkCreateSwapchainKHR` +
  `Swapchain::from_handle` (confirmed present in vulkano 0.35.2; design doc covers this seam).
- **Direct display needs the `input` group** (`sudo usermod -aG input $USER` + re-login) or Escape
  does nothing. **SIGINT bricks the VT** — always exit via Escape (clean path restores i915).
  Script: `scratchpad/run_direct_display.sh`.
- Raw handles are `repr(transparent)` u64 → wrap into vulkano's ash-0.38 types cleanly.

## Scheduling (once B1/B3 land)

Scheduled flips are now trivially scanout-native: target = `t0 + k·T` in scanout ns (the
`ScanoutClock` epoch + refresh), set `PresentTimingInfoEXT.targetTime` + `timeDomainId` =
present-stage-local id (the provider already stores it as `present_stage_domain_id`),
`present_at_absolute_time` feature is enabled. **No CPU spin, no host-clock conversion** — this is
the reframe payoff. Delete/retire `wait_for_target`'s spin on the EXT path.

## Key files

- `src/core/present_timing_ext.rs` — FFI (all present/feedback structs exist; add raw present helpers)
- `src/core/swapchain.rs` — `SwapchainManager::present`/`submit_nonblocking` (possible `PresentEngine` home)
- `src/timing/provider.rs` — re-add `query_scanouts`; `present_stage_domain_id` already stored
- `src/timing/clock.rs` — `ScanoutClock::rebase` (feedback ns → `ScanoutTimestamp`)
- `src/core/context.rs` — `flip`/`flip_with_payload`/`build_confirmed_flip` integration
- `src/timing/flip_info.rs` — `present_time` becomes scanout-domain in B3

## Verification plan

- Unit/schema: `CARGO_INCREMENTAL=0 cargo test` (137 pass).
- Windowed smoke (B1/B2): present_id2 + non-empty feedback + correlation.
- Direct-display (B3, the real test): `scratchpad/run_direct_display.sh` from a spare TTY —
  per-frame `IMAGE_FIRST_PIXEL_OUT` at real vblank cadence, `present_id` monotonic, scheduled flip
  `on_target` true. Success = a `frames.csv`/parquet with `timing_source=ExtPresentTiming`,
  monotonic `present_id`, and `present_time` tracking vblank (cross-check `refresh`, ideally a
  photodiode).
