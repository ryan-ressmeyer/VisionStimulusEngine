# 3D & VR Stimulus Rendering in VisionStimulusEngine — Landscape, Tradeoffs, and Direction

**Date:** 2026-07-09
**Status:** Planning / landscape survey (no implementation committed)
**Scope:** How to add 3D-scene and (eventually) VR-haploscope rendering to VSE **without
surrendering the hardware-verified presentation-timing guarantee** that is the project's
reason to exist. Surveys game-engine internals, VR compositor pipelines, the reusable-3D
subsystem landscape and its Rust ecosystem, and prior art in scientific stimulus software.
Ends with a recommended direction (**custom haploscope panels + Bevy-rendered content +
VSE-owned direct present**) and fully-documented alternatives.

> This document is a *research synthesis and decision map*, not an implementation plan. It
> deliberately does not commit sequencing or code. It exists so the eventual design docs can
> reference a shared understanding of the landscape and the reasons behind the chosen path.

---

## 0. Context: what VSE is today

VSE is a Rust + Vulkan (vulkano 0.35.2) stimulus engine whose entire value proposition is
**deterministic, hardware-verified frame timing**:

- `VK_EXT_present_timing` on the present path (hardware-scheduled presents via
  `targetPresentTime`, per-present scanout timestamps, `VK_KHR_present_id2` correlation),
  wired through hand-rolled minimal FFI because vulkano 0.35 predates the Vulkan 1.4
  extensions (see `docs/plans/2026-07-09-ext-present-timing-design.md`).
- One calibrated clock domain: hardware scanout time ↔ VSE `Clock` ↔ `CLOCK_MONOTONIC`, so
  `submit_time`, `present_time`, scheduled targets, and external ephys timestamps are
  directly comparable.
- A direct-display backend (`VK_KHR_display`) that takes exclusive control of a physical
  display with no compositor in the path — the deterministic path for real experiments.
- A "loud fallback" posture: `ExtPresentTiming` when available, else `CpuEstimate` with an
  explicit warning written into every record. No silent degradation.
- Per-frame verification artifacts in `FlipInfo` (`present_id`, `target_time`, `on_target`)
  propagated into Parquet/CSV.

Rendering today is **2D**: gabors, noise, textured primitives (`src/drawing/`). The question
this document addresses is how to grow a **3D / VR** rendering capability on top of that
timing foundation, eventually targeting a **VR haploscope** (independent, precisely-timed
image to each eye) for **naturalistic immersive environments**.

**Target platform (locked for planning):** Linux + Mesa (AMD/Intel). This is the strongest
possible position — `VK_EXT_present_timing`, `VK_KHR_display` direct scanout, and the Monado
open-source OpenXR runtime are all fully available. Windows/macOS are explicitly out of
scope for the timing-critical path.

---

## 1. The central principle: separate *scene → pixels* from *pixels → photons*

Everything in this document follows from one architectural invariant.

A game engine does two separable jobs:

1. **Scene → pixels** — turn a 3D world (meshes, materials, lights, camera, animation,
   physics) into a finished image. This is the enormous, genuinely-hard body of work game
   engines have spent 25 years perfecting.
2. **Pixels → photons** — get that finished image onto the panel at a specific moment and
   report when it actually arrived. This is *exactly what VSE already does well* and what
   game engines deliberately do badly.

Game engines have no timing certainty because they **fuse** these jobs and let job #1's
variability (shader-compile stutter, streaming hitches, dynamic resolution, temporal
upscaling, GC stalls) leak into job #2. VSE's architecture inverts the priority: it owns
job #2 with a hardware contract.

**Therefore the merge is not "adopt a game engine and bolt timing on." It is: borrow job #1's
machinery to render into an offscreen `VkImage`, and keep VSE's swapchain/present path as the
*sole* owner of job #2.** The 3D renderer never touches the swapchain. It produces a finished
frame; VSE decides when it scans out and timestamps when it did.

This is not a workaround — it is exactly how the industry wires VR. Bevy added
`RenderTarget::TextureView` ([PR #8042](https://github.com/bevyengine/bevy/pull/8042))
specifically so OpenXR/WebXR can own the swapchain image and Bevy just renders into it. The
"present decoupling" seam is a first-class, supported pattern.

The payoff of the decoupling: the renderer's nondeterminism is *quarantined before the
handoff*. Shader compiles, culling variance, asset streaming — all perturb *what is ready
when*, never *when a ready frame appears*. VSE's existing missed-frame detection catches any
frame that wasn't ready in time and logs it loudly.

```
   ┌─────────────────────────────┐        ┌──────────────────────────────────┐
   │  SCENE → PIXELS (job #1)     │ VkImage│  PIXELS → PHOTONS (job #2)        │
   │  3D renderer (Bevy / in-house│───────▶│  VSE present authority           │
   │  Vulkan). Nondeterminism     │ handoff│  VK_EXT_present_timing,          │
   │  lives and dies here.        │        │  direct display, calibrated clock│
   └─────────────────────────────┘        └──────────────────────────────────┘
                                                    │
                                            scanout timestamp + FlipInfo
```

---

## 2. Why commercial game engines cannot deliver isochronous, verified timing

The point is not that game engines are badly built — it is that they optimize an **orthogonal
objective**: mean throughput (high average FPS) and minimum input latency, at the deliberate
cost of exactness of *when any individual frame reaches the panel*. Isochrony (every frame at
an exact fixed interval) and per-frame onset verification are non-goals, and several headline
features are actively hostile to them.

### 2.1 The deep, data-dependent pipeline

Unreal runs a pipelined multi-stage architecture where each stage handles a *different frame*
simultaneously: **game thread** (frame N+1) → **render thread** (frame N) → **RHI thread**
(translates to Vulkan/DX12, frame N or N−1) → **GPU**. Unreal calls itself a "single frame
behind" renderer, but with the RHI thread in play **2–3 frames are in flight at once**, and
*the exact number is data-dependent*
([Epic: Parallel Rendering Overview](https://dev.epicgames.com/documentation/unreal-engine/parallel-rendering-overview-for-unreal-engine),
[UE4 render-thread flow](https://ikrima.dev/ue4guide/graphics-development/render-architecture/render-thread-code-flow/)).
Pipelining exists to keep every unit busy and maximize average FPS. Its cost is that a
decision made on the game thread does not appear on screen for a *variable* number of frames —
there is no fixed, guaranteed phase relationship between logic and photons.

### 2.2 Frame pacing targets *average rate + low latency*, not exact intervals

Unreal's frame pacer predicts the next vsync and kicks off the next game frame relative to it;
its knobs are explicitly latency-vs-robustness dials. `rhi.SyncSlackMS` is documented as: a
smaller value "reduce[s] input latency at the cost of ... making it more likely for hitches to
cause dropped frames," a larger value adds latency for hitch resilience
([Epic: Low-Latency Frame Syncing](https://dev.epicgames.com/documentation/en-us/unreal-engine/low-latency-frame-syncing-in-unreal-engine)).
Dropped frames are **budgeted for**, not flagged as errors. Unity's `FrameTimingManager` is
*diagnostic telemetry*, not a scheduler — it reports per-frame CPU/GPU durations with a fixed
**four-frame delay**, and its `cpuMainThreadPresentWaitTime` explicitly folds in the wait to
hit a target FPS
([Unity docs](https://docs.unity3d.com/6000.0/Documentation/Manual/frame-timing-manager.html),
[Unity blog](https://unity.com/blog/engine-platform/detecting-performance-bottlenecks-with-unity-frame-timing-manager)).
Neither engine promises frame *k* lands exactly *k·refresh* after frame 0.

### 2.3 Adaptive features that break both pixel- and time-reproducibility

Every modern engine's flagship features assume frame content, cost, and even resolution may
vary frame-to-frame to hold an FPS target. Each is antithetical to reproducible stimuli:

- **Dynamic resolution scaling** — renders at whatever resolution keeps GPU time under budget
  ([Unity HDRP](https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/Dynamic-Resolution.html)).
  Identical content renders at *different resolutions on different runs* → not pixel-identical.
- **Temporal upscaling / AA (TAA, TSR, DLSS, FSR, XeSS)** — the displayed frame is a *function
  of frame history* accumulated via motion vectors and jittered sub-pixel samples
  ([Epic: AA & Upscaling](https://dev.epicgames.com/documentation/en-us/unreal-engine/anti-aliasing-and-upscaling-in-unreal-engine)).
  You cannot present one exact, self-contained stimulus image; DLSS/FSR also route through
  vendor neural nets that aren't bit-reproducible across hardware.
- **Variable-rate shading, runtime LOD/mesh/texture streaming** — per-frame cost becomes
  content- and heuristic-dependent; a stream-in can stall the render/RHI thread mid-frame.
- **GC stalls** — managed layers (Unreal UObject GC, Unity Mono/IL2CPP) pause at
  *non-deterministic* times for multi-ms spikes.
- **Shader / PSO compilation stutter** — the canonical modern hitch; a pipeline state object
  is compiled by the driver the first time a material/state combo is drawn, "tens of ms or
  more," stalling the thread. UE5's Nanite/Lumen generate hundreds of permutations, making
  this acute
  ([Epic tech blog](https://www.unrealengine.com/tech-blog/game-engines-and-shader-stuttering-unreal-engines-solution-to-the-problem)).
  Precaching *reduces* probability; it cannot guarantee absence.

### 2.4 What presentation control they actually expose

Mostly **retrospective statistics and a soft rate target**, never "present this exact image at
time T and confirm it appeared." The nearest platform primitives live *below* the engine
abstraction: DXGI `GetFrameStatistics` (past-frame flip times, not photons-on-panel),
`VK_GOOGLE_display_timing` (Android future-present request), and the Windows 11 composition
swapchain's present-at-timestamp API
([raphlinus: swapchain frame pacing](https://raphlinus.github.io/ui/graphics/gpu/2021/10/22/swapchain-frame-pacing.html),
[MS: DXGI_FRAME_STATISTICS](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ns-dxgi-dxgi_frame_statistics)).
None is surfaced through Unity/Unreal, and all are request-not-guarantee. This is precisely
the gap `VK_EXT_present_timing` closes — and it lives at the layer VSE owns, not the engine's.

### 2.5 The philosophical difference, stated plainly

Game engines optimize *smoothness of a variable stream* — NVIDIA Reflex minimizes the render
queue; G-Sync/VRR makes the *display* chase the engine's variable frame rate, the literal
opposite of isochrony. A stimulus engine optimizes *constancy of interval* + *verified onset*;
latency is cheap (schedule ahead) and jitter is fatal (neural responses are time-locked at
ms resolution). A game that shifts a frame by half an interval is a non-event; the same event
silently corrupts a trial. **You cannot bolt verified isochrony onto an architecture whose
every optimization assumes it is free to vary** — which is the whole argument for the §1
decoupling.

---

## 3. Why VR runtimes/compositors *compound* the problem — and the two Linux escape hatches

VR is not "3D plus a headset." It inserts a **compositor that owns the final present** between
your application and the panels, and that compositor is architecturally incompatible with
verified timing on every commercial runtime.

### 3.1 The compositor owns the present

OpenXR structures each frame as `xrWaitFrame` → `xrBeginFrame` → `xrEndFrame`. `xrWaitFrame`
*throttles the app* and returns `predictedDisplayTime`; `xrEndFrame` hands the runtime
**composition layers** (not a finished frame) and may return immediately. The runtime's
compositor then composes and drives the panel
([xrWaitFrame](https://registry.khronos.org/OpenXR/specs/1.0/man/html/xrWaitFrame.html),
[xrEndFrame](https://registry.khronos.org/OpenXR/specs/1.0/man/html/xrEndFrame.html)).
Swapchain images are runtime-created; the app only acquires/waits/releases them. **There is
no app-visible `vkQueuePresentKHR` to the panel** — so `VK_EXT_present_timing` *does not apply
on the OpenXR path at all*. Your entire timing investment is bypassed.

`predictedDisplayTime` is defined as the **midpoint** of the display interval, on a
**runtime-chosen monotonic clock** deliberately decoupled from the system clock — correlating
it with a DAQ clock requires `XR_KHR_convert_timespec_time` re-queried each frame, never a
cached offset ([XrFrameState](https://registry.khronos.org/OpenXR/specs/1.1/man/html/XrFrameState.html)).

### 3.2 Reprojection resamples every frame and can fabricate frames

Even in the best case (app hits full rate, nothing dropped), the compositor **resamples every
frame**:

- **Asynchronous Timewarp + late-latching** — the latest completed frame is re-warped against
  a head pose sampled ~3 ms before scanout, on a separate thread, *whether the app wants it or
  not* and generally **not disableable from a shipping app**
  ([Meta: ATW examined](https://developers.meta.com/horizon/blog/asynchronous-timewarp-examined/),
  [Meta: late latching](https://developers.meta.com/horizon/blog/optimizing-vr-graphics-with-late-latching/)).
  Any pixel-addressed or retinotopically-calibrated stimulus is displaced by head motion the
  experiment never commanded or logged.
- **Synthesis layers — ASW / AppSW / SteamVR Motion Smoothing / WMR reprojection** — when the
  app can't sustain rate (and sometimes always), up to **every other displayed frame is
  motion-vector extrapolated from frames the app never produced**
  ([Meta: ASW](https://developers.meta.com/horizon/blog/asynchronous-spacewarp/),
  [Meta: AppSW](https://developers.meta.com/horizon/blog/introducing-application-spacewarp/),
  [Valve: Motion Smoothing](https://www.roadtovr.com/steamvr-motion-smoothing-asw-alex-vlachos/)).
  WMR has *no "off"* — doing nothing still yields fixed-plane reprojection.

The consequences are fatal for the core experimental need: (a) the pixels are altered even at
full rate; (b) frames appear the app never rendered; (c) **you cannot know which app frame was
shown at a given photon time** — the frame index you logged at submit is not a reliable label
for what the retina received.

### 3.3 Prediction only — never a measured photon timestamp

Core OpenXR and every ratified extension give the app only the forward *prediction*. There is
no post-hoc measured scanout timestamp and no frame identifier to key one to; Meta confirms
"predictedDisplayTime ... is not a unique identifier for frames"
([Meta: frames](https://developers.meta.com/horizon/documentation/native/android/mobile-openxr-frames/)).
The timing-adjacent extensions expose only aggregates: `XR_META_performance_metrics`
(frametimes, dropped/stale counts), `XR_ANDROID_performance_metrics` (a
`motion_to_photon_latency` *scalar*), `XR_FB_display_refresh_rate` (nominal rate). None is a
per-frame photon timestamp. The Psychtoolbox maintainers, who care specifically about
ms-onset timestamping, state that as of OpenXR 1.0.26 "the current OpenXR specification does
not provide any means of reliable, trustworthy, accurate timestamping of presentation, and all
so far tested proprietary OpenXR runtime implementations have severely broken and defective
timing support," and that **only Monado on Linux provides a reliable implementation**
([PsychOpenXR](http://psychtoolbox.org/docs/PsychOpenXR)).

### 3.4 Per-eye pixel-exactness is unavailable through an OpenXR projection layer

OpenXR gives clean *logical* per-eye separation (`XR_VIEW_CONFIGURATION_TYPE_PRIMARY_STEREO`,
view 0 = left, view 1 = right, independent pose and FOV), but after submission the compositor
applies a **mandatory, non-disableable per-eye chain**: per-eye timewarp, barrel/pincushion
lens-distortion correction, and **per-color-channel chromatic-aberration correction** — all
resampling passes ([Meta: PC rendering](https://developers.meta.com/horizon/documentation/native/pc/dg-render/)).
The one nominal control bit "has no effect on any known conformant runtime, and is planned for
deprecation" ([XrCompositionLayerFlagBits](https://registry.khronos.org/OpenXR/specs/1.0/man/html/XrCompositionLayerFlagBits.html)).
For a haploscope needing pixel-exact, independently-controlled per-eye geometry (single-pixel
lines, precise disparity, isoluminant color, Nyquist-limited SF), this rules out the
projection-layer path without either exhaustive per-device calibration
([Zaman et al. 2023, *J Vis*](https://jov.arvojournals.org/article.aspx?articleid=2785700))
or bypassing the compositor entirely.

### 3.5 The two Linux escape hatches

**(a) Direct-to-panel via the Vulkan display stack.** A wired "dumb panel" HMD (Valve
Index/Vive, wired Varjo) exposes a real DRM/KMS connector. `VK_KHR_display` +
`VK_EXT_acquire_xlib_display`/`VK_EXT_direct_mode_display` + `VK_KHR_display_swapchain` let an
application *grab the panel away from the window system* and present directly, bypassing both
the desktop compositor and the VR runtime — **the exact mechanism SteamVR and Monado already
use; the compositor is not a hardware gatekeeper, just the first app to grab the DRM lease**
([VK_EXT_acquire_xlib_display](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_acquire_xlib_display.html),
[Phoronix: KeithP Vulkan direct display](https://www.phoronix.com/news/KeithP-Vulkan-Direct-Display)).
`VK_EXT_display_control` provides vblank counters and a `FIRST_PIXEL_OUT` fence for timing.
**This is VSE's existing `VK_KHR_display` direct-display architecture, pointed at an HMD panel
instead of a monitor.** HMDs deliberately no longer enumerate as monitors (a kernel
`non-desktop` EDID quirk), so acquiring them needs the direct-mode extensions, but the panel
is drivable. Prefer Mesa AMD/Intel; NVIDIA proprietary has documented DRM-lease latency and
acquisition failures.

**(b) Monado — the forkable open compositor.** Monado is the Collabora/freedesktop.org
open-source, Khronos-conformant, Vulkan-based OpenXR runtime (Boost license — a lab can fork
and redistribute), now the foundation of Google Android XR, NVIDIA CloudXR, and Qualcomm
Snapdragon Spaces ([Collabora](https://www.collabora.com/news-and-blog/news-and-events/monado-2100-officially-conformant-openxr-implementation.html),
[Monado GitLab](https://gitlab.freedesktop.org/monado/monado)). It offers: a **modifiable
frame-pacing module** (plain C in `auxiliary/util`, `u_pacing_app`/`u_pacing_compositor` —
replaceable with a fixed phase-locked schedule
[frame-pacing docs](https://monado.pages.freedesktop.org/monado/frame-pacing.html)); a
**controllable/forkable reprojection** path (`comp_render`, open Vulkan/GLSL); **exclusive
DRM/KMS panel ownership** including a bare-KMS `VkDisplayKHR` backend that takes DRM master
with no windowing system; and — decisively — **access to real scanout timestamps** via both
`VK_GOOGLE_display_timing`/`VK_KHR_present_wait` and **raw DRM page-flip completion events**
(`drm_send_vblank_event`, whose timestamp is "for the vblank right before the first frame that
scans out the new set of buffers"
[kernel DRM-KMS](https://docs.kernel.org/gpu/drm-kms.html)). This is a genuine
actual-display-time feedback path that closed runtimes hide.

**Panel-physics caveat (applies to both).** Owning scanout is necessary but not sufficient.
Low-persistence HMD panels black out at frame start and illuminate for a sub-millisecond
window after a blank period; published VR onset lags are ~18 ms
([PLOS ONE 2020](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0231152)).
Present time ≠ photon time; the illumination profile must be modeled and validated with a
photodiode.

---

## 4. The hard subsystems of a 3D renderer, and what Rust lets you buy vs. build

The subagent survey's blunt conclusion: **in Rust you can buy the *peripheral* machinery
(math, asset decode, physics, text shaping) as clean renderer-agnostic libraries, but the
*rendering systems* (scene graph, PBR, shadows, culling, IBL, skinning) do not exist as
adoptable standalone crates — they live only inside loop-owning engines.** So "merging
game-engine benefits" concretely means: buy the peripherals, steal the *techniques and formats*
for the rendering systems, and either rebuild those systems yourself or embed an engine's
renderer as a black box that draws into your image (§5).

### 4.1 Why each rendering system is genuinely hard (the textbook formula is the easy 10%)

- **Scene graph / transforms + ECS.** The tree is trivial; correctness-at-speed is not.
  World transforms must update root-to-child, so a flattened cache-friendly array requires
  parents to physically precede children, and reordering needs careful index-preserving moves
  ([Bitsquid](http://bitsquid.blogspot.com/2014/10/building-data-oriented-entity-system.html)).
  Dirty-flag propagation trades wasted recompute for *silently-stale* world positions if any
  mutation path forgets to dirty. ECS's archetype storage buys near-free iteration at the cost
  of expensive structural change, and is "generally not good at" the very tree structure a
  transform hierarchy needs ([ECS FAQ](https://github.com/SanderMertens/ecs-faq)).
- **Asset import (glTF/FBX/USD).** glTF is runtime-delivery ("the JPEG of 3D," GPU-ready);
  USD is lossless interchange; FBX is Autodesk-proprietary and a notorious time-sink
  ([Porcino](https://gist.github.com/meshula/654cac9803a37d59a88954e61091f5da)). Import is hard
  because failures are *visual, not crashes*: handedness/up-axis conversion applied
  consistently to normals/tangents/keyframes/joint-binds, unit scale, tangent-space sign
  (MikkTSpace), skinning joint-order matching inverse-bind-matrix order, and material-model
  mismatch (FBX Phong → glTF metallic-roughness is lossy).
- **PBR materials + shader permutations.** The microfacet Cook-Torrance model (GGX D, Smith
  G, Schlick F) is standard ([Filament](https://google.github.io/filament/Filament.md.html)),
  but single-scatter loses energy at high roughness (needs Kulla-Conty compensation), fp16
  cancellation near highlights needs reformulation, and roughness/reflectance need perceptual
  remapping. The *systems* half is the **permutation explosion** — independent feature toggles
  multiply into thousands–millions of variants
  ([MJP part 1](https://therealmjp.github.io/posts/shader-permutations-part1/)); every
  mitigation (uber-shaders, specialization constants, deferred rendering) is a tradeoff.
- **Culling / LOD.** Frustum culling is easy; occlusion is where decades went. HW occlusion
  queries stall the pipeline; Hierarchical-Z is a chicken-and-egg two-pass with reprojection
  gaps; GPU-driven culling adds atomic contention; mesh shaders required a *hardware
  architecture change* to cull fine-grained; LOD popping has no closed-form fix (Nanite was
  newsworthy for a reason)
  ([RasterGrid HZB](https://www.rastergrid.com/blog/2010/10/hierarchical-z-map-based-occlusion-culling/),
  [NVIDIA mesh shaders](https://developer.nvidia.com/blog/introduction-turing-mesh-shaders/)).
- **Lighting / shadows.** Shadow-map bias is a genuine no-win (acne vs. peter-panning);
  cascaded shadow maps flicker unless the projection is texel-snapped and sphere-fit; real-time
  GI has no exact solution (baked/probes/voxel/RT are all compromises); IBL needs Karis's
  split-sum with careful importance sampling to avoid fireflies
  ([Microsoft shadow techniques](https://learn.microsoft.com/en-us/windows/win32/dxtecharts/common-techniques-to-improve-shadow-depth-maps),
  [stable CSM](http://longforgottenblog.blogspot.com/2014/12/rendering-post-stable-cascaded-shadow.html),
  [LearnOpenGL IBL](https://learnopengl.com/PBR/IBL/Specular-IBL)).
- **Skeletal animation.** Linear blend skinning is mathematically flawed (candy-wrapper
  collapse); dual-quaternion skinning fixes it with its own tradeoffs; blend trees need
  phase-syncing or feet skate
  ([Kavan et al.](https://users.cs.utah.edu/~ladislav/kavan07skinning/kavan07skinning.pdf)).
- **Text + camera math.** Text hides a 20-year shaping problem (HarfBuzz) beneath apparent
  triviality; projection math is a field of silent-wrongness bugs, and **Vulkan's clip space
  (Z ∈ [0,w], flipped NDC-Y, right-handed) differs from OpenGL's** — reusing an OpenGL
  projection renders inside-out with mis-mapped depth. Reversed-Z + float depth nearly
  eliminates z-fighting
  ([Khronos: VK vs GL coords](https://www.khronos.org/news/permalink/handling-differences-between-vulkan-and-opengl-coordinate-system),
  [NVIDIA: depth precision](https://developer.nvidia.com/blog/visualizing-depth-precision/)).

### 4.2 Buy vs. build map (Rust)

| Subsystem | Verdict | Crate / approach |
|---|---|---|
| Math (vec/mat/quat, projection) | **Buy** | `glam` — SIMD, Vulkan-aware `perspective_rh`/`look_at_rh`; ecosystem standard (Bevy/Rapier re-export it) |
| glTF loading | **Buy** | `gltf` (+ `image`) — decodes buffers & images; you do GPU upload |
| OBJ / other formats | **Buy** | `tobj` (OBJ); `russimp-ng` only if FBX/exotic needed (native Assimp dep) |
| Texture decode | **Buy** | `image` |
| Physics | **Buy** | `rapier3d` — best-in-class, glam API, very active |
| Collision queries | **Buy** | `parry3d` — pure geometry, use standalone if no dynamics |
| ECS (optional) | **Buy or skip** | `bevy_ecs` / `hecs` standalone; a plain scene struct may be simpler for a stimulus engine |
| Text shaping + raster | **Buy** | `rustybuzz`/`harfbuzz-rs` + `fontdue`/`ab_glyph`/`glyph_brush` — never build shaping |
| GPU abstraction (optional) | **Buy carefully** | `wgpu` via `wgpu-hal` shared-device interop (see §5) — `unsafe`, unstable, ash-level glue |
| **Scene graph / transforms** | **Build** | No crate; small, data-coupled — explicit ordering + dirty flags |
| **PBR material + shader system** | **Build** | No crate; metallic-roughness Cook-Torrance per Filament |
| **IBL (BRDF LUT / prefilter)** | **Build** | No crate; split-sum precompute passes |
| **Shadows (maps / CSM)** | **Build** | No crate; slope-scaled bias + stabilized cascades |
| **Culling / LOD** | **Build** | No crate; frustum first, HZB occlusion only if profiling demands |
| **Skeletal animation** | **Build** | No clean crate; `gltf` gives data, implement LBS/DQS |
| VR / OpenXR | **Buy bindings** | `openxr` crate; study `hotham` as reference; accept compositor owns present, use Monado |

The strategic consequence, restated: **because the rendering systems are build-either-way, the
choice of GPU API (raw vulkano vs. wgpu) is far less pivotal than which *renderer* produces
your image.** That is what §5 decides.

---

## 5. The three renderer strategies

All three honor the §1 invariant (VSE owns present). They differ in *what produces the
offscreen image*.

### Strategy A — In-house Vulkan renderer

Extend VSE's own vulkano renderer; borrow `glam`/`gltf`/`rapier`; build the rendering systems
per §4.

- **Get for free:** nothing rendering-wise — total control.
- **Build:** every rendering system (scene graph, PBR, shadows, culling, skinning, IBL).
- **Determinism:** maximal, by construction — you never write a nondeterministic feature.
- **Integration tax:** low (pure vulkano; no new abstraction on the device).
- **Best for:** controlled parametric 3D (depth/disparity/motion probes, textured primitives,
  simple imported meshes). Grows expensive fast if you need naturalistic lighting/materials.

### Strategy B — wgpu-hal shared-device (offscreen)

Import VSE's existing `ash` `VkInstance`/`VkDevice`/queue into wgpu via
`create_instance_from_hal`/`create_device_from_hal`; wrap your own `VkImage` as a wgpu texture
via `create_texture_from_hal` (`Device::texture_from_raw`, with drop-guards so wgpu won't
destroy handles it doesn't own); render offscreen; **VSE keeps the swapchain and present**
([wgpu PR #1850](https://github.com/gfx-rs/wgpu/pull/1850),
[wgpu issue #6142](https://github.com/gfx-rs/wgpu/issues/6142)). This interop path was *built
for* externally-owned swapchains (OpenXR/SteamVR).

- **Get for free:** a portable modern shader stack (WGSL) and a cleaner GPU abstraction —
  **but still every scene system per §4.**
- **Build:** every rendering system (same as A).
- **Determinism:** maximal (you still write the systems).
- **Integration tax:** medium — `unsafe`, explicitly unstable wgpu-hal API (pin a version;
  signatures churn), ash↔vulkano glue (vulkano exposes raw handles; you own the marshalling),
  and a *fourth* consumer of the one `VkDevice` (vulkano + raw present-timing FFI + wgpu).
  Image-layout/semaphore synchronization across the boundary is on you.
- **Best for:** a portability hedge, or as the **enabling substrate for Strategy C** (see
  below). On its own it buys an API, not the systems you must write — so as an end in itself it
  is the weakest-value option.

`wgpu::Surface` is *not* usable — it wraps `vkQueuePresentKHR` with no `VK_EXT_present_timing`
hook. Only the below-`Surface` HAL path preserves present control. (Separate-device interop via
`VK_KHR_external_memory`/`vkGetMemoryFdKHR` exists but is only needed across devices/processes;
wgpu's texture-side external-memory import is still unfinished — prefer shared-device.)

### Strategy C — Bevy rendering into *your* image (the recommended primary)

This is the literal "merge": Bevy's mature scene graph, PBR, glTF pipeline, and culling render
into a `VkImage` **you own and VSE presents with verified timing**. Mechanically it is
Strategy B's interop plus Bevy consuming the result:

1. Create wgpu on VSE's `ash` device (Strategy B).
2. Create a `VkImage`, wrap it as a wgpu `Texture`, take a `TextureView`.
3. Hand that `TextureView` to a Bevy camera as `RenderTarget::TextureView`
   ([PR #8042](https://github.com/bevyengine/bevy/pull/8042) — the OpenXR/WebXR path).
4. Run Bevy **headless** (no winit — [without_winit example](https://github.com/bevyengine/bevy/blob/main/examples/app/without_winit.rs)),
   stepping its schedule manually so *you* drive when a frame is produced (render-on-demand,
   not free-running).
5. VSE presents the finished image on its own `VK_EXT_present_timing` path.

- **Get for free:** the actual prize — Bevy 0.19's `bevy_pbr`, scene graph, `bevy_asset` glTF
  pipeline, culling, and the a-la-carte `bevy_ecs`/`bevy_math`/`bevy_asset` crates
  ([Bevy 0.19](https://bevy.org/news/bevy-0-19/)). MIT/Apache-2.0.
- **Build:** mostly nothing rendering-wise; instead you *audit and disable* Bevy's
  nondeterministic features (TAA/temporal, any dynamic scaling), pre-warm pipelines, and pin
  RNG. Plus the interop glue from Strategy B.
- **Determinism:** must be **clawed back** from Bevy's renderer — the central risk of this
  path. Because the frame is handed over *before* present (§1), residual nondeterminism
  perturbs *what is ready when*, not *when it appears* — but a stimulus must also be
  *pixel-reproducible*, so temporal/adaptive features must genuinely be off, not just
  timing-irrelevant.
- **Integration tax:** high + **upstream risk**. Headless + external-`TextureView` target +
  manual schedule-stepping is at the frontier of Bevy's supported usage. Bevy's `RenderApp`
  sub-app expects to own the winit loop; driving it from an external swapchain is a
  long-standing *unsupported* request
  ([bevy #12159](https://github.com/bevyengine/bevy/issues/12159)). **This must be de-risked
  with a spike before it is committed** (see §8).
- **Best for:** naturalistic immersive environments — the stated near-term driver — without
  writing a renderer.

**Why C over a Unity/Unreal shared-memory bridge:** same language (Rust), same GPU abstraction
(wgpu), *single device* — no cross-process, no external-memory marshalling, no engine
determinism-audit across an opaque C++ boundary. The Unity/Unreal-as-content-renderer path
(§7.1) is strictly heavier on every axis and is documented as an alternative, not a
recommendation.

### Strategy comparison

| | **A. In-house Vulkan** | **B. wgpu-hal** | **C. Bevy → your image** |
|---|---|---|---|
| Free rendering systems | none | none | Bevy PBR / scene graph / assets / culling |
| Systems you build | all | all | ~none (you *disable* features) |
| Determinism | maximal | maximal | clawed back (main risk) |
| Integration tax | low | medium | high + upstream risk |
| Present control kept | ✅ | ✅ | ✅ |
| Best for | parametric 3D | portability hedge / C's substrate | naturalistic scenes |

---

## 6. Prior art: the timing-precision landscape of scientific stimulus tools

Understanding where VSE sits — and what it can uniquely claim — requires the prior-art map.

### 6.1 The spectrum

| Tier | Tools | What they deliver |
|---|---|---|
| **A-priori verified (flat displays only)** | **Psychtoolbox-3**; **PsychoPy** close behind | Beamposition/OpenML (PTB) or frame-locked VBL sync. Sub-ms in ideal Linux/fullscreen conditions (mega-study: PTB 0.18 ms, PsychoPy 0.34 ms visual SD [[Bridges et al. 2020, *PeerJ*]](https://peerj.com/articles/9414/)). **Guarantee evaporates in VR.** |
| **Deterministic hardware add-on** | VPixx Pixel Sync, Cedrus, Black Box ToolKit | Not engines — hardware that makes timing *measurable/correctable* to µs via shared-clock triggers + photodiode. |
| **Rigorous measure-after-the-fact** | **bonVision**, **USE** | Disclaim a-priori guarantees; measure true onset with a corner photodiode on a shared timebase and publish the distribution (bonVision ~2–2.7 frames [[eLife 2021]](https://elifesciences.org/articles/65541); USE reconstructs every frame offline, correcting 0–20 ms Unity error [[Watson et al. 2019]](https://doi.org/10.1016/j.jneumeth.2019.05.002)). |
| **Best-effort + user correction** | UXF, Vizard/WorldViz, rodent VR (ViRMEn, ratCAVE, MouseGoggles), Godot | Log at whatever rate the loop runs; frame-locked, no onset guarantee. MouseGoggles ([*Nat. Methods* 2024](https://www.nature.com/articles/s41592-024-02540-y)) even concedes <130 ms is too slow for fast closed-loop work. |

### 6.2 The two structural facts that define the gap

1. **The one tool with real a-priori guarantees (PTB) loses them the moment you enter VR.**
   OpenXR provides "no means for reliable ... onset timestamping," and every proprietary
   runtime tested had broken timing (§3.3). **No tool today offers PTB-grade verified onset
   timing in a 3D/VR/HMD context.**
2. **Everyone else's answer to "when did the frame actually appear?" is a photodiode taped to
   a screen corner, read out offline** on the ephys clock. This is the universal fallback —
   from Unity/USE to bonVision to *PTB itself* (which ships `PsychPhotodiode`). It works but is
   post-hoc, adds hardware, and (in the game-engine world) corrects errors of 0–20 ms that are
   non-repeatable frame to frame. Foundational reference:
   [Elze 2010, *J Neurosci Methods*](https://pubmed.ncbi.nlm.nih.gov/20600318/).

### 6.3 Where VSE differentiates

A per-frame, **per-eye** hardware scanout timestamp (`VK_EXT_present_timing`, or Monado's DRM
page-flip events) moves VR/3D stimulus timing from "best-effort + external photodiode" toward
"a-priori verified" — **in the exact regime where even Psychtoolbox cannot.** For a haploscope,
where per-eye independence and inter-ocular synchronization are first-class requirements
(classically solved with LC shutter goggles synced to refresh
[[Scholarpedia: binocular rivalry]](http://www.scholarpedia.org/article/Binocular_rivalry)),
a per-swapchain hardware timestamp is the natural primitive, and no surveyed tool provides it.

**Honest framing of the goal:** the aim is *not* to eliminate the photodiode — even PTB keeps
one as ground truth, and panel physics (§3.5) mean present time ≠ photon time regardless. The
aim is to make the hardware timestamp trustworthy enough that the **photodiode becomes a
validation check, not the primary source of truth** — and to have it *per eye, in VR*, which
no one does today.

---

## 7. Alternatives, documented (not recommended, but on the record)

### 7.1 Unity/Unreal as a content renderer + shared-memory handoff + bolted-on timing

Run a real engine purely for **scene → pixels**, share rendered frames to VSE via
`VK_KHR_external_memory`, and let VSE present with timing.

- **Appeal:** the richest asset-authoring tooling and artist pipelines; naturalistic content
  "for free."
- **Costs:** cross-process/cross-device external-memory sharing; a determinism audit across an
  opaque C++ engine you cannot fully control (§2.3 — dynamic-res, TAA, streaming, GC, PSO
  stutter must all be forced off, and some cannot be); the engine must render *on demand*
  (render-to-texture, one self-contained frame per request), not free-run. Measured
  code→pixel latency is ~10.8 ms (Unity) / 21.1 ms (Unreal) [[Kang & Wallraven 2023,
  arXiv:2306.02637]](https://arxiv.org/abs/2306.02637) — tolerable if *scheduled ahead*, but
  the jitter/nondeterminism is the real problem, not the mean.
- **Verdict:** strictly heavier than Strategy C on every axis (language boundary, process
  boundary, device boundary, determinism-audit surface) with no compensating advantage for a
  Linux/Rust lab. Reconsider only if a specific artist/content pipeline is unavailable in the
  Bevy ecosystem. This is the canonical "USE pattern" (§6.1) applied to rendering — and USE
  itself only survives Unity's nondeterminism via a photodiode + offline reconstruction.

### 7.2 Commercial VR gear via a vendor OpenXR runtime (SteamVR / Quest / WMR)

- **Appeal:** cheapest hardware, easiest bring-up, broad device support, existing eye-tracking.
- **Costs:** *disqualifying for verified timing* — compositor owns present (§3.1),
  prediction-only display time (§3.3), mandatory reprojection/distortion/chromatic resampling
  (§3.2, §3.4), no `VK_EXT_present_timing` on the path. Valid only for higher-level dichoptic
  manipulations (contrast/pattern rivalry) with photodiode correction, not pixel-exact
  geometry. Published VR haploscope work (strabismus/phoria/suppression on Vive/Quest)
  operates in exactly this "good-enough for clinical correlation, not for ms-timed neural
  alignment" regime
  ([Vive prism cover test](https://pmc.ncbi.nlm.nih.gov/articles/PMC8178882/)).
- **Verdict:** acceptable *only* for prototyping or non-timing-critical pilots. If commercial
  HMDs are needed for timing-critical work, the answer is **Monado (§3.5b), not a vendor
  runtime.**

### 7.3 In-house-only renderer for naturalistic scenes (Strategy A pushed to the rich end)

- **Appeal:** maximal control and determinism; no external-crate churn or upstream risk.
- **Costs:** you rebuild PBR + IBL + shadows + culling + material system + asset pipeline —
  the exact 90%-hard systems §4.1 catalogues — to reach naturalistic quality. Years of work
  Bevy already did.
- **Verdict:** the right call *only if* Strategy C's determinism-clawback proves intractable
  (the spike fails). Otherwise it re-solves solved problems. Keep as the fallback for the rich
  end; keep A as the *actual* near-term path for parametric stimuli regardless.

### 7.4 rend3 / Fyrox / Rafx / hotham as the renderer

- **rend3:** archived read-only (June 2025), last release Feb 2022 — **do not adopt.**
- **Fyrox 1.0:** whole-application engine, owns the loop, ships its *own OpenGL* renderer —
  cannot render into a vulkano/wgpu context. Only leaf crates (`fyrox-sound`,
  `fyrox-animation`) are plausibly liftable.
- **Rafx:** dormant (commits stalled Aug 2024), brings its *own* Vulkan device — won't share
  VSE's.
- **hotham:** full OpenXR+Vulkan VR engine (ash + `openxr` crate, `hecs` ECS); owns the
  session/swapchain/loop — all-or-nothing. **Best used as a *reference implementation*** for
  driving OpenXR+ash from Rust, not as a library.

---

## 8. Recommended direction and rationale

**Direction: custom haploscope panels + Bevy-rendered content + VSE-owned direct present.**
Concretely:

1. **Present authority stays VSE, always (§1).** No renderer ever touches the swapchain. The
   scene→image→present handoff is a hard `VkImage` boundary. This is non-negotiable and
   backend-independent.

2. **Renderer: Strategy A now, Strategy C as the naturalistic target.** Build a small
   deterministic in-house Vulkan renderer for controlled parametric 3D first (it forces the
   handoff seam to be designed cleanly and covers parametric science immediately). **Design
   that seam as an identical `VkImage` boundary so Strategy C — Bevy rendering into that same
   image — is a drop-in later, changing only the producer.** Given the stated near-term driver
   is *naturalistic environments*, prioritize de-risking C early (below) rather than investing
   heavily in in-house PBR.

3. **VR/display: custom two-panel direct-display backend first; Monado for HMDs, ready but
   uncommitted.** The custom-panel path is pure reuse of VSE's `VK_KHR_display` strength and
   gives the strongest per-eye timing (§3.5a). Monado (§3.5b) is the researched, documented
   answer if/when a commercial HMD is needed — and on Linux+Mesa it is fully open. Vendor
   OpenXR runtimes are prototyping-only (§7.2). The two VR paths are *two presentation backends
   behind one renderer*, mirroring VSE's existing direct-vs-composited split.

4. **A written determinism contract, enforced on every path (§2.3):** no temporal AA /
   upscaling, no dynamic resolution, all PSOs/shaders precompiled before a trial (fail closed
   if a compile happens mid-trial), no runtime LOD swaps in the presented frame, deterministic
   seeded RNG, no synchronous asset streaming during presentation, allocation-free hot path
   (Rust's ownership model is a real advantage — no tracing GC). This is a design constraint
   from line one, not a later cleanup.

5. **Sequencing (asserted as principle, not schedule): 3D does not begin until the
   `feat/ext-present-timing` reorg is solid.** Per-eye verified present is the foundation the
   entire 3D/VR story stands on; building scene rendering on an unverified present path would
   invert the project's priorities.

### The one thing that must be de-risked before committing to C

Strategy C's viability rests on an **unproven-for-us** capability: running Bevy **headless**,
targeting an **externally-owned `TextureView`** backed by a VSE-owned `VkImage` on a
**shared `ash` device**, with **manual schedule-stepping** for render-on-demand, and with its
nondeterministic features genuinely disabled. Each piece is individually documented as
possible; the *combination* is at the frontier of Bevy's supported usage
([bevy #12159](https://github.com/bevyengine/bevy/issues/12159)). **The next concrete step,
when 3D work begins, is a throwaway spike proving this end-to-end** — render one Bevy PBR
scene into a VSE-presented image with a verified scanout timestamp — *before* any design doc
commits to C. If the spike fails, the documented fallback is Strategy A pushed toward the rich
end (§7.3), accepting the larger build cost.

---

## 9. Open questions / risks

- **Bevy determinism-clawback (highest risk).** Can every temporal/adaptive feature be
  disabled such that the *same scene state renders the same pixels every run*? Needs
  measurement (render twice, diff frames) during the spike.
- **Shared-device object lifetime.** Three consumers of one `VkDevice` (vulkano + present-timing
  FFI + wgpu-hal). Drop-guard discipline and layout/semaphore correctness across the wgpu↔VSE
  boundary must be validated with validation layers on.
- **wgpu-hal API instability.** Signatures churn between releases; pin a version and budget for
  interop-glue maintenance.
- **Per-eye synchronization on custom panels.** Two direct-display swapchains, two present
  timelines — inter-ocular sync becomes a first-class timing problem (both eyes targeting the
  same absolute onset). VSE's calibrated single clock domain is the right substrate, but the
  two-swapchain scheduling model needs its own design.
- **Panel physics (§3.5 caveat).** Low-persistence illumination timing must be characterized
  per panel; the hardware timestamp is present time, not photon time. Photodiode validation
  remains necessary.
- **Haploscope optics/geometry** (mirrors, per-eye calibration, vergence) are out of scope
  here but interact with the renderer's per-eye camera model.
- **Monado fork maintenance** (if the HMD path is taken) — surfacing DRM page-flip timestamps
  is a real but bounded C change to an actively-developed upstream; budget for rebasing.

---

## 10. References

### VSE internal
- `docs/plans/2026-07-09-ext-present-timing-design.md` — the present-timing foundation.
- `docs/timing.md` — timing philosophy and tiers.
- `docs/guides/display_backends.md` — direct-display (`VK_KHR_display`) architecture.

### Game-engine timing
- Epic — [Parallel Rendering Overview](https://dev.epicgames.com/documentation/unreal-engine/parallel-rendering-overview-for-unreal-engine),
  [Low-Latency Frame Syncing](https://dev.epicgames.com/documentation/en-us/unreal-engine/low-latency-frame-syncing-in-unreal-engine),
  [Anti-Aliasing & Upscaling](https://dev.epicgames.com/documentation/en-us/unreal-engine/anti-aliasing-and-upscaling-in-unreal-engine),
  [Shader Stuttering / PSO](https://www.unrealengine.com/tech-blog/game-engines-and-shader-stuttering-unreal-engines-solution-to-the-problem).
- Unity — [Frame Timing Manager](https://docs.unity3d.com/6000.0/Documentation/Manual/frame-timing-manager.html),
  [Dynamic Resolution (HDRP)](https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/Dynamic-Resolution.html).
- [raphlinus — Swapchain frame pacing](https://raphlinus.github.io/ui/graphics/gpu/2021/10/22/swapchain-frame-pacing.html);
  [MS — DXGI_FRAME_STATISTICS](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ns-dxgi-dxgi_frame_statistics).

### VR / OpenXR / compositor
- Khronos OpenXR — [xrWaitFrame](https://registry.khronos.org/OpenXR/specs/1.0/man/html/xrWaitFrame.html),
  [xrEndFrame](https://registry.khronos.org/OpenXR/specs/1.0/man/html/xrEndFrame.html),
  [XrFrameState](https://registry.khronos.org/OpenXR/specs/1.1/man/html/XrFrameState.html),
  [XrCompositionLayerFlagBits](https://registry.khronos.org/OpenXR/specs/1.0/man/html/XrCompositionLayerFlagBits.html).
- Meta — [Async Timewarp Examined](https://developers.meta.com/horizon/blog/asynchronous-timewarp-examined/),
  [Late Latching](https://developers.meta.com/horizon/blog/optimizing-vr-graphics-with-late-latching/),
  [ASW](https://developers.meta.com/horizon/blog/asynchronous-spacewarp/),
  [AppSW](https://developers.meta.com/horizon/blog/introducing-application-spacewarp/),
  [PC Rendering](https://developers.meta.com/horizon/documentation/native/pc/dg-render/),
  [OpenXR frames](https://developers.meta.com/horizon/documentation/native/android/mobile-openxr-frames/).
- Valve — [SteamVR Motion Smoothing](https://www.roadtovr.com/steamvr-motion-smoothing-asw-alex-vlachos/).
- Direct display — [VK_EXT_acquire_xlib_display](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_acquire_xlib_display.html),
  [Phoronix: KeithP Vulkan Direct Display](https://www.phoronix.com/news/KeithP-Vulkan-Direct-Display),
  [Monado direct-mode](https://monado.freedesktop.org/direct-mode.html).
- Monado — [GitLab](https://gitlab.freedesktop.org/monado/monado),
  [frame pacing](https://monado.pages.freedesktop.org/monado/frame-pacing.html),
  [Collabora conformance](https://www.collabora.com/news-and-blog/news-and-events/monado-2100-officially-conformant-openxr-implementation.html);
  [kernel DRM-KMS page-flip events](https://docs.kernel.org/gpu/drm-kms.html).
- [Psychtoolbox PsychOpenXR](http://psychtoolbox.org/docs/PsychOpenXR);
  [PLOS ONE 2020 — VR display timing](https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0231152);
  [Zaman et al. 2023 — HMD calibration for vision research, *J Vis*](https://jov.arvojournals.org/article.aspx?articleid=2785700).

### 3D subsystems & Rust ecosystem
- [ECS FAQ](https://github.com/SanderMertens/ecs-faq);
  [Bitsquid data-oriented entity system](http://bitsquid.blogspot.com/2014/10/building-data-oriented-entity-system.html);
  [Game Programming Patterns — Dirty Flag](https://gameprogrammingpatterns.com/dirty-flag.html).
- [Khronos glTF](https://www.khronos.org/gltf/);
  [glTF 2.0 spec](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html);
  [Porcino — asset interchange](https://gist.github.com/meshula/654cac9803a37d59a88954e61091f5da).
- [Filament PBR](https://google.github.io/filament/Filament.md.html);
  [LearnOpenGL PBR](https://learnopengl.com/PBR/Theory), [IBL](https://learnopengl.com/PBR/IBL/Specular-IBL);
  [MJP shader permutations](https://therealmjp.github.io/posts/shader-permutations-part1/).
- [RasterGrid HZB culling](https://www.rastergrid.com/blog/2010/10/hierarchical-z-map-based-occlusion-culling/);
  [NVIDIA mesh shaders](https://developer.nvidia.com/blog/introduction-turing-mesh-shaders/);
  [stable cascaded shadow maps](http://longforgottenblog.blogspot.com/2014/12/rendering-post-stable-cascaded-shadow.html);
  [MS shadow depth-map techniques](https://learn.microsoft.com/en-us/windows/win32/dxtecharts/common-techniques-to-improve-shadow-depth-maps).
- [Kavan et al. — geometric skinning](https://users.cs.utah.edu/~ladislav/kavan08geometric/kavan08geometric.pdf);
  [HarfBuzz — why a shaping engine](https://harfbuzz.github.io/why-do-i-need-a-shaping-engine.html).
- [Khronos — Vulkan vs OpenGL coordinates](https://www.khronos.org/news/permalink/handling-differences-between-vulkan-and-opengl-coordinate-system);
  [NVIDIA — depth precision / reversed-Z](https://developer.nvidia.com/blog/visualizing-depth-precision/).
- Rust crates — [wgpu](https://github.com/gfx-rs/wgpu) ([HAL interop PR #1850](https://github.com/gfx-rs/wgpu/pull/1850),
  [issue #6142](https://github.com/gfx-rs/wgpu/issues/6142)),
  [Bevy 0.19](https://bevy.org/news/bevy-0-19/) ([RenderTarget::TextureView PR #8042](https://github.com/bevyengine/bevy/pull/8042),
  [without_winit](https://github.com/bevyengine/bevy/blob/main/examples/app/without_winit.rs),
  [external-swapchain issue #12159](https://github.com/bevyengine/bevy/issues/12159)),
  [glam](https://docs.rs/glam), [gltf-rs](https://github.com/gltf-rs/gltf),
  [rapier](https://lib.rs/crates/rapier3d), [parry](https://github.com/dimforge/parry),
  [hotham](https://github.com/leetvr/hotham), [openxrs](https://github.com/Ralith/openxrs),
  [rend3 (archived)](https://github.com/bve-reborn/rend3), [Fyrox](https://github.com/FyroxEngine/Fyrox).

### Scientific stimulus tools & timing
- [Kleiner et al. 2007 — What's new in Psychtoolbox-3](https://pure.mpg.de/rest/items/item_1790332/component/file_3136265/content);
  [PTB flip-timestamp FAQ](https://github.com/Psychtoolbox-3/Psychtoolbox-3/wiki/FAQ:-Explanation-of-Flip-Timestamps).
- [Peirce et al. 2019 — PsychoPy2](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC6420413/);
  [Bridges et al. 2020 — The timing mega-study, *PeerJ*](https://peerj.com/articles/9414/).
- [Lopes et al. 2021 — BonVision, *eLife*](https://elifesciences.org/articles/65541);
  [Lopes et al. 2015 — Bonsai](https://www.frontiersin.org/journals/neuroinformatics/articles/10.3389/fninf.2015.00007/full).
- [Watson et al. 2019 — USE, *J Neurosci Methods*](https://doi.org/10.1016/j.jneumeth.2019.05.002);
  [Brookes et al. 2020 — UXF](https://link.springer.com/article/10.3758/s13428-019-01242-0);
  [Kang & Wallraven 2023 — Gotta Go Fast](https://arxiv.org/abs/2306.02637).
- Rodent/VR — [Aronov & Tank 2014 — ViRMEn](https://pmc.ncbi.nlm.nih.gov/articles/PMC4454359/);
  [Del Grosso & Sirota 2019 — ratCAVE](https://pmc.ncbi.nlm.nih.gov/articles/PMC6797704/);
  [MouseGoggles, *Nat Methods* 2024](https://www.nature.com/articles/s41592-024-02540-y).
- Haploscopes/dichoptic — [Wheatstone stereoscope](https://en.wikipedia.org/wiki/Wheatstone_stereoscope);
  [binocular rivalry (Scholarpedia)](http://www.scholarpedia.org/article/Binocular_rivalry);
  [VR prism cover test](https://pmc.ncbi.nlm.nih.gov/articles/PMC8178882/).
- Photodiode pattern — [Elze 2010, *J Neurosci Methods*](https://pubmed.ncbi.nlm.nih.gov/20600318/);
  [PTB PsychPhotodiode](http://psychtoolbox.org/docs/PsychPhotodiode);
  [VPixx Pixel Sync](https://docs.vpixx.com/vocal/using-pixel-sync-for-stimulus-accurate-timing).

---

## Appendix — sourcing caveats (from the research pass)

- OpenXR spec quotes were cross-confirmed against the docs.vulkan.org mirror and KhronosGroup
  GitHub source where `registry.khronos.org` returned fetch errors; normative text is
  identical.
- Filament BRDF formulas reached the survey via a doc summarizer — **verify against the
  Filament source before transcribing into shader code.**
- ratCAVE (~15 ms) and MouseGoggles (<130 ms / 80 fps) latency figures have weak measurement
  documentation in their sources — treat as approximate.
- "Vizard built on OpenSceneGraph" and a standard "Bonsai+Unity" neuroscience integration were
  **not** confirmed in primary sources; do not rely on them.
- Godot in neuroscience beyond MouseGoggles is genuinely sparse — that absence is a finding,
  not a search gap.
