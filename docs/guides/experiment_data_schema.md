# Experiment Data Schema Reference

This document describes the exact output schema for VSE's two data backends.
All timestamps are in **microseconds since VSE context creation** (not wall clock).

## frames.csv / frames.parquet — Frame Records

One row per `flip()` or `flip_with_payload()` call. Timing columns are always populated.
User columns (from `record_frame()`) are empty/null for frames where
`record_frame` was not called.

| Column | Type | Units | Notes |
|---|---|---|---|
| `frame_number` | u64 | — | Monotonically increasing from 0 |
| `present_time_us` | u64 | µs | Frame present timestamp (see timing_source and note below) |
| `submit_time_us` | u64 | µs | GPU command buffer submission timestamp |
| `timing_source` | string | — | `CpuEstimate` or `GoogleDisplayTiming` |
| `missed` | bool | — | True if this frame was dropped |
| `missed_count` | u32 | — | Number of display intervals missed (0 = on time) |
| `skipped` | bool | — | True if frame was skipped (minimized/swapchain recreation) |
| *(user columns)* | varies | user-defined | Populated from first `record_frame()` payload |

## events.csv — Annotations and Events

Annotations (from `record_annotation()`) and raw events (from `record_event()`)
share this file, distinguished by the `stream` column.

| Column | Type | Units | Notes |
|---|---|---|---|
| `timestamp_us` | u64 | µs | Clock timestamp when recorded |
| `stream` | string | — | Stream name. For raw events: the `name` arg |
| `payload` | string | — | JSON string for annotations; raw value for events |

### Note on `present_time_us` accuracy

In synchronous `run()`, `present_time_us` is captured after the blocking GPU fence wait —
it reflects when the GPU finished executing the command buffer, not necessarily the exact
display scanout.

In `run_buffered()`, `present_time_us` is populated in `FlipEvent::Presented` after the
fence signals. When `VK_GOOGLE_display_timing` is available (`timing_source =
GoogleDisplayTiming`), the driver provides an actual hardware scanout timestamp via
`vkGetPastPresentationTimingGOOGLE`. When the CPU path is used (`CpuEstimate`),
`present_time_us` is the fence-signal time. Either way, it is never a pre-submit estimate.

## Null Handling

**CSV:** User columns for timing-only rows are empty strings (`,,`).
**Parquet:** User columns for timing-only rows are Arrow null values.

In Python: `pd.read_csv(..., keep_default_na=True)` treats empty strings
as `NaN` automatically.
