# VK_EXT_present_timing Reorganization — Design Document

**Date:** 2026-07-09
**Status:** Approved — in implementation (branch `feat/ext-present-timing`)

> **Superseded framing (2026-07-09, later same day):** the "One clock domain = everything
> calibrated to `CLOCK_MONOTONIC`" goal below is **reframed**. The **scanout clock is primary**;
> VSE lives in it for all display timing. Alignment to acquisition hardware is physical
> (photodiode + ADC in the acquisition clock), and the scanout ↔ `CLOCK_MONOTONIC` calibration is
> an **opt-in bridge** for host-originated events only — not the backbone, and off the hot path.
> `FlipInfo.present_time` is scanout-native by default. See `docs/clock-synchronization.md` and
> the `CLAUDE.md` "Clock Model" section. Read the goal below in that light.
**Scope:** Make `VK_EXT_present_timing` the primary presentation-timing mechanism —
hardware-scheduled presents, hardware scanout timestamps, and exact present-id
correlation — calibrated into a single clock domain for reproducible, deterministic
stimulus onset. Remove the `VK_GOOGLE_display_timing` path entirely. Fall back loudly to
CPU estimation on hardware that lacks the extension.

---

## Problem Statement

The timing system does not currently do what the documentation claims, and the arrival of
a shipping `VK_EXT_present_timing` is the moment to fix that rather than paper over it.

**1. "Scheduled presents" are a CPU spin-wait — even on the GOOGLE backend.**
Both `flip()` (`src/core/context.rs:1732`) and `flip_with_payload()` (`context.rs:1906`)
implement `target_time` by calling `timing_provider.wait_for_target()`, which is a
`std::hint::spin_loop()` on the CPU clock in *both* providers (`provider.rs:82`,
`provider.rs:233`). `VkPresentTimeGOOGLE` is never attached to a present. The GOOGLE
backend only ever did *feedback* (querying past timings), never *scheduling*. This is
exactly the CPU-burning spin-wait that `docs/timing.md` §6 sells VSE as an improvement
over. That improvement does not exist yet.

**2. Present-id correlation is broken.** `confirmed_present_time_for()`
(`provider.rs:242-261`) maps `frame_number → present_id`, but nothing attaches a present-id
to any present, so the driver records `presentID = 0` for every frame and the lookup always
falls through to "most recent timing." Frames are not actually correlated to their own
hardware timestamps — a hazard for the buffered pipeline, which assumes per-frame
confirmation.

**3. Clock epochs are mismatched.** `record_present_time()` (`provider.rs:217-231`) takes a
driver timestamp that lives in a *device-specific epoch* and does
`Timestamp::from_micros(nanos / 1_000)`, then compares it against `Clock`-epoch timestamps
built from `Instant` (`clock.rs:23`). Inter-frame *differences* survive (the epoch cancels),
but *absolute* present times are not in the same domain as `submit_time` or anything a
neural-recording system would log. The code comment admits this.

`VK_EXT_present_timing` has shipped and is verified on this machine (Intel ANV, Mesa
26.1.4, revision 3). It provides real hardware scheduling, real per-present scanout
timestamps, and — via calibrated timestamps — a path to put every timestamp in one clock
domain. This is the opportunity to build the scheduling half of the timing system correctly
for the first time.

---

## Goals

- **EXT as the primary path:** hardware-scheduled presentation (`targetPresentTime`
  attached to the present, no CPU spin), hardware scanout timestamps, exact per-present
  correlation via `VK_KHR_present_id2`.
- **One clock domain:** ~~calibrate hardware scanout timestamps ↔ VSE `Clock` ↔
  `CLOCK_MONOTONIC` so `submit_time`, `present_time`, and external ephys hardware timestamps are
  all directly comparable.~~ **Reframed (see superseding note above):** the one domain is the
  **scanout clock**, which VSE lives in natively; `CLOCK_MONOTONIC` comparability is an opt-in
  bridge for host events, and acquisition alignment is done physically (photodiode + ADC).
- **Deterministic onset:** a stimulus scheduled for frame *N* is held by hardware until its
  target and reported back with the scanout time it actually achieved — independent of CPU
  scheduler jitter.
- **Loud two-tier fallback:** `ExtPresentTiming` when available, else `CpuEstimate` with an
  explicit warning written into every record. No silent degradation.
- **Stable public surface:** keep `run()`, `run_buffered()`, `flip()`, `flip_with_payload()`,
  and `FlipInfo` working; changes are additive where possible.

---

## Non-Goals

- Keeping `VK_GOOGLE_display_timing`. It is removed, not demoted (per the cross-platform
  rationale).
