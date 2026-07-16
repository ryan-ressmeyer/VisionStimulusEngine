# Experiment Data Schema Reference

This document describes the exact output schema for VSE's two data backends.
Timing fields are in microseconds. Their clock domain depends on `timing_source`.

## frames.csv / frames.parquet — Frame Records

One row per `flip()` or `flip_with_payload()` call. Timing columns are always populated.
User columns (from `record_frame()`) are empty/null for frames where
`record_frame` was not called.

| Column | Type | Units | Notes |
|---|---|---|---|
| `frame_number` | u64 | — | Monotonically increasing from 0 |
| `present_time_us` | u64 | µs | Frame present timestamp (see `timing_source` and note below) |
| `submit_time_us` | u64 | µs | GPU command buffer submission timestamp |
| `timing_source` | string | — | `ExtPresentTiming` or `CpuEstimate` |
| `present_id` | u64 | — | `VK_KHR_present_id2` id for hardware feedback correlation; `0` on the CPU path |
| `target_time_us` | u64/null | µs | Requested scanout target for scheduled flips; null for immediate presents |
| `on_target` | bool | — | True when the confirmed scanout was at or after `target_time_us`; true for unscheduled or unconfirmed CPU-path frames |
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

When `timing_source = ExtPresentTiming`, `present_time_us` is in the session's scanout-clock
domain. VSE derives it from `IMAGE_FIRST_PIXEL_OUT` feedback when the driver populates that
record, or from a calibrated scanout-clock sample taken immediately after `wait_for_present`
when the driver advertises the feature but returns zero-valued feedback.

When `timing_source = CpuEstimate`, `present_time_us` is a host-clock timestamp taken after the
GPU fence signals. It confirms that rendering completed, but it does not prove display scanout.

## Null Handling

**CSV:** User columns for timing-only rows are empty strings (`,,`).
**Parquet:** User columns for timing-only rows are Arrow null values.

In Python: `pd.read_csv(..., keep_default_na=True)` treats empty strings
as `NaN` automatically.
