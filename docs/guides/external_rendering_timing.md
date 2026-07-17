# External Rendering Timing Policies

External renderers such as Bevy produce pixels for VSE to present. VSE still owns the swapchain, scanout scheduling, and `FlipInfo`. The external renderer only fills imported images.

A display refresh imposes a hard deadline. If a frame is not ready before VSE submits the present, VSE must choose one of three outcomes:

- wait for the producer and risk missing the flip;
- show an older producer frame;
- show no external underlay for that flip.

No policy removes this deadline. The right choice depends on whether the experiment values exact frame identity, low latency, or smooth motion under variable render cost.

---

## Two separate choices

External rendering has two independent timing choices.

### VSE flip loop

`run()` submits one frame and waits for its result before the next frame. It gives the simplest timing model, but CPU and GPU work do not overlap.

`run_buffered()` pipelines presentation. With the default `BufferedConfig { depth: 1, ... }`, frame `N+1` is already submitted when VSE reports confirmed timing for frame `N`. This improves throughput and keeps the GPU busy. Closed-loop changes based on confirmed timing take effect one frame later than in a fully synchronous mental model. See [`buffered_flips.md`](buffered_flips.md).

### External-frame consumption policy

The external-frame policy controls what VSE does when the external producer has, or has not, queued a new image for the next flip.

```rust
use vision_stimulus_engine::prelude::*;

vse.attach_external_frame_source_with_policy(
    producer.export_ring()?,
    release_tx.clone(),
    ExternalFramePolicy::LatestReadyHoldLast,
)?;
```

The existing `attach_external_frame_source(...)` method uses `ExternalFramePolicy::FrameLocked`.

---

## `ExternalFramePolicy::FrameLocked`

This is the default policy.

For each VSE frame, the producer renders a matching external frame and queues its ring slot before `flip()` or `flip_with_payload()`:

```rust
let n = vse.frame_number();
let slot = producer.render_frame(n)?;
vse.queue_external_frame(slot)?;
vse.flip_with_payload(None, n)?;
```

VSE consumes the queued external frame for that flip. If several frames accumulated, VSE displays the newest but still waits the older binary semaphores so those slots can be safely reused.

Use this policy when frame identity matters:

- deterministic movie playback;
- validation runs where producer frame `n` should match VSE frame `n`;
- stimuli whose state is defined as an exact function of VSE's frame counter;
- experiments where dropping or repeating external frames would change the stimulus class.

Tradeoff: a synchronous producer can hold up VSE. If Bevy rendering takes too long, the present may miss the intended refresh. VSE will still report this in `FlipInfo`.

---

## `ExternalFramePolicy::LatestReadyHoldLast`

This policy lowers latency by treating the external stream as latest-ready content.

At each flip, VSE does the following:

1. Polls completed VSE submits and returns releasable ring slots to the producer.
2. Consumes all currently queued external frames.
3. Displays the newest queued frame, if one exists.
4. If no new external frame is queued, displays the last external slot again.
5. Keeps the displayed slot pinned on the VSE side until a replacement submit succeeds.

The pinned slot is not released to the producer while VSE may repeat it. This avoids an extra blit into a VSE-owned latch image. It also means the producer ring needs enough slots for one held display image, current producer work, and in-flight VSE submits.

Use this policy when latency matters more than exact producer-frame identity:

- simple Bevy animations where repeating the previous frame is better than blanking;
- closed-loop visual feedback where the newest available pose or scene state should be shown;
- workloads with occasional producer hitches where a one-frame hold is acceptable.

Tradeoff: VSE may show the same external frame for multiple refreshes. Motion is smoother than dropping the underlay, but the external stream no longer has a one-to-one mapping between producer frame id and VSE frame id. Record the producer frame id in the VSE payload if the analysis needs to know which scene state was shown.

---

## Ring sizing for pinned-slot mode

`LatestReadyHoldLast` keeps one imported image owned by VSE as the current latch. A too-small ring can starve the producer because the producer cannot overwrite the pinned slot.

Practical guidance:

| VSE mode | External policy | Recommended ring length |
|---------|-----------------|-------------------------|
| `run()` | `FrameLocked` | 2 or 3 |
| `run_buffered(depth = 1)` | `FrameLocked` | 3 |
| `run()` | `LatestReadyHoldLast` | 3 |
| `run_buffered(depth = 1)` | `LatestReadyHoldLast` | 4 or more |

Use more slots if the producer runs on a separate thread or can queue several frames ahead. Extra slots reduce producer starvation at the cost of more memory and a larger backlog of stale frames that VSE may skip.

---

## Smoothness versus latency

A frame-locked producer gives the cleanest stimulus record. Frame `n` means one thing across Bevy, VSE, and the data file. The cost is that VSE may wait for the producer.

A latest-ready producer reduces waiting. VSE shows the newest completed image and repeats the previous image when the producer misses the deadline. The cost is temporal quantization in the external animation. Some VSE frames carry new external pixels; others repeat old pixels.

For analysis, distinguish these events:

- **new external frame**: VSE displayed a producer frame that had not been displayed before;
- **repeat**: VSE displayed the pinned producer slot again;
- **stale drop**: multiple producer frames were ready and VSE displayed only the newest.

The current API exposes the policy and preserves safe slot ownership. If an experiment needs per-frame external-stream annotations, include the producer frame id and repeat/drop status in the payload passed to `flip_with_payload()`.

---

## Deadline-aware producers

`LatestReadyHoldLast` is most useful when the external producer is nonblocking from VSE's point of view. The current Bevy producer API can be used synchronously, where `render_frame()` returns only after Bevy submits the frame. In that pattern, VSE still waits for Bevy before it can flip.

A deadline-aware producer should instead render ahead or render on another thread, then let VSE poll ready frames:

```rust
while let Some(frame) = producer.try_recv_ready() {
    vse.queue_external_frame(frame.slot)?;
}

vse.flip_with_payload(None, payload)?;
```

With `LatestReadyHoldLast`, a missed producer deadline then repeats the pinned frame rather than blocking the flip. VSE's scanout timing remains deterministic; the external content stream becomes opportunistic.

---

## Timeline semaphores

The current implemented path uses one binary semaphore per ring slot. A binary signal must be waited before that slot's semaphore can be signaled again, so VSE drains all ready slots even when it displays only the newest.

A future timeline-semaphore backend can make latest-ready consumption cleaner. The producer would signal monotonically increasing values on one semaphore, and VSE could wait directly for the newest value. Older producer frames would be superseded by that wait. Slot ownership would still matter: VSE must keep any pinned display slot away from the producer until a replacement submit has finished reading it.