- A `present_wait2` verification tier. present_wait2 is adopted only as a *pacing
  primitive* (see below), not as a `TimingSource`.
- Forking vulkano (unless the adoption path below proves untenable).
- VRR / adaptive-sync scheduling beyond fixed absolute targets (future work).
- Windows / macOS validation in this cycle. The design stays cross-platform (calibrated
  timestamps abstract the OS clock domain); verification happens on Linux/ANV first.

---

## Locked Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | **Hand-rolled minimal FFI** for the extension, not `ash` git. | Avoids compiling two `ash` versions. The extension structs are declared locally; raw `u64` handles are wrapped into vulkano's `ash`-0.38 handle types (see the vulkano boundary below). |
| 2 | **Two-tier loud fallback:** `ExtPresentTiming \| CpuEstimate`. | Cross-platform posture; GOOGLE removed. A CPU-estimate run must never look like a hardware-verified one. |
| 3 | **present_wait2 = pacing primitive, not a tier.** | It corrects the *anchor* of a CPU timestamp (present vs. render-completion) and enables spin-free backpressure, but gives no hardware scanout time — no verification gain worth a tier. Usable in both paths. |

---

## The Central Constraint: the vulkano 0.35 boundary

This is the crux of the reorg, and it is larger than the `ash` bindings question.

vulkano 0.35.2 is generated against **Vulkan 1.3.281**. Its typed `DeviceExtensions` knows
`khr_present_id` / `khr_present_wait` (v1) and `{khr,ext}_calibrated_timestamps`, but has
**no field** for the three extensions we need — `ext_present_timing`, `khr_present_id2`,
`khr_present_wait2` — because they are Vulkan 1.4 (1.4.335). And the finalized
`VK_EXT_present_timing` depends specifically on `present_id2`, so the v1 fields vulkano does
expose cannot substitute.

The consequence: vulkano cannot enable these extensions, cannot chain their structs into
swapchain creation, and cannot chain them into present. **Raw control is required at three
seams**, with vulkano objects adopted afterward via their `unsafe from_handle` constructors:

| Seam | Raw call + pNext | Adopt into vulkano |
|------|------------------|--------------------|
| Device creation | `vkCreateDevice` with the extension name strings + `VkPhysicalDevicePresentTimingFeaturesEXT` / present-id2 feature structs in pNext | `Device::from_handle` (`vulkano/src/device/mod.rs:391`) |
| Swapchain creation | `vkCreateSwapchainKHR` with `VkSwapchainPresentTimingCreateInfoEXT` (+ present-id2 enablement) in pNext; then `vkGetSwapchainImagesKHR` | `Swapchain::from_handle` (`vulkano/src/swapchain/mod.rs:1042`) |
| Present + feedback | `vkQueuePresentKHR` with `VkPresentId2KHR` + `VkSwapchainPresentTimingInfoEXT` in pNext; `vkGetPastPresentationTimingEXT`; optional `vkWaitForPresent2KHR` | n/a (raw) |

**Why this vindicates the hand-rolled-FFI decision:** `from_handle` takes `ash::vk::Device`
/ `ash::vk::SwapchainKHR` — vulkano's `ash`-0.38 handle types. `ash` handles are
`#[repr(transparent)]` over `u64`, so our FFI mints raw `u64` handles and wraps them into
the 0.38 types with zero risk of a second `ash` version entering the tree. Pulling `ash`
git would instead give us handle types *incompatible* with vulkano's, forcing transmutes
across the version boundary at every seam.

**Capability probing must also be raw.** vulkano's `supported_extensions()` parses
`vkEnumerateDeviceExtensionProperties` into its typed struct and silently drops names it
doesn't know — so it will never report `VK_EXT_present_timing`. The FFI module must call
`vkEnumerateDeviceExtensionProperties` itself and string-match the three names (replacing
`device.rs:435 supports_google_display_timing`).

**Two creation paths.** The EXT path (raw create + adopt) and the fallback path
(vulkano-native `Device::new` / `Swapchain::new`, unchanged) diverge at init. A startup
capability probe selects one. Divergence is contained in a single `present_timing_ext`
module; the fallback path keeps today's code.

> **Decision A — RESOLVED (2026-07-09): raw-create-and-adopt.** No vulkano fork. Raw
> `vkCreateDevice` / `vkCreateSwapchainKHR` on the EXT path, adopted via `from_handle`. Fork
> stays the escape hatch only if `from_handle`'s invariants (image-handle ordering,
> `create_info` must match) prove brittle under validation layers.

---

## Architecture

### Windowed vs. direct display

`VK_EXT_present_timing` is a **swapchain** feature and works in *both* windowed/composited
and direct-display modes — being compositor-aware was the whole point of its ~5-year
standardization. It is not restricted to direct display. But timing *fidelity and
determinism* differ, and VSE should surface the difference rather than hide it:

- **Direct display** (`VK_KHR_display`, VSE's `initialize_direct` path): no compositor. The
  swapchain scans out straight to the display controller, so
  `VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT` is the true hardware scanout of *your*
  content and scheduled targets are honored by display hardware directly. Highest fidelity
  and determinism — **the recommended path for real experiments.**
- **Windowed / composited** (Wayland or X11 with a compositor, VSE's `initialize_compositor`
  path): each present hands the image to the compositor, which composites and presents at
  its own cadence. Timing feedback still comes back hardware-anchored, but it reflects when
  the *compositor's* frame (containing your content) reached the screen — one more pipeline
  stage of latency, and exact target scheduling depends on the compositor honoring it (on
  Wayland via the presentation-time / commit-timing / FIFO protocols, which modern
  Mutter/KWin/wlroots support). A fullscreen, unoccluded window often gets a direct
  scanout/flip path that approaches direct-display fidelity — but that is a compositor
  optimization, not a contract.

`VkPresentTimingSurfaceCapabilitiesEXT.presentStageQueries` reports which present stages the
*current* surface/path can actually timestamp, so VSE can detect at runtime what a given
compositor supports. Per the "loud" philosophy: log the available present stages at startup,
and record per frame which stage produced the timestamp — so a windowed run is never
silently mistaken for compositor-free scanout timing. Both paths still report
`TimingSource::ExtPresentTiming`; the present-stage field distinguishes their quality.

### Extension / feature set (`src/core/device.rs`)

Remove `google_display_timing` from `create_device` (`device.rs:455`) and
`supports_google_display_timing` (`device.rs:435`). The EXT path enables, via raw
`vkCreateDevice`: `VK_EXT_present_timing`, `VK_KHR_present_id2`, `VK_KHR_present_wait2`, and
`VK_KHR_calibrated_timestamps` (vulkano *does* know the last one, but the whole device is
created raw on the EXT path, so all four go through the raw name list). Feature structs for
present-timing / present-id2 are chained into the create-info pNext.

### Timing backend reshape (`src/timing/provider.rs`)

The current trait is shaped around *spin-wait-before-present* + *poll-after-present*. EXT
wants *schedule-at-present* + *query-by-present-id* + *clock-calibration*. Because timing
and presentation are inseparable under EXT (the schedule rides on the present call, the
feedback is keyed to the present-id it assigned), the backend **owns the present path** —
mirroring how `GoogleDisplayTimingProvider` already holds the swapchain handle and ash
device (`provider.rs:94-99`).

```rust
/// Sketch — final names TBD.
pub trait PresentTimingBackend {
    fn source(&self) -> TimingSource;
    fn refresh_cycle_duration(&self) -> Option<Duration>;

    /// Submit `image_index` for presentation, assigning `present_id` and optionally
    /// scheduling it for `target` (VSE Clock domain). Under EXT this attaches
    /// VkPresentId2KHR + VkSwapchainPresentTimingInfoEXT and returns immediately —
    /// no CPU spin. Under CPU fallback this paces via the fence (and present_wait2
    /// when available) and returns a fence handle.
    fn present(&mut self, req: PresentRequest<'_>) -> Result<InFlight, SwapchainError>;

    /// Hardware scanout time for `present_id`, already converted into the VSE Clock
    /// domain via the calibration. `None` until the driver has reported it.
    fn confirmed_scanout(&self, present_id: u64) -> Option<Timestamp>;
}
```

`wait_for_target()` is **deleted** — scheduling no longer has a CPU-side wait step. The
fallback backend keeps a *pacing* wait (fence, optionally `present_wait2`) but it is no
longer how targets are honored; on the fallback path an absolute target simply cannot be
hardware-enforced, and that limitation is reported through `TimingSource::CpuEstimate`.

`TimingSource` (`src/timing/timing_source.rs`) collapses to two variants:
`ExtPresentTiming`, `CpuEstimate`. `GoogleDisplayTiming` is removed.

### Clock calibration — the reproducibility core (`src/timing/clock.rs`)

Today `Clock` wraps an opaque `Instant` epoch (`clock.rs:11`). For scheduling we must emit
absolute target times in the driver's time domain, and for verification we must pull
hardware scanout times *into* the VSE domain. Both need one calibration anchor:

- At `Clock::new()`, additionally capture an absolute `CLOCK_MONOTONIC` reading
  (`libc::clock_gettime`; `libc` is already a dependency on Linux). Store `epoch_mono_ns`.
- VSE `Timestamp(µs)` ↔ absolute monotonic ns:
  `mono_ns = epoch_mono_ns + ts_us * 1_000`.
- Learn the swapchain's supported time domains from the extension:
  `vkGetSwapchainTimeDomainPropertiesEXT` returns `(VkTimeDomainKHR, timeDomainId)` pairs.
  **If `VK_TIME_DOMAIN_CLOCK_MONOTONIC_KHR` (= 1) is offered**, use it directly — scheduling
  (`VkPresentTimingInfoEXT.timeDomainId`) and readback (`VkPastPresentationTimingEXT.time`)
  are then in `CLOCK_MONOTONIC` nanoseconds, converting as
  `ts = (scanout_ns − epoch_mono_ns) / 1_000` and targets as
  `epoch_mono_ns + target_us · 1_000`.

  > **Validation finding (2026-07-09): `CLOCK_MONOTONIC` is NOT offered on this hardware.**
  > On Intel Meteor Lake / ANV / Mesa 26.1.4 (i915), the swapchain offers only
  > `[1000208000] = VK_TIME_DOMAIN_PRESENT_STAGE_LOCAL_EXT` — in **both** windowed and
  > direct-display modes. The compositor was not the variable; the driver simply does not
  > expose a CPU clock domain through present-timing. The earlier "prefer MONOTONIC, rarely
  > need calibration" framing was wrong for this driver.

- **The calibration path via `VK_KHR_calibrated_timestamps` is the spec-intended mechanism**,
  not a workaround for deficient hardware. Per the extension proposal, present timestamps
  normally live in an opaque present-stage-local domain precisely because present hardware may
  run on its own clock; a directly-exposed `CLOCK_MONOTONIC` domain is an optional shortcut we
  use *if offered* but do not depend on. Full write-up in
  [`docs/clock-synchronization.md`](../clock-synchronization.md). `PRESENT_STAGE_LOCAL` is a
  real nanosecond clock with an unknown relationship to `CLOCK_MONOTONIC`. To correlate: call
  `vkGetCalibratedTimestampsKHR`
  with two domains sampled together — one `VkCalibratedTimestampInfoKHR` for
  `CLOCK_MONOTONIC`, and one carrying `VkSwapchainCalibratedTimestampInfoEXT`
  (`sType` 1000208009: `{ swapchain, presentStage = IMAGE_FIRST_PIXEL_OUT, timeDomainId }`)
  for the present-stage-local clock. The pair yields an offset (plus a max-deviation bound)
  that maps scanout `PRESENT_STAGE_LOCAL` times → `CLOCK_MONOTONIC` → VSE `Clock`. Re-sample
  periodically to track drift. `VK_KHR_calibrated_timestamps` (rev 1) and
  `VK_EXT_calibrated_timestamps` (rev 2) are both present on this device.

The result: `submit_time`, hardware `present_time`, scheduled targets, **and** any external
recording system that timestamps in `CLOCK_MONOTONIC` all live in one comparable domain.
That single-domain guarantee is the concrete reproducibility win over the current
epoch-mismatched state.

### Present-id correlation (`context.rs`, `buffered.rs`)

- `present_id = frame_number + 1`, monotonic, **`u64`** (`present_id2` is 64-bit — this also
  removes the `& 0xFFFF_FFFF` truncation at `provider.rs:251`). Attached via
  `VkPresentId2KHR`.
- Feedback: `vkGetPastPresentationTimingEXT` records are keyed by their present-id and
  matched *exactly* to the pending frame. `build_confirmed_flip` (`context.rs:483`) and the
  buffered confirmation loop key on present-id instead of "latest," fixing the buffered
  pipeline's core assumption.
- Missed-frame detection uses real per-present scanout deltas rather than the
  `ratio > 1.5` heuristic (`context.rs:1801`, `context.rs:512`). Under EXT we can
  additionally report whether a scheduled frame hit its requested slot.

### `FlipInfo` evolution (`src/timing/flip_info.rs`)

`present_time` becomes a genuine hardware scanout timestamp under EXT. Proposed additive
fields (serde-compatible, defaulted for skipped frames):

- `present_id: u64` — the correlation key, useful for joining against raw driver logs.
- `target_time: Option<Timestamp>` and `on_target: bool` — was this a scheduled present,
  and did hardware honor the target? A first-class verification artifact.

> **Decision B — RESOLVED (2026-07-09): add the fields.** `FlipInfo` gains `present_id: u64`,
> `target_time: Option<Timestamp>`, and `on_target: bool`, propagated into the Parquet/CSV
> schema. They are the verification data the whole reorg exists to produce.

### present_wait2 as a pacing primitive

Optional, capability-gated, orthogonal to `TimingSource`. In `run_buffered()` the fence
poll that confirms a frame (`swapchain.rs:456-463`, `InFlightFuture::is_complete`) can be
replaced or augmented by `vkWaitForPresent2KHR(present_id, timeout)` for deterministic
"frame N is on screen" backpressure without spinning. It applies in both the EXT and
fallback paths; when unavailable, the current fence poll stands. It never sets
`TimingSource`.

### Removing GOOGLE

Delete `GoogleDisplayTimingProvider` (`provider.rs:89-262`),
`TimingSource::GoogleDisplayTiming`, the `supports_google_display_timing` probe and its
enablement (`device.rs:435,455`), both selection branches (`context.rs:696-704`,
`context.rs:794-800`), the `provider.rs` re-export (`timing/mod.rs:13`), and the
`ash::google::display_timing` usage. Update docs: `timing.md` (tier table + selection
logic), `timing-roadmap.md`, `guides/buffered_flips.md`, `guides/data_recording.md`,
`guides/experiment_data_schema.md`, `README.md`, and the `FlipInfo` rustdoc timing-source
list (`flip_info.rs:22-25`).

---

## Reproducibility & Determinism

Pulling the pieces together — this is *why* the reorg matters, not just how:

- **One clock across the whole chain.** Submit time, hardware scanout time, scheduled
  targets, and `CLOCK_MONOTONIC`-based ephys timestamps become directly comparable. Aligning
  a spike train to photon onset stops requiring an unknown, variable offset.
- **Hardware-enforced onset.** A stimulus targeted at an absolute time is held by the GPU
  until that time and reported with the time it actually achieved. Onset no longer depends
  on when the CPU spin-loop happened to observe the clock — removing a real source of
  run-to-run nondeterminism.
- **Exact frame accounting.** Per-present-id scanout timestamps replace the "duration >
  1.5× expected" heuristic, so missed/duplicated frames are detected from ground truth.
- **Honest provenance.** Two loud tiers mean a `CpuEstimate` run is never silently mistaken
  for a hardware-verified one in the recorded data.

Deterministic scheduling model: given a start-vblank timestamp `t0` and refresh `T`, onset
`k` is the absolute target `t0 + k·T`, enforced by hardware and verified against the
reported scanout — reproducible to the frame regardless of CPU load.

---

## Risks / Open Questions

- **`from_handle` adoption brittleness.** Its invariants (image-handle order matching
  `vkGetSwapchainImagesKHR`, `create_info` matching the raw create) must be honored exactly.
  Mitigation: one module owns raw creation and mirrors vulkano's own create parameters
  field-for-field. Fork-vulkano is the escape hatch (decision A).
- **Struct-layout fidelity.** Hand-rolled `VkSwapchainPresentTimingInfoEXT`,
  `VkPastPresentationTimingEXT`, `VkPresentId2KHR`, feature structs, etc. must match
  `present_timing` **revision 3** exactly. Mitigation: pin to a cited header revision;
  add size/offset `const` assertions in the FFI module.
- **Time-domain variance** across Mesa / NVIDIA / Windows. Mitigation: never assume
  `CLOCK_MONOTONIC`; route conversion through the calibrated-timestamps abstraction, with
  the direct-MONOTONIC case as a fast path.
- **Validation-layer noise** at the vulkano/raw seams (unknown pNext on adopted objects).
  Expected; verify with validation enabled on ANV.
- **Two init paths** double the initialization surface. Mitigation: contain all divergence
  in `present_timing_ext`; fallback path is byte-for-byte today's code.
- **Nothing lands** until EXT is verified end-to-end on this machine (ANV) *and* the CPU
  fallback path is re-verified against the current behavior.

---

## Rollout (sequencing deferred)

Per the "design doc only" decision, the phased implementation breakdown is deferred to a
companion `2026-07-09-ext-present-timing-impl.md`. The design implies this order: FFI module
+ raw capability probe → clock calibration → raw device/swapchain create + adopt (behind the
probe) → backend reshape + raw present/feedback → wire `flip()` / `flip_with_payload()` /
`build_confirmed_flip()` to present-id → remove GOOGLE → docs, examples, tests. Detailed
steps, test plan, and verification gates belong in the impl doc once this design is approved.
