# Experiment Data Recording

VSE provides a flexible data recording system via `ExperimentSession` and the
`DataWriter` trait. All disk I/O happens on a dedicated writer thread so your
render loop is never stalled by filesystem operations.

## Data Flow

```
RenderContext::flip()
    │
    ▼
RecordingState::on_flip()
    │  Sends timing-only FrameMessage if previous flip had no record_frame
    ▼
ExperimentSession (SyncSender<WriterMessage>)
    │  Bounded channel (default: 4096 messages)
    ▼
Writer thread
    │
    ▼
DataWriter::write_frame() / write_annotation() / write_event()
    │
    ▼
Disk (frames.csv + events.csv  or  frames.parquet)
```

## Quick Start

Define your per-frame data struct (must derive `serde::Serialize`):

```rust
#[derive(serde::Serialize)]
struct FrameData {
    stimulus_id: u32,
    contrast: f32,
    orientation_deg: f32,
}
```

Attach a session to your context:

```rust
use vision_stimulus_engine::prelude::*;

let session = ExperimentSession::builder()
    .with_writer(CsvDataWriter::new("data/session_001/"))
    .build()?;

let context = VSEContext::builder()
    .with_window_size(1920, 1080)
    .with_session(session)
    .build()?;
```

Record data in the run loop:

```rust
context.run(|vse| {
    vse.clear()?;
    let _flip = vse.flip(None)?;

    vse.record_frame(FrameData { stimulus_id: 42, contrast: 0.8, orientation_deg: 45.0 })?;

    if trial_just_started {
        vse.record_annotation("trial", &TrialMeta { trial_id, condition: "A".into() })?;
    }
    if key_pressed {
        vse.record_event("response", "left")?;
    }
    Ok(())
})?;
```

## Reading Data in Python

**CSV:**
```python
import pandas as pd
frames = pd.read_csv("data/session_001/frames.csv")
events = pd.read_csv("data/session_001/events.csv")
```

**Parquet:**
```python
import pandas as pd
frames = pd.read_parquet("data/session_001.parquet")
```

## Choosing a Backend

| Feature | CsvDataWriter | ParquetDataWriter |
|---|---|---|
| Dependencies | None (pure Rust) | None (pure Rust) |
| Human-readable | Yes | No |
| Compression | No | Yes (via parquet encoding) |
| Append-friendly | Yes | No (written at flush) |
| Python/R interop | pandas, R csv | pandas, polars, R arrow |
| Missing data | Empty columns | Null values |

Use `CsvDataWriter` for quick experiments and debugging. Use
`ParquetDataWriter` for large datasets or when columnar compression matters.

## Missing Frame Data

Not every frame needs a `record_frame()` call. VSE automatically records a
timing-only row (all timing fields populated, user fields null/empty) for every
flip where `record_frame` was not called. This ensures the complete timing
history is always present in your output file.

## Backpressure

The channel capacity (default: 4096) controls how many messages can queue up
before the render loop is affected. At 60 Hz, 4096 provides ~68 seconds of
buffering.

```rust
// Block render loop if writer falls behind (default, no data loss)
ExperimentSession::builder()
    .with_overflow(OverflowBehavior::Block)

// Drop records if full (no frame drops, possible data loss)
ExperimentSession::builder()
    .with_overflow(OverflowBehavior::DropWithWarning)
```

## Custom Backends

Implement `DataWriter` to write to any destination:

```rust
use vision_stimulus_engine::data::{DataWriter, DataError};
use vision_stimulus_engine::data::messages::{FrameMessage, AnnotationMessage, EventMessage};

struct MyWriter;

impl DataWriter for MyWriter {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
        println!("frame {}: present={}us", msg.flip.frame_number,
                 msg.flip.present_time.as_micros());
        Ok(())
    }
    fn write_annotation(&mut self, _msg: AnnotationMessage) -> Result<(), DataError> { Ok(()) }
    fn write_event(&mut self, _msg: EventMessage) -> Result<(), DataError> { Ok(()) }
    fn flush(&mut self) -> Result<(), DataError> { Ok(()) }
}
```

## Timing Notes

Every row in `frames.csv` / `frames.parquet` includes a `timing_source` column:

- `CpuEstimate`: timestamp taken after GPU fence signal — accurate to ~0.5ms
- `GoogleDisplayTiming`: hardware scanout timestamp from the driver — accurate
  to the display refresh interval

The `present_time_us` field is microseconds since the VSE clock epoch
(context creation time), not wall-clock time.

## Future: Buffered Flip

A future version will add non-blocking `flip()` variants for pipelined GPU
submission. The `ExperimentSession` architecture is designed for this: a
pending-confirmation queue will hold frame records with estimated timing until
the driver confirms the actual scanout time via `vkGetPastPresentationTimingGOOGLE`.
This is transparent to user code — the `record_frame()` API is unchanged.
