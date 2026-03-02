# Timing & Data Recording — Design Document

**Date:** 2026-03-01
**Status:** Approved
**Scope:** Decouple timing infrastructure from data persistence; introduce a
flexible, backend-agnostic experiment data recording system.

---

## Problem Statement

The current `FlipLogger` conflates two responsibilities:
1. Timing infrastructure — capturing per-frame GPU/CPU timestamps
2. Data persistence — writing those timestamps to a CSV file

This conflation makes it impossible to save user-defined experimental data
(stimulus parameters, behavioral responses, trial metadata) alongside timing
data without invasive changes, and locks users into a single output format.

---

## Goals

- Per-frame experimental data (user-defined struct) merged with timing data in
  output, using a typed, schema-declared approach (`#[derive(serde::Serialize)]`).
- Flexible event recording for less-structured data: typed annotation structs
  and raw key-value events as a catch-all.
- Pluggable `DataWriter` trait — ship CSV and Parquet backends, allow custom
  backends (e.g. network streaming, binary formats).
- Non-blocking disk I/O: serialize on the main thread, flush to disk on a
  dedicated writer thread via a bounded channel.
- Configurable backpressure: block (default) or drop-with-warning on channel
  overflow.
- No C library dependencies — both shipped backends are pure Rust.
- Thorough documentation: rustdoc on all public types, a prose data recording
  guide, and a schema reference document.

---

## Non-Goals

- HDF5 support (data can be converted from Parquet as needed).
- Buffered/pipelined flip support (separate design session; architecture is
  forward-compatible — see Section 6).
- Backward-compatible preservation of the `FlipLogger` public API beyond one
  minor version deprecation window.

---

## Core Abstractions

### `DataWriter` trait

Implemented by each backend. Runs exclusively on the writer thread. Receives
pre-serialized messages from the main thread.

```rust
pub trait DataWriter: Send + 'static {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError>;
    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError>;
    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError>;
    /// Called on clean shutdown (session drop). Must flush all buffered data.
    fn flush(&mut self) -> Result<(), DataError>;
}
```

The trait is documented with a complete worked example of a custom backend
(e.g. a UDP streaming writer) so users can extend the system without reading
internal source code.

### Message types

Serialized on the main thread (microseconds cost), sent over the channel,
deserialized and written by the writer thread.

```rust
pub struct FrameMessage {
    pub flip: FlipInfo,
    pub payload: Option<Vec<u8>>,    // serde-serialized user struct; None for timing-only rows
    pub schema_name: &'static str,   // std::any::type_name of the user struct
}

pub struct AnnotationMessage {
    pub stream: String,              // user-specified stream/table name
    pub timestamp: Timestamp,
    pub payload: Vec<u8>,            // serde-serialized annotation struct
}

pub struct EventMessage {
    pub name: String,
    pub timestamp: Timestamp,
    pub value: String,               // raw string value
}
```

### `ExperimentSession`

Owns the writer thread and the channel sender. Constructed by the user and
passed to `VSEContext::builder()`. Exposes no public write methods — all writes
go through `RenderContext` inside the run loop.

```rust
pub struct ExperimentSession { /* channel sender, thread handle */ }

pub struct ExperimentSessionBuilder {
    writer: Box<dyn DataWriter>,
    channel_capacity: usize,       // default: 4096
    overflow: OverflowBehavior,    // default: Block
}

pub enum OverflowBehavior {
    /// Block the render loop until channel space is available. Default.
    /// No data loss. Risk: frame drop if writer falls persistently behind.
    Block,
    /// Drop the record and emit tracing::warn!(). Never stalls the render loop.
    /// Risk: data loss under sustained writer slowness.
    DropWithWarning,
}
```

---

## `RenderContext` API

Three methods added, called inside the `run()` callback:

```rust
impl RenderContext {
    /// Record per-frame data merged with the most recent FlipInfo.
    /// Must be called after flip(). Returns VSEError::NoFlipPending if not.
    /// Safe to call multiple times per frame — each call writes one row.
    pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError>;

    /// Record a typed annotation at the current timestamp.
    /// `stream` becomes the table/dataset name in the output file.
    pub fn record_annotation<A: serde::Serialize>(
        &mut self,
        stream: &str,
        data: A,
    ) -> Result<(), VSEError>;

    /// Record a raw string key-value event at the current timestamp.
    /// Escape hatch for unstructured or one-off data.
    pub fn record_event(&mut self, name: &str, value: &str) -> Result<(), VSEError>;
}
```

### Typical experiment loop

```rust
#[derive(serde::Serialize)]
struct MyFrameData {
    stimulus_id: u32,
    contrast: f32,
    orientation_deg: f32,
}

#[derive(serde::Serialize)]
struct TrialMeta {
    trial_id: u32,
    condition: String,
}

context.run(move |vse| {
    vse.clear()?;
    let flip = vse.flip(None)?;

    vse.record_frame(MyFrameData {
        stimulus_id: current_stim,
        contrast: 0.8,
        orientation_deg: 45.0,
    })?;

    if trial_just_started {
        vse.record_annotation("trial", &TrialMeta {
            trial_id,
            condition: "high_contrast".into(),
        })?;
    }

    if key_pressed {
        vse.record_event("response", "left")?;
    }

    Ok(())
})?;
```

User structs only require `#[derive(serde::Serialize)]` — no VSE-specific
derives or trait implementations needed.

---

## Timing Attachment & Missing Frame Data

### How timing attaches

`flip()` is synchronous and blocking — it waits for the GPU fence to signal
before returning. When `flip()` returns, `FlipInfo` is fully populated with the
best available timing. Every `flip()` call automatically enqueues a
`FrameMessage` with `payload: None` (timing-only row). When `record_frame(data)`
is called after `flip()`, it reads the cached `FlipInfo` and enqueues a
`FrameMessage` with the serialized user data merged in.

```
flip()          → caches FlipInfo, enqueues timing-only FrameMessage
record_frame()  → reads cached FlipInfo, enqueues merged FrameMessage
                  (replaces the timing-only row for this frame)
```

### Missing frame data

Not every frame needs a `record_frame` call. Every flip always produces a
timing row. User data is optional.

| Frame | `record_frame` called? | Output row |
|-------|------------------------|------------|
| N     | Yes                    | All timing fields + all user fields |
| N+1   | No                     | All timing fields + user fields null/empty |
| N+2   | Yes (twice)            | Two rows, same frame_number, both with timing |

Backends handle null user fields gracefully: CSV writes empty columns, Parquet
uses null values. The schema is determined by the first `record_frame` call;
subsequent calls must use the same struct type.

### `timing_source` field

Every row carries a `timing_source` field (`CpuEstimate` or
`GoogleDisplayTiming`) so users always know the provenance and precision of
each frame's `present_time_us`.

---

## Threading Model & Backpressure

```
Main thread (render loop)              Writer thread
──────────────────────────             ─────────────────────────
flip()  ─── FrameMessage(None) ──────→ DataWriter::write_frame()
record_frame() ─ FrameMessage(data) →  DataWriter::write_frame()
record_annotation() ─────────────────→ DataWriter::write_annotation()
record_event() ──────────────────────→ DataWriter::write_event()
session drop ──── Shutdown ──────────→ DataWriter::flush() + thread join
```

The channel capacity default of 4096 messages provides ~68 seconds of buffering
at 60 Hz before backpressure occurs. In practice the writer thread drains
continuously; stalls only occur during filesystem hiccups or large Parquet row
group flushes.

On unclean shutdown (panic), the channel is dropped and the writer thread
receives a disconnected error, triggering a best-effort flush. Data written
before the panic is preserved; in-flight channel messages may be lost.

### Forward compatibility: pending-confirmation queue

`ExperimentSession` will include a small pending-record queue to support future
buffered/pipelined flip modes. When a non-blocking `flip()` variant is added,
frame records queued with estimated timing will be held here until confirmed GPU
timestamps arrive via `vkGetPastPresentationTimingGOOGLE`, then promoted to the
writer channel. This is a no-op in the current synchronous flip path. Full
design of buffered flip is deferred to a separate design session.

---

## `DataWriter` Backends

### `CsvDataWriter`

No extra dependencies. Produces two files per session:

```
experiment_data/
  frames.csv    — frame_number, present_time_us, submit_time_us, missed,
                  missed_count, timing_source, skipped, [user frame fields...]
  events.csv    — timestamp_us, stream, payload (JSON string)
```

Annotations and raw events share `events.csv`, distinguished by the `stream`
column. Frame schema is inferred from the first `record_frame` call.

### `ParquetDataWriter`

Pure Rust (`parquet` crate, no C library). Produces one file per session:

```
experiment.parquet   — one row group per session, columnar
                       all frame fields including timing + user fields
                       annotations and events stored as nested columns
                       or in a companion events.parquet
```

Parquet's columnar compression is well-suited to frame data (many rows of
similar values). Files are directly readable by pandas, polars, and R's
`arrow` package.

### Custom backends

Users implement `DataWriter` directly. The trait documentation includes a
complete worked example. Possible uses: UDP streaming to a real-time analysis
system, a compact binary format for maximum throughput, writing directly into
an existing lab data pipeline.

---

## `ExperimentSession` Builder

```rust
// CSV
let session = ExperimentSession::builder()
    .with_writer(CsvDataWriter::new("data/my_experiment/"))
    .with_channel_capacity(4096)
    .with_overflow(OverflowBehavior::Block)
    .build()?;

// Parquet
let session = ExperimentSession::builder()
    .with_writer(ParquetDataWriter::new("data/my_experiment.parquet"))
    .with_overflow(OverflowBehavior::DropWithWarning)
    .build()?;

let context = VSEContext::builder()
    .with_window_size(1920, 1080)
    .with_session(session)
    .build()?;
```

If no session is attached, `VSEContext` behaves exactly as today with zero
data recording overhead.

---

## FlipLogger Migration

`FlipLogger` is refactored to `pub(crate)`. Its responsibility shrinks to
driving automatic per-flip `FrameMessage` dispatch into the `ExperimentSession`
channel. The following builder methods are **deprecated** and removed in the
next minor version:

- `VSEContextBuilder::with_flip_logging(bool)`
- `VSEContextBuilder::with_flip_log_csv(path)`

**Migration path:** replace `with_flip_log_csv("timing.csv")` with:
```rust
.with_session(
    ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("timing/"))
        .build()?
)
```

`TimingStats`, `FlipInfo`, `Timestamp`, and `Clock` remain fully public — they
are useful independent of data recording.

---

## Documentation Deliverables

Documentation is a first-class deliverable, shipped alongside the
implementation.

### rustdoc
Every public type, method, and trait gets a doc comment with: purpose, usage
notes, and a working code example. Special attention to:
- `DataWriter` trait: full worked example of a custom backend implementation
- `ExperimentSession`: threading model, backpressure behavior, shutdown
  semantics, panic behavior
- `record_frame` / `record_annotation` / `record_event`: when to use each,
  schema constraints, error conditions

### `docs/guides/data_recording.md`
Prose walkthrough covering:
- Full data flow from `record_frame` call to file on disk (with diagram)
- How to define frame data and annotation structs
- Choosing between CSV and Parquet
- Reading data back in Python — pandas and polars code snippets for both formats
- Implementing a custom `DataWriter` backend
- Timing precision notes: what `present_time_us` means, which `timing_source`
  values are most accurate, and the pending-confirmation queue for future
  buffered flip support

### `docs/guides/experiment_data_schema.md`
Reference documentation for the exact output schema of both backends: column
names, types, units, null handling. Users should be able to write analysis
code from this document alone without reading library source.
