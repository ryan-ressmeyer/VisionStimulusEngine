# Timing in VisionStimulusEngine

## 1. The Need for Precision

The neural code for vision is known to be precise on millisecond timescales. When correlating a visual stimulus with a recorded spike from a neuron, we must know **exactly** when the photons left the monitor and entered the subject's eye. This is a fudandamental requirement for any visual neuroscience experiment, and thus is central to the design of VisionStimulusEngine. 

Only recently have graphics APIs provided the necessary tools to achieve this level of precision, having taken almost two decades to provide official support for what neurophysiologists previously needed to force OpenGL to support. The problem is that standard game engines prioritize *throughput* (average FPS) and *latency* (input-to-response time) over *isochrony* (exact, predictable intervals). They often allow frame intervals to jitter to maintain responsiveness.

For this project, we prioritize **predictability** and **verification**:

1. **Isochrony:** Frames must appear at exact intervals (e.g., every 16.66ms).
2. **Verification:** We must produce a timestamp for every frame that confirms when scanout actually began.

## 2. The Complexity of Display Timing

Modern rendering is a deep, asynchronous pipeline. A "frame" exists on multiple timelines, and confusing them is a common source of timing errors in experiments. 

1. **CPU Time (`t_submit`):** The moment the code calls `queue_present()`.
* *Error:* The GPU may be busy and not process this command for milliseconds.


2. **GPU Completion (`t_render`):** The moment the GPU finishes drawing the pixels.
* *Error:* The image sits in a swapchain queue waiting for the next VSYNC interval.


3. **Display Scanout (`t_present`):** The moment the display controller begins reading memory to send to the panel.
* *This is the only timestamp that matters for science.*

### Which clock do we live in?

Because scanout is the timestamp that matters, **the scanout clock is VSE's primary experimental
clock** — not something we convert into host time. We anchor a scanout `t=0` at session start,
schedule onsets as `t0 + k·T`, and record actual scanout times, all natively in the scanout
domain. The host CPU clock never enters the presentation loop.

This is deliberate agnosticism about the experiment's *ultimate* clock, matching Psychtoolbox:

* **Aligning to a neural-recording / DAQ box** is done **physically** — a photodiode on a stimulus
  patch feeding the acquisition ADC records true photon onset in *that box's* clock. VSE does not
  need to know the acquisition clock; it only guarantees deterministic, known onsets in scanout
  time.
* **Host-originated events** (key presses, network messages) arrive in CPU `CLOCK_MONOTONIC` time
  and can only reach scanout time through an **opt-in** calibration bridge (scanout ↔
  `CLOCK_MONOTONIC`; see [`clock-synchronization.md`](clock-synchronization.md)). It is off the
  hot path and must be explicitly requested.

## 3. Supported Timing Mechanisms in Vulkan

We support a hierarchy of timing mechanisms, ranging from "Gold Standard" to "Best Guess."

### Tier 1: `VK_EXT_present_timing` (The Gold Standard)

* **What it is:** A hardware feedback loop, released in Vulkan 1.4.335 (November 2025) after ~5 years of development.
* **Capabilities:**
* **Scheduling:** Allows specifying an `earliestPresentTime`. The GPU will hold the frame until this specific nanosecond, eliminating early flips.
* **Feedback:** Returns `VkPastPresentationTimingEXT`, containing the exact hardware clock time when the image scanout began.

* **Status:** Preferred and implemented. Available in stable Mesa 26.1 (AMD/RADV, NVIDIA/NVK, Intel/ANV) and the NVIDIA 595 series; verified on Intel (Mesa 26.1.4). VSE uses hand-rolled FFI for the Vulkan 1.4 present-timing structs because vulkano 0.35 predates those bindings.

### Historical: `VK_GOOGLE_display_timing`

`VK_GOOGLE_display_timing` was the Linux/Android predecessor to `EXT_present_timing`. VSE no
longer uses it. The active fallback is the explicit `CpuEstimate` path, so data files have two
provenance labels rather than a partially supported middle tier.

### Tier 2: `VK_KHR_present_wait` / `VK_KHR_present_wait2` (Pacing)

* **What it is:** A synchronization tool that allows the CPU to wait for the GPU to finish *handing off* the image.
* **Capabilities:**
* **Feedback:** Tells you when the present "completed" on the GPU side.
* **Limitation:** Does not confirm the image is on screen, only that it is ready to be.


* **Status:** Useful for preventing queue buildup, but insufficient for verification.

### Tier 3: `std::time::Instant` (The Software Baseline)

* **What it is:** Checking the CPU clock immediately after submitting the work.
* **Capabilities:** None (Estimate only).
* **Limitation:** Subject to OS scheduler jitter and GPU queue depth.
* **Status:** Last resort / Development mode.

## 4. Project Strategy: The "Loud Fallback"

VisionStimulusEngine does not crash if high-precision timing is unavailable. Instead, it degrades gracefully but **reports the degradation explicitly** in the data.

We use a `TimingSource` enum in our data logs (`FlipInfo`):

