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



## 3. Supported Timing Mechanisms in Vulkan

We support a hierarchy of timing mechanisms, ranging from "Gold Standard" to "Best Guess."

### Tier 1: `VK_EXT_present_timing` (The Gold Standard)

* **What it is:** A 2025 extension that provides a hardware feedback loop.
* **Capabilities:**
* **Scheduling:** Allows specifying an `earliestPresentTime`. The GPU will hold the frame until this specific nanosecond, eliminating early flips.
* **Feedback:** Returns `VkPastPresentationTimingEXT`, containing the exact hardware clock time when the image scanout began.

* **Status:** Preferred. Requires recent drivers (NVIDIA Beta / Mesa 26.1+).

### Tier 2: `VK_GOOGLE_display_timing` (The Silver Standard)

* **What it is:** The predecessor to `EXT_present_timing`, widely supported on Android and Linux.
* **Capabilities:**
* **Feedback:** Provides accurate `pastPresentationTime` from the kernel/display driver.
* **Limitation:** Less robust scheduling controls than the EXT version.

* **Status:** Good fallback for Linux systems.

### Tier 3: `VK_KHR_present_wait` (The Bronze Standard)

* **What it is:** A synchronization tool that allows the CPU to wait for the GPU to finish *handing off* the image.
* **Capabilities:**
* **Feedback:** Tells you when the present "completed" on the GPU side.
* **Limitation:** Does not confirm the image is on screen, only that it is ready to be.


* **Status:** Useful for preventing queue buildup, but insufficient for verification.

### Tier 4: `std::time::Instant` (The Software Baseline)

* **What it is:** Checking the CPU clock immediately after submitting the work.
* **Capabilities:** None (Estimate only).
* **Limitation:** Subject to OS scheduler jitter and GPU queue depth.
* **Status:** Last resort / Development mode.

## 4. Project Strategy: The "Loud Fallback"

VisionStimulusEngine does not crash if high-precision timing is unavailable. Instead, it degrades gracefully but **reports the degradation explicitly** in the data.

We use a `TimingSource` enum in our data logs (`FlipInfo`):

```rust
pub enum TimingSource {
    HardwareScanout, // VK_EXT_present_timing (Trust this)
    DriverComplete,  // VK_GOOGLE_display_timing (Mostly trust this)
    CpuEstimate,     // std::time::Instant (Use only for debugging)
}

```

**Selection Logic:**

1. On startup, `Context` queries device extensions.
2. If `VK_EXT_present_timing` is available, enable it and set `source = HardwareScanout`.
3. Else, try `VK_GOOGLE_display_timing`.
4. Else, fall back to `CpuEstimate` and emit a warning log.

## 5. Historical Context: The Psychtoolbox Method

Historically, **Psychtoolbox-3 (PTB)** set the standard for timing precision using OpenGL. Since older APIs lacked explicit timing controls, PTB achieved precision through "Blocking and Polling":

1. **Spin-Waiting:** PTB would busy-wait the CPU (burning 100% core usage) to catch the exact moment of a vertical blank.
2. **Beam Position Queries:** After swapping buffers, PTB would query the GPU's "raster beam position" register to see which scanline was currently being drawn.
3. **Mathematical Correction:** By knowing the beam position, PTB could calculate back in time to find the exact start of the frame.

**The Improvement:**
VisionStimulusEngine's use of `VK_EXT_present_timing` replaces these CPU-intensive "hacks" with a **Hardware Contract**. We do not fight the OS scheduler; we simply register a time with the GPU, and the hardware guarantees execution and reporting. This results in lower CPU usage and higher reliability across different OS versions.

## 6. References

**Core Standards:**

* [VK_EXT_present_timing Specification](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_present_timing.html)
* [Khronos Blog: The Journey to State-of-the-Art Frame Pacing](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan)

**Academic References:**

* **Kleiner M, et al. (2007).** *What's new in Psychtoolbox-3?* (Describes the beam-position method).
* **Brainard, D. H. (1997).** *The Psychophysics Toolbox.* Spatial Vision.
* **Peirce, J. W. (2007).** *PsychoPy: Psychophysics software in Python.* J. Neurosci. Methods. (See `docs/refs/Peirce2006.pdf`).
References:

