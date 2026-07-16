# Clock Synchronization: CPU, GPU, and Present Clocks

For vision science we need to know **when photons left the display**. VSE takes the Psychtoolbox
posture and is **intentionally agnostic about the experiment's ultimate clock**, with a strict
hierarchy among the clocks a modern graphics stack exposes:

- **The scanout clock is primary.** Hardware scanout time is reported by `VK_EXT_present_timing`
  in the **present-stage-local** domain, and VSE *lives in that domain* for all display timing:
  anchor a scanout `t=0` at session start, schedule onsets as `t0 + k·T`, and record actual
  scanout times, natively. The core presentation loop does **no** cross-clock conversion.
- **Alignment to neural-recording hardware is physical, not software.** A DAQ/ephys box runs its
  own clock, usually on another machine. The canonical tie is a **photodiode on a stimulus patch
  feeding the acquisition ADC** — one physical event, recorded in the acquisition clock. VSE never
  needs to know or estimate that clock; it only guarantees onsets that are *deterministic and
  known in scanout time*, which the photodiode then ties to acquisition time.
- **The host CPU clock is secondary and opt-in.** Bridging scanout ↔ `CLOCK_MONOTONIC` is a
  *convenience tool*, not the backbone — needed only to place host-originated events (key presses,
  network messages) into scanout time, or for host-only behavioral experiments where the CPU clock
  is the response clock.

This document explains the clocks, how VSE relates them when the opt-in bridge is used, and —
critically — the **error** involved, with links to primary sources. The rest of the document
concerns that opt-in bridge; the primary (scanout-native) path needs none of it.

## 1. The clocks in play

| Clock | Where it lives | How VSE reads it |
|---|---|---|
| **CPU monotonic** (`CLOCK_MONOTONIC`) | Host CPU / OS | `std::time::Instant`; VSE's [`Clock`] is anchored to it |
| **GPU device clock** (`VK_TIME_DOMAIN_DEVICE`) | GPU, ticks at `timestampPeriod` ns | `vkGetCalibratedTimestampsKHR` |
| **Present-stage-local** (`VK_TIME_DOMAIN_PRESENT_STAGE_LOCAL_EXT`) | The display/scanout hardware | `VK_EXT_present_timing` reports scanout times here |
| **External acquisition clock** | Ephys / DAQ hardware | Recorded alongside stimulus events; often `CLOCK_MONOTONIC`-derived |

The scientifically meaningful timestamp — hardware scanout — is reported by
`VK_EXT_present_timing` in the **present-stage-local** domain. VSE treats that domain as
primary and times relative to it. It is *not* directly comparable to the CPU clock; the
sections below cover the **opt-in** bridge for when host events must be expressed in scanout
time. When only display timing and a photodiode matter, no bridge is needed at all.

## 2. Why present times are not just `CLOCK_MONOTONIC`

`VK_EXT_present_timing` defines two swapchain-relative time domains,
`VK_TIME_DOMAIN_PRESENT_STAGE_LOCAL_EXT` and `VK_TIME_DOMAIN_SWAPCHAIN_LOCAL_EXT`.
`PRESENT_STAGE_LOCAL` is **required to always be supported**. From the extension proposal:

> This time domain … allows platforms where different presentation stages are handled by
> independent hardware to report timings in their own time domain.

In other words, the designers assumed the general case is that presentation hardware runs on
its *own* clock, and the intended mechanism for relating it to a CPU clock is **calibration**,
via `VkSwapchainCalibratedTimestampInfoEXT` and `vkGetCalibratedTimestampsKHR`. Exposing
`CLOCK_MONOTONIC` directly in a swapchain's time-domain list is an *optional convenience* a
driver may offer; the spec neither requires it nor describes when it would appear.

**Empirically (this project, 2026-07):** Intel Meteor Lake / ANV / Mesa 26.1.4 (i915) offers
only `PRESENT_STAGE_LOCAL` from `vkGetSwapchainTimeDomainPropertiesEXT`, in both windowed and
direct-display modes. So VSE does **not** rely on a native `CLOCK_MONOTONIC` swapchain domain.
Calibration is the primary path — as the extension intended — with a native-domain fast path
used only if a driver happens to offer one.

## 3. How calibration works

`VK_KHR_calibrated_timestamps` (`vkGetCalibratedTimestampsKHR`) samples **several clocks as
close together as the hardware allows** and returns, for each, a timestamp plus a single
`maxDeviation`:

```
(timestamps[], maxDeviation) = vkGetCalibratedTimestamps([
    { timeDomain: PRESENT_STAGE_LOCAL, pNext: VkSwapchainCalibratedTimestampInfoEXT{
        swapchain, presentStage = IMAGE_FIRST_PIXEL_OUT, timeDomainId } },
    { timeDomain: CLOCK_MONOTONIC },
])
```