```rust
pub enum TimingSource {
    ExtPresentTiming, // scanout-clock timing from VK_EXT_present_timing
    CpuEstimate,      // host-clock fence time; no scanout verification
}

```

**Selection Logic:**

1. On startup, `Context` queries device extensions.
2. If `VK_EXT_present_timing` and its required companion extensions are available, enable the EXT path and set `source = ExtPresentTiming`.
3. Else, fall back to `CpuEstimate` and emit a warning log.

## 5. Development vs. Experiment: Windowed and Direct Display

> **Dogma: develop windowed, run experiments in direct display.**

`VK_EXT_present_timing` is a *swapchain* feature and works in **both** windowed/composited
and direct-display modes — compositor-awareness was the entire point of its multi-year
standardization, so it is not restricted to direct display. What changes between the two is
the **fidelity and determinism** of the timing, and VSE reports which one you are getting
rather than hiding it.

### Windowed / composited (development)

In a normal desktop session your present hands the image to the compositor (Wayland or X11),
which composites it with everything else and presents on *its* cadence.

- **Pros:** trivial to run, debug, and iterate; works in your normal session; timing data is
  still hardware-anchored and labeled `ExtPresentTiming`.
- **Cons:** the reported time reflects when the *compositor's* frame (containing your
  stimulus) reached the screen — one extra pipeline stage of latency, and exact scheduled
  targets are honored only if the compositor cooperates (modern Mutter/KWin/wlroots do, via
  the presentation-time / FIFO protocols). A fullscreen, unoccluded window often gets a
  direct scanout/flip path that approaches direct-display fidelity — but that is a compositor
  optimization, **not a guarantee**, and it can silently regress when anything else redraws.

### Direct display (experiments)

VSE's direct-display mode (`VK_KHR_display`, see `docs/guides/display_backends.md`) takes
exclusive control of the physical display. There is no compositor in the path.

- **Pros:** `IMAGE_FIRST_PIXEL_OUT` is the true hardware scanout of *your* content; scheduled
  targets are enforced directly by display hardware; no compositor jitter or latency. This is
  the deterministic, reproducible path.
- **Cons:** requires a free display / VT; not your everyday desktop session.

### How VSE keeps this honest

Both paths report `TimingSource::ExtPresentTiming`, so the source label alone does not tell
you which you were on. VSE therefore:

- logs, at startup, which present *stages* the current surface can actually timestamp
  (`VkPresentTimingSurfaceCapabilitiesEXT::presentStageQueries`), and
- records, per frame, which present stage produced the timestamp.

A windowed run is thus never silently mistaken for compositor-free scanout timing. **Prototype
your experiment in a window; collect the data you will publish in direct display.**

> **Note on clock calibration (not a compositor issue).** Hardware scanout timestamps come
> back in a driver-chosen time domain. Some drivers expose `CLOCK_MONOTONIC` directly; others
> — including Intel/ANV (Mesa 26.1.4), in *both* windowed and direct display — expose only an
> opaque `PRESENT_STAGE_LOCAL` clock. VSE bridges that to `CLOCK_MONOTONIC` (and thus to your
> ephys hardware) using `VK_KHR_calibrated_timestamps` — the mechanism the extension was
> designed around. This calibration is required regardless of windowed vs. direct display; the
> reason to run experiments in direct display is *fidelity and determinism* (no compositor
> latency, jitter, or reordering), not the clock domain. Full details, including the measured
> error on this hardware, are in [`clock-synchronization.md`](clock-synchronization.md).

## 6. Historical Context: The Psychtoolbox Method

Historically, **Psychtoolbox-3 (PTB)** set the standard for timing precision using OpenGL. Since older APIs lacked explicit timing controls, PTB achieved precision through "Blocking and Polling":

1. **Spin-Waiting:** PTB would busy-wait the CPU (burning 100% core usage) to catch the exact moment of a vertical blank.
2. **Beam Position Queries:** After swapping buffers, PTB would query the GPU's "raster beam position" register to see which scanline was currently being drawn.
3. **Mathematical Correction:** By knowing the beam position, PTB could calculate back in time to find the exact start of the frame.

**The Improvement:**
VisionStimulusEngine's use of `VK_EXT_present_timing` replaces these CPU-intensive "hacks" with a **Hardware Contract**. We do not fight the OS scheduler; we simply register a time with the GPU, and the hardware guarantees execution and reporting. This results in lower CPU usage and higher reliability across different OS versions.

## 7. References

**Core Standards:**

* [VK_EXT_present_timing Specification](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_present_timing.html)
* [Khronos Blog: The Journey to State-of-the-Art Frame Pacing](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan)

**Academic References:**

* **Kleiner M, et al. (2007).** *What's new in Psychtoolbox-3?* (Describes the beam-position method).
* **Brainard, D. H. (1997).** *The Psychophysics Toolbox.* Spatial Vision.
* **Peirce, J. W. (2007).** *PsychoPy: Psychophysics software in Python.* J. Neurosci. Methods. (See `docs/refs/Peirce2006.pdf`).
References:

