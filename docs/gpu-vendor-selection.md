# GPU Vendor Selection for VSE — AMD vs. NVIDIA (and Intel)

**Date:** 2026-07-11
**Status:** Reference / hardware-selection guidance
**Scope:** Historical hardware-selection analysis written after the 3D-rendering research pass. Driver and display-path capabilities change; this document does not establish timing conformance. Use [Timing conformance](timing-conformance.md) and characterize the candidate rig.

---

## TL;DR

For VSE, GPU selection also selects a driver, display stack, external-memory implementation, and maintenance model. None of those choices supplies a timing guarantee without measurement.

**Recommendation: a midrange AMD (RDNA-class) dGPU for experiment rigs.** AMD keeps every layer
of VSE's verified-timing story — present timing, direct scanout, buffer sharing, queue
quality-of-service, and driver auditability — on the same open Mesa stack already characterized
on the Intel reference hardware. NVIDIA offers a co-authored present-timing extension and more
hardware queues, but breaks the direct-display path outright and turns every driver anomaly into
a black box. Keep NVIDIA out of the timing-critical chain; if CUDA compute is ever needed
(e.g. in-loop neural-network stimulus synthesis), put it on a *second* GPU feeding frames across
the external-memory seam (landscape doc §5.4) and let AMD drive the displays.

| Axis | AMD (RADV / Mesa) | NVIDIA (proprietary) |
|---|---|---|
| Driver source auditable / patchable | ✅ open (Mesa) | ❌ closed |
| `VK_EXT_present_timing` | ✅ Mesa 26.1 (shared with Intel ANV) | ✅ shipped (co-authored the ext) — but unverifiable by source |
| Direct display (`VK_KHR_display`) / DRM lease | ✅ mature (SteamVR/Monado path) | ❌ documented lease failures + latency |
| dmabuf external memory + DRM format modifiers | ✅ first-class | ⚠️ historically missing/inconsistent |
| Queue QoS (`VK_KHR_global_priority`) | ✅ documented, used by SteamVR/Monado | ⚠️ many queues, opaque preemption |
| Behavior transfers from Intel dev machine | ✅ same Mesa WSI/present-timing code | ❌ different stack entirely |
| Top-end raster performance / CUDA | ⚠️ no CUDA | ✅ |

---

## 1. Auditability — the decisive axis

VSE records advertised and observed behavior separately. The 2026-08-03 investigation initially blamed ANV for zero scanout-stage timestamps and ignored targets, but validation showed that VSE had omitted the required swapchain timing opt-in. Corrected runs populated feedback and enforced the tested direct-display targets. The episode supports validation and controlled measurement; it is not evidence of an ANV conformance defect.