From one such sample you get an **offset**: `offset = monotonic_ns − present_stage_ns`. A
scanout time `S` (present-stage-local) then maps to the CPU clock as `S + offset`, and finally
into VSE's [`Clock`] via its `CLOCK_MONOTONIC` epoch anchor. Because the two clocks can drift
relative to each other, VSE **re-samples periodically** rather than trusting one offset
forever.

For a general GPU↔CPU sanity check that needs no swapchain, the same call with `{ DEVICE,
CLOCK_MONOTONIC }` characterizes how well the GPU and host clocks correlate on a given machine
— this is what the capabilities probe reports (§5).

Primary references: [`vkGetCalibratedTimestampsKHR`](https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetCalibratedTimestampsKHR.html),
[VK_EXT_calibrated_timestamps proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_calibrated_timestamps.html).

## 4. The error budget

Three distinct error terms, smallest to largest in typical impact:

1. **Sampling deviation (`maxDeviation`).** The bound, in nanoseconds, on how far apart the
   two clock reads were. This is the *irreducible read error* of a single calibration. It
   depends heavily on hardware: a GPU-`DEVICE`-clock read requires a register round-trip that
   can be slow on integrated parts, and any single read is sensitive to OS scheduling.
   **Measured on this machine (Intel MTL, iGPU): best-of-8 ≈ 12–40 µs for
   `Device↔CLOCK_MONOTONIC`, with individual samples spiking to ~340 µs.** The probe reports
   the best-of-N read as the representative figure. A discrete GPU, or the
   `PRESENT_STAGE_LOCAL↔CLOCK_MONOTONIC` pair (which is derived from kernel KMS vblank
   timestamps rather than a GPU register read), is typically far tighter. Do not read the
   Device↔Monotonic number as "the scanout timing error" — it is an upper-bound proxy for
   general clock-sync quality.

2. **Inter-sample drift.** The GPU/present clock and the CPU clock run off different
   crystals, drifting at up to tens of ppm. At 20 ppm, 1 second between calibrations = 20 µs
   of accumulated error. Mitigation: re-calibrate on a short cadence (VSE re-samples; the
   exact interval is a tuning parameter of the calibration subsystem).

3. **Display panel latency — the real floor.** `IMAGE_FIRST_PIXEL_OUT` is *scanout begin*, not
   photon emission. Pixel response and backlight strobing add their own delay and smear,
   which the API only *estimates* via `IMAGE_FIRST_PIXEL_VISIBLE`. This term dominates the
   others and **cannot be measured by software** — it requires a **photodiode**. For any
   experiment where onset timing to the eye matters, a photodiode on a corner patch, logged on
   the acquisition clock, remains the ground truth, matching the Psychtoolbox timing model.

**Takeaway:** the software calibration (terms 1–2) gets stimulus onset onto the CPU/ephys
timeline to well within a frame — good enough that the display panel (term 3), not the clock
math, is the accuracy floor.

## 5. What VSE does

- **VSE `Clock`** is a `std::time::Instant` anchored, at construction, to an absolute
  `CLOCK_MONOTONIC` reading (`clock_gettime`), so any `CLOCK_MONOTONIC` nanosecond value —
  including calibrated GPU/present timestamps and `CLOCK_MONOTONIC`-based ephys hardware —
  converts directly into VSE timestamps.
- **Capabilities probe** (`HostInfo.timing`, `TimingCapabilities`): captured into every
  experiment's host snapshot. Reports whether present timing / present-id2 / present-wait2 /
  calibrated timestamps are supported, which CPU clock domains are calibrateable, and a
  measured `Device↔CLOCK_MONOTONIC` `maxDeviation` — a per-machine indicator of clock-sync
  quality. Run `examples/06_host_info` to dump it.
