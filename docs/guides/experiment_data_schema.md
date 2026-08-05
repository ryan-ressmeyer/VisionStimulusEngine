# Experiment Data Schema Reference

This document describes the exact output schema for VSE's two data backends.
Timing fields are in microseconds. Their clock domain depends on `timing_source`.

## frames.csv / frames.parquet — Frame Records

One row per synchronous `flip()` or confirmed `BufferedFrame`. Timing columns are always populated.
User columns (from `record_frame()`) are empty/null for frames where
`record_frame` was not called.

| Column | Type | Units | Notes |
|---|---|---|---|
| `frame_number` | u64 | — | Monotonically increasing from 0 |
| `present_time_us` | u64 | µs | Frame present timestamp (see `timing_source` and note below) |
| `submit_time_us` | u64 | µs | Host-clock GPU submission timestamp; not directly comparable with scanout-domain `present_time_us` |
| `timing_source` | string | — | Per-frame timestamp source: `ExtPresentTiming`, `CpuEstimate`, or `Offscreen` |
| `present_id` | u64 | — | `VK_KHR_present_id2` id for hardware feedback correlation; `0` on the CPU path |
| `target_time_us` | u64/null | µs | Requested scanout target for scheduled flips; null for immediate presents |
| `on_target` | bool | — | True when comparable scanout evidence was at or after the target; also true by convention for unscheduled or unconfirmed frames, where it is not evidentiary |
| `missed` | bool | — | Interval-based late-frame diagnostic; does not by itself prove a displayed frame was dropped |
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

When `timing_source = ExtPresentTiming`, `present_time_us` is in the session's scanout-clock domain. VSE derives it from nonzero `IMAGE_FIRST_PIXEL_OUT` feedback or, on the synchronous path, from a calibrated present-stage clock sample after a successful present wait. The latter inherits the implementation-dependent relationship between wait completion and presentation.

When `timing_source = CpuEstimate`, `present_time_us` is a host-clock observation. It confirms completion processing, not display scanout. This value can occur for an individual frame even when the session selected the EXT backend.

When `timing_source = Offscreen`, `present_time_us` is synthesized from frame number and nominal refresh interval. No display presentation occurred.

See [Timing conformance](../timing-conformance.md#timestamp-provenance) before comparing timing columns.

## Null Handling

**CSV:** User columns for timing-only rows are empty strings (`,,`).
**Parquet:** User columns for timing-only rows are Arrow null values.

In Python: `pd.read_csv(..., keep_default_na=True)` treats empty strings
as `NaN` automatically.
