# Psychtoolbox demo content deferred from the VSE demo suite

This is a reference catalogue of functionality that Psychtoolbox-3 (PTB) demonstrates
in its `PsychDemos/` tree but that the curated VSE demo curriculum **deliberately does
not cover** in its first pass. It exists so that a later decision to implement any of
these has a starting point: what the PTB demos actually show, why it was deferred, what
infrastructure VSE would need first, and a rough sense of effort.

Nothing here is a commitment. The VSE demo curriculum (`examples/`) intentionally
consolidates ~60 PTB demos into ~20 demos plus the timing/reproducibility demos PTB has
no equivalent for. The items below are the parts of PTB's surface we chose to leave out.

For the categories we *did* cover, and the mapping from PTB demos to VSE demos, see the
`examples/` curriculum itself.

---

## Ranking (if we ever revisit)

Rough order of scientific value-to-effort for the VSE target user (primate visual
neuroscience, timing-critical):

1. **Gamma / color calibration tooling** — high value, medium-large effort. Directly
   serves reproducibility and cross-lab comparability.
2. **Movie / video playback** — high value, large effort. Naturalistic-stimulus
   experiments need it.
3. **2D stereo** — medium value, medium effort. Some labs need dichoptic presentation;
   the 3D/VR path (Bevy) partially overlaps.
4. **HDR** — medium value, medium effort. Relevant for high-luminance / wide-gamut
   displays used in some rig setups.
5. **Audio** — lower value for pure visual work, medium effort. Matters for
   audiovisual and cueing paradigms.
6. **Live video capture** — low value for most stimulus work, large effort. Mostly a
   gaze/eye-camera and closed-loop concern.
7. **External-hardware trigger boxes** — low value given VSE's photodiode-native model
   already covers the canonical acquisition-clock alignment.

---

## 1. Gamma / color calibration and color science

**PTB demos:** `CalDemo`, `FitGammaDemo`, `DKLDemo`, `ClutAnimDemo`,
`IsomerizationsInEyeDemo`, `IsomerizationsInDishDemo`, `NomogramDemo`,
`PhotopigmentNomogramDemo`, `ValetonVanNorrenDemo`.

**What they show:** measuring a display's gamma response, fitting gamma functions,
building/loading calibration files, converting between device RGB and physiologically
meaningful color spaces (DKL, cone isomerizations), photopigment nomograms, and
CLUT-based animation.

**Why deferred:** VSE has no gamma-LUT / color-space layer yet. `draw_*` colors are
linear device values written straight to the swapchain. This is the single most
scientifically important gap for reproducibility, but it is a whole subsystem, not a
demo.

**Infrastructure VSE would need:**
- A per-channel gamma LUT applied at present time (ideally in the presentation shader,
  so it does not perturb timing).
- A calibration file format that travels with session metadata (fits the existing
  host/session logging model).
- Color-space conversion utilities (device RGB ↔ XYZ ↔ DKL / cone space) with
  measured primaries.
- Optionally a measurement loop driving a photometer/spectroradiometer (hardware).

**Effort:** medium-large. The LUT + calibration-file plumbing is tractable; the full
color-science stack (nomograms, isomerizations) is a large numerical library port.

**Suggested first demo if revisited:** `gamma_calibration` — measure/load a gamma
table, show corrected vs. uncorrected ramps, record the calibration in session
metadata. Consolidates `FitGammaDemo` + `CalDemo` + `ClutAnimDemo`.

---

## 2. Movie / video playback

**PTB demos:** `SimpleMovieDemo`, `PlayMoviesDemo`, `PlayMoviesWithoutGapDemo1/2`,
`LoadMovieIntoTexturesDemo`, `PlayDualMoviesDemo`, `PlayInterlacedMovieDemo`,
`DetectionRTInVideoDemo`, plus the `MovieDemos/` and `PsychTutorials/PlayDualMovies`
material.

**What they show:** decoding a video file to a stream of textures, presenting frames
in sync with the display, gapless playback across clips, multiple simultaneous movies,
and reaction-time collection over a playing video.

**Why deferred:** VSE has no video decode path. This matters for naturalistic-stimulus
work and is high value, but it is a substantial subsystem with its own timing
subtleties (decode jitter vs. scanout clock).