- **Scanout is the native domain.** `FlipInfo.present_time` is a **scanout-clock** timestamp
  (present-stage-local, referenced to the session's scanout `t=0`). It is *not* converted to CPU
  time by default; the presentation loop stays entirely in the scanout domain.
- **Opt-in host-clock bridge** (calibration subsystem): samples `PRESENT_STAGE_LOCAL ↔
  CLOCK_MONOTONIC` together via `VkSwapchainCalibratedTimestampInfoEXT` +
  `vkGetCalibratedTimestampsKHR`, and maintains a **lower-envelope, drift-tracked** offset
  (offset + rate). Its purpose is to place host-originated events (key presses, network) into
  scanout time, and to expose an optional CPU-clock value alongside `present_time` when a user
  asks for it. It is never on the presentation hot path.
- **Loud provenance:** `FlipInfo.timing_source` records whether a frame's `present_time` is a
  hardware scanout (`ExtPresentTiming`) or a CPU estimate (`CpuEstimate`), so hardware-verified
  and estimated runs are never confused in the data.

### Measured drift (Intel MTL / ANV / Mesa 26.1.4, windowed, 2026-07)

`examples/09_present_clock_drift` samples the bridge every frame and fits it. On this hardware:

- `PRESENT_STAGE_LOCAL` is a **genuinely separate clock** — a fixed ~29,714 s epoch offset from
  `CLOCK_MONOTONIC`, i.e. *not* a re-based `CLOCK_MONOTONIC`. Calibration is genuinely required to
  bridge.
- **Relative drift is a stable ~1.97 ppm** over 120 s (per-window 1.93 ± 0.14 ppm across twelve
  10 s windows; a visibly clean line). Left uncorrected that is ~3.55 ms over a 30-min session —
  small but a real *systematic*, which is why the bridge models **offset + drift rate**, not a
  single offset.
- **Read noise is one-sided:** the true offset is the *lower envelope* of the samples; jitter can
  only make a read appear later (median `maxDeviation` ~18 µs, tail to ~425 µs). So the estimator
  takes the **minimum / low quantile** over a sliding window (averaging would bias high), yielding
  ~1–2 µs offset stability — far below the display-panel floor (§4, term 3).

Caveat: 120 s cannot observe thermal wander of the *rate* itself over a long recording — another
reason the bridge **tracks** (sliding window) rather than freezing a slope. Re-confirm on the
direct-display path when the raw-present subsystem lands.

## 6. Driver conformance: advertised ≠ implemented

`VK_EXT_present_timing` is very new (finalized 2024–2025), and a driver can **advertise** the
extension and its feature bits while only **partially implementing** them. VSE therefore treats
advertised support as a claim to be *behaviorally verified*, falls back to a correct path when a
sub-feature is missing, and **records what the driver actually did** in the host snapshot so a run's
timing pedigree is never silently wrong.

**Measured on this project's reference machine (Intel Meteor Lake / ANV / Mesa 26.1, 2026-07),
direct-display and windowed:** two sub-features are advertised but not functional:

| Sub-feature | Advertised | Actually works | VSE's response |
|---|---|---|---|
| `present_id2` correlation | ✓ | ✓ | Used directly (`FlipInfo.present_id`). |
| `present_wait2` | ✓ | ✓ | Paces the sync `flip()` to the vblank. |
| Calibrated `PRESENT_STAGE_LOCAL` clock | ✓ | ✓ | The real scanout-time source (see below). |
| **`vkGetPastPresentationTimingEXT` stage timestamps** | ✓ | **✗ — returns `IMAGE_FIRST_PIXEL_OUT = 0`** in *complete* records | Sync `flip()` samples the calibrated scanout clock right after `wait_for_present` (which returns at the frame's scanout) → real scanout `present_time`, **7 µs** from the clock. |
| **Absolute scheduling `VkPresentTimingInfoEXT.targetTime`** | ✓ (`presentAtAbsoluteTime`) | **✗ — target accepted and echoed in feedback, but the present is not held** | VSE **software-paces** scheduled flips against the scanout clock (still sends the hardware target, which a conformant driver would honor). |

Both gaps are the *timing-report* and *scheduling* halves of the extension; the driver implemented
the *correlation* and *wait* halves first. On a driver that fully implements the extension, VSE
automatically prefers the true per-present feedback timestamp and (harmlessly) the hardware target —
no code change needed.

### How VSE detects and reports it

- **Passive feedback check (automatic, every session):** VSE watches present-timing feedback and,
  once a ring's worth of records has arrived all-zero, concludes the stage timestamps are stubbed,
  emits a **one-time `WARN`** naming the fallback in use, and records
  `HostInfo.timing.scanout_feedback_populated = Some(false)`. `ctx.scanout_feedback_populated()`
  exposes it. No extra presents, no startup cost.
- **Scheduling provenance:** the first scheduled `flip(Some(t))` logs a one-time note that presents
  are software-paced and that hardware `targetTime` enforcement is driver-dependent and unverified.
- **Enforcement characterization (on demand):** actively testing scheduling enforcement requires
  presenting deliberate multi-vblank gaps, which disrupts frames, so it is **not** auto-run.
  `examples/13_direct_display_scanout` measures it (schedules gaps from a fixed `t0 + k·T` anchor and
  checks the measured scanout gap) and reports `absolute_scheduling_enforced`. Run it once per
  hardware/driver config; `examples/06_host_info` prints the advertised-vs-observed table.

The guiding rule: **VSE's behavior stays correct via fallbacks, and provenance
(`FlipInfo.timing_source`, `HostInfo.timing.*`, the one-time warnings) reports the mechanism that was
actually used** — so hardware-verified and worked-around runs are never confused in the data.

## 7. Primary sources

- [VkTimeDomainKHR — reference](https://docs.vulkan.org/refpages/latest/refpages/source/VkTimeDomainKHR.html)
- [VK_EXT_present_timing — proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_present_timing.html)
- [VK_EXT_present_timing — reference](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_present_timing.html)
- [VK_EXT_calibrated_timestamps — proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_calibrated_timestamps.html)
- [vkGetCalibratedTimestampsKHR — reference](https://registry.khronos.org/vulkan/specs/latest/man/html/vkGetCalibratedTimestampsKHR.html)
- [Khronos blog: VK_EXT_present_timing — the journey to state-of-the-art frame pacing](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan)

[`Clock`]: ../src/timing/clock.rs