- **AMD on Linux means RADV**, which lives in the same Mesa tree as Intel's ANV and shares the
  same WSI (window-system integration — the code that connects Vulkan to the display system) and
  present-timing implementation
  ([Mesa 26.1 merged `VK_EXT_present_timing` for X11 & Wayland](https://www.phoronix.com/forums/forum/linux-graphics-x-org-drivers/vulkan/1608746-vulkan-vk_ext_present_timing-merged-to-mesa-26-1-for-x11-wayland)).
  Driver behavior characterized on the Intel dev laptop largely **transfers** to an AMD rig, and
  a driver bug is inspectable, reportable, and in the worst case locally patchable.
- **NVIDIA's proprietary driver is closed.** When a timestamp looks wrong there is no source to
  read, no way to distinguish a hardware limit from a driver bug, and no way to fix it. VSE's
  behavioral conformance checks (`scanout_feedback_populated`, `absolute_scheduling_enforced`)
  would still run — but they would be the *only* instrument.
- **NVK** (Mesa's open-source NVIDIA Vulkan driver) is improving
  ([Collabora: implementing DRM format modifiers in NVK](https://www.collabora.com/news-and-blog/news-and-events/implementing-drm-format-modifiers-in-nvk.html))
  but is not yet a timing-grade reference stack; it does not change the recommendation today.

## 2. `VK_EXT_present_timing` — NVIDIA is actually fine here

Credit where due: NVIDIA co-authored the extension
([Khronos: the journey to state-of-the-art frame pacing](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan),
[Phoronix: merged after five years](https://www.phoronix.com/news/VK_EXT_present_timing-Merged))
and recent NVIDIA Linux drivers ship it
([NVIDIA Vulkan driver page](https://developer.nvidia.com/vulkan-driver)). Availability alone does not establish behavior. Record enabled features and surface capabilities, run validation-clean behavior probes, and characterize the exact display path regardless of driver source model.

## 3. Direct display — disqualifying on NVIDIA today

VSE's preferred path for characterized experiments is `VK_KHR_display` direct display with no compositor in its swapchain path. This removes one source of mediation but does not guarantee scheduling or panel timing. For future haploscope/HMD work, DRM leases are the kernel
mechanism that lets one process borrow exclusive control of a display connector. On NVIDIA
proprietary this is a documented mess:

- A standing NVIDIA forum thread reports the proprietary modules
  [completely unable to acquire a DRM lease on any display server, all known drivers, any hardware](https://forums.developer.nvidia.com/t/nvidia-proprietary-non-open-modules-completely-unable-to-acquire-a-drm-lease-on-any-display-server-all-known-nvidia-drivers-any-hardware/341244).
- The Linux VR community documents
  [DRM-lease presentation-latency issues on wired headsets with NVIDIA](https://wiki.vronlinux.org/docs/hardware/)
  and direct-mode failures under Monado/SteamVR
  ([Monado: what is direct mode](https://monado.freedesktop.org/direct-mode.html)).

This is why `docs/3d-vr-rendering-landscape.md` §3.5 already says "prefer Mesa AMD/Intel;
NVIDIA proprietary has documented DRM-lease latency and acquisition failures."

## 4. Buffer sharing — the §5.4 topology has an NVIDIA-specific snag

The recommended 3D integration (landscape doc §5.4) shares the renderer's output images across
two Vulkan devices zero-copy via **dmabuf** (the Linux kernel's file-descriptor mechanism for
passing GPU buffers between drivers and processes). On Mesa this is first-class — it is the
mechanism every Wayland compositor uses each frame. On NVIDIA proprietary,
`VK_EXT_external_memory_dma_buf` has been
[missing or inconsistently exposed](https://forums.developer.nvidia.com/t/vk-ext-external-memory-dma-buf-missing-in-545/275834)
([earlier support request](https://forums.developer.nvidia.com/t/support-for-vk-ext-external-memory-dma-buf/199241)).

The design would *survive* on NVIDIA — NVIDIA↔NVIDIA sharing works with opaque file descriptors
(`VK_KHR_external_memory_fd`) — but you lose wgpu's `texture_from_dmabuf_fd` convenience path
and leave the well-trodden road again.

## 5. Queue topology and QoS — the one place NVIDIA is genuinely nicer

Head-of-line blocking (a long GPU job in front delaying the short present job behind it) is the
scheduling hazard the §5.4 topology exists to solve:

- **Intel (ANV)**: one queue family, one queue (verified locally on the MTL reference machine).
- **AMD (RADV)**: one graphics queue plus a **separate compute queue family** — the composite
  pass can run as a compute dispatch on a high-priority compute queue, exactly Monado's
  `XRT_COMPOSITOR_COMPUTE=1` path
  ([Monado frame pacing](https://monado.pages.freedesktop.org/monado/frame-pacing.html)).
  `VK_EXT/KHR_global_priority` (a driver-level QoS knob letting the kernel preempt
  lower-priority GPU work) is long-supported and is how SteamVR keeps its compositor ahead of
  app rendering — `REALTIME` priority needs `CAP_SYS_NICE`
  ([SteamVR-for-Linux #107](https://github.com/ValveSoftware/SteamVR-for-Linux/issues/107)).
- **NVIDIA**: typically exposes **many queues** (≈16) in its graphics family, which softens
  within-device head-of-line blocking — but kernel-level preemption behavior is opaque on the
  closed driver, so you cannot *verify* the QoS you're getting.

## 6. What skipping NVIDIA costs

Top-end raster performance and CUDA. Neither bites:

- VSE's rendering load is modest by gaming standards; a midrange AMD dGPU has ~4× the memory
  bandwidth of the Intel iGPU reference, ample for the composite pass at 4K/240 Hz (landscape
  doc §5.4 estimates).
- If in-loop ML stimulus synthesis is ever needed, the cleaner architecture is a **second GPU**
  (NVIDIA if CUDA is required) producing frames across the same external-memory seam §5.4
  defines — keeping the display GPU's driver stack boring, open, and verified.

## 7. Prior art agrees

Psychtoolbox — the only stimulus tool with comparable timing rigor — has long steered Linux
users toward AMD/Intel with open drivers for its timestamping pathways
([PTB flip-timestamp FAQ](https://github.com/Psychtoolbox-3/Psychtoolbox-3/wiki/FAQ:-Explanation-of-Flip-Timestamps)),
and the Linux VR stack (Monado, SteamVR direct mode) is developed and tested Mesa-first
([Linux VR Adventures: VR gear & GPUs](https://wiki.vronlinux.org/docs/hardware/)).

---

## References

- [Phoronix — VK_EXT_present_timing merged after five years](https://www.phoronix.com/news/VK_EXT_present_timing-Merged)
- [Khronos blog — VK_EXT_present_timing: the journey to state-of-the-art frame pacing](https://www.khronos.org/blog/vk-ext-present-timing-the-journey-to-state-of-the-art-frame-pacing-in-vulkan)
- [NVIDIA — Vulkan driver support page](https://developer.nvidia.com/vulkan-driver)
- [Phoronix forums — Mesa 26.1 merges VK_EXT_present_timing (X11 & Wayland)](https://www.phoronix.com/forums/forum/linux-graphics-x-org-drivers/vulkan/1608746-vulkan-vk_ext_present_timing-merged-to-mesa-26-1-for-x11-wayland)
- [NVIDIA forums — proprietary modules unable to acquire a DRM lease](https://forums.developer.nvidia.com/t/nvidia-proprietary-non-open-modules-completely-unable-to-acquire-a-drm-lease-on-any-display-server-all-known-nvidia-drivers-any-hardware/341244)
- [Linux VR Adventures wiki — VR gear & GPUs](https://wiki.vronlinux.org/docs/hardware/)
- [Monado — what is direct mode](https://monado.freedesktop.org/direct-mode.html)
- [Monado — frame pacing](https://monado.pages.freedesktop.org/monado/frame-pacing.html)
- [NVIDIA forums — VK_EXT_external_memory_dma_buf missing in 545](https://forums.developer.nvidia.com/t/vk-ext-external-memory-dma-buf-missing-in-545/275834)
- [NVIDIA forums — support request for VK_EXT_external_memory_dma_buf](https://forums.developer.nvidia.com/t/support-for-vk-ext-external-memory-dma-buf/199241)
- [SteamVR-for-Linux #107 — global priority / CAP_SYS_NICE](https://github.com/ValveSoftware/SteamVR-for-Linux/issues/107)
- [Collabora — implementing DRM format modifiers in NVK](https://www.collabora.com/news-and-blog/news-and-events/implementing-drm-format-modifiers-in-nvk.html)
- [Psychtoolbox — flip-timestamp FAQ](https://github.com/Psychtoolbox-3/Psychtoolbox-3/wiki/FAQ:-Explanation-of-Flip-Timestamps)
- VSE internal: `docs/timing-conformance.md`, `docs/3d-vr-rendering-landscape.md` (§3.5, §5.4), `docs/clock-synchronization.md`