**Infrastructure VSE would need:**
- A decoder (e.g. `ffmpeg`/`libav` binding, or a Rust decoder) producing RGBA frames.
- A frame queue that hands decoded frames into the existing texture upload path — the
  external-frame ring machinery (`vse-external-frame`, used by the Bevy integration) is
  a natural fit: a decoder becomes just another external frame producer.
- A policy for frame selection under the scanout clock (present the frame whose
  presentation time is closest to the scheduled onset), reusing the latest-ready /
  hold-last policies already implemented.

**Effort:** large (decoder integration + timing policy), but the frame-handoff seam
already exists.

**Suggested first demo if revisited:** `movie_playback` — decode a clip, present
frame-accurately against the scanout clock, log per-frame presentation times.
Consolidates `SimpleMovieDemo` + `PlayMoviesDemo` + `LoadMovieIntoTexturesDemo`.

---

## 3. 2D stereo (dichoptic presentation)

**PTB demos:** `StereoDemo`, `StereoViewer`, `ImagingStereoDemo`,
`ImagingStereoMoviePlayer`, `SimpleHDRLinuxStereoDemo`.

**What they show:** rendering separate left/right eye images and routing them through
various stereo modes (frame-sequential, dual-display, anaglyph, side-by-side for
mirror-stereoscopes / shutter glasses).

**Why deferred:** VSE presents a single swapchain image. Stereo needs either two
swapchains/displays or a per-eye split of one framebuffer, plus mode-specific routing.
The 3D/VR path (Bevy external frames) already handles the geometric side for HMDs, so
this overlaps partially.

**Infrastructure VSE would need:**
- Left/right render targets and a compositing step that lays them out per stereo mode
  (side-by-side, top/bottom, anaglyph channel mix, frame-sequential with per-eye
  present).
- For frame-sequential shutter glasses: per-eye present timing (the scanout-clock
  machinery already gives us the timestamps; we'd need the eye-tag on each flip).
- Dual-display routing for mirror-stereoscope rigs (the direct-display path is a
  starting point).

**Effort:** medium. Side-by-side / anaglyph are straightforward; frame-sequential and
dual-display are more involved.

**Suggested first demo if revisited:** `stereo_modes` — one stimulus shown in
side-by-side, anaglyph, and frame-sequential modes selectable at runtime.

---

## 4. HDR (high dynamic range)

**PTB demos:** `SimpleHDRDemo`, `HDRViewer`, `HDRDebugViewer`,
`HDRMinimalisticOpenGLDemo`, `SimpleHDRLinuxStereoDemo`.

**What they show:** driving HDR-capable displays with >8-bit, wide-gamut, high-nits
content, loading OpenEXR images, and inspecting HDR pixel values.

**Why deferred:** requires an HDR swapchain format and colorimetry, HDR metadata
signalling, and a display that supports it. Relevant for high-luminance rigs but niche.

**Infrastructure VSE would need:**
- HDR swapchain surface format (e.g. `VK_COLOR_SPACE_HDR10_ST2084_EXT` or scRGB) and
  the corresponding Vulkan HDR metadata extension.
- Float/half-float framebuffer path and EXR image loading.
- Tone-mapping / clamping utilities and a nits-aware color pipeline (ties into the
  gamma/color-calibration subsystem above).

**Effort:** medium, but gated on hardware and driver HDR support (check behaviorally,
like the present-timing features).

---

## 5. Audio

**PTB demos:** `BasicSoundOutputDemo`, `BasicSoundInputDemo`, `BasicSoundScheduleDemo`,
`BasicSoundChannelHoppingDemo`, `BasicSoundPhaseShiftDemo`, `BasicAMAndMixScheduleDemo`,
`DelayedSoundFeedbackDemo`, `SimpleSoundScheduleDemo`, `SimpleVoiceTriggerDemo`,
`AudioTunnel3DDemo`/`2`.

**What they show:** low-latency audio output with scheduling, audio capture, multi-
channel routing, phase-accurate playback, voice-triggered responses, and 3D spatial
audio.

**Why deferred:** VSE is a visual stimulus engine; audio is out of its current scope.
It matters for audiovisual paradigms and auditory cueing but is a separate concern.

**Infrastructure VSE would need:**
- A low-latency audio backend (e.g. `cpal`, or a PortAudio/JACK binding) with
  sample-accurate scheduling.
- A clock bridge between the audio device clock and the scanout clock so audiovisual
  onsets can be co-scheduled (analogous to the existing host-clock bridge).
- Buffered scheduling API mirroring the visual flip scheduling.

**Effort:** medium for basic scheduled output; large for tight AV sync and capture.

---

## 6. Live video capture (cameras)

**PTB demos:** `VideoCaptureDemo`, `VideoCaptureToMatlabDemo`, `VideoRecordingDemo`,
`VideoDVCamCaptureDemo`, `VideoIPWebcamCaptureDemo`, `VideoMultiCameraCaptureDemo`,
`VideoOfflineCaptureDemo`, `VideoPluginCaptureDemo`, `VideoDelayLoopMiniDemo`,
`VideoTextureExtractionDemo`, `BlurredVideoCaptureDemo`, `ImagingVideoCaptureDemo`.

**What they show:** capturing from cameras, recording, multi-camera capture, low-latency
delay loops, and using captured frames as textures (closed-loop / gaze-camera style).

**Why deferred:** mostly an eye-tracking / closed-loop concern; large surface, low value
for open-loop stimulus presentation. Eye tracking in modern rigs is usually handled by a
dedicated tracker with its own API.

**Infrastructure VSE would need:** a capture backend (V4L2 / GStreamer / vendor SDK)
feeding frames into the texture path (again, the external-frame ring is the natural
seam).

**Effort:** large.

---

## 7. External hardware triggers and response boxes

**PTB demos:** `PsychRTBoxDemo`, `RaspberryPiGPIODemo`,
`ReceivingTriggerFromSerialPortDemo`, `DatarecordingFromSerialPortDemo`,
`DatarecordingFromISCANDemo`, `SimpleVoiceTriggerDemo`.

**What they show:** millisecond response boxes, GPIO triggers, serial-port trigger
send/receive, and recording from serial devices (e.g. eye trackers).

**Why deferred:** VSE's clock model makes the canonical acquisition-clock alignment a
**physical photodiode on a stimulus patch feeding the DAQ's ADC** (see
`docs/clock-synchronization.md` and the `photodiode_sync` demo). That already covers the
most important use case — tying stimulus onset to the acquisition clock — without VSE
needing to speak any trigger-box protocol. Serial/GPIO trigger support is a
lab-specific integration rather than core functionality.

**Infrastructure VSE would need:** a serial/GPIO abstraction with timestamps in the
host clock (bridgeable to scanout via the existing host-clock bridge). Straightforward
but per-device.

**Effort:** small-medium per device, but open-ended in breadth.

---

## 8. Miscellaneous PTB demos not carried over

Smaller one-offs that did not fit a category and were not judged worth a dedicated VSE
demo:

- **Image warping / geometry correction** — `ImageWarpingDemo`, `ImageUndistortionDemo`,
  `VignettingCorrectionDemo`, `PanelFitterDemo`. Projector/curved-screen geometry
  correction. Medium value for dome/projector rigs; needs a warp-mesh compositing pass.
- **Mipmap / blur** — `BlurredMipmapDemo`. A shader-effect demo; low standalone value.
- **GPGPU / fractal** — `MandelbrotDemo`, `GPGPUDemos/`. Showpiece shader compute, not
  vision-science content.
- **AR / markers** — `ARToolkitDemo`, `ApriltagsDemo`. Marker tracking; out of scope.
- **Kinect / depth** — `KinectDemo`, `Kinect3DDemo`. Depth-camera hardware; out of scope.
- **Raw OpenGL 3D** — `OpenGL4MatlabDemos/`, `TurnTableDemo`, `SadowskiDemo`,
  `RenderDemo`. VSE's 3D story is the higher-level Bevy external-frame path instead.
- **Multitouch** — `MultiTouchDemo`, `MultiTouchMinimalDemo`, `MultiTouchPinchDemo`.
  Touch input; low value for primate rig work.
- **VR HMD** — `VRHMDDemo`. Partially served by the Bevy 3D path; full HMD support
  (per-eye distortion, HMD present timing) is a large separate effort.
