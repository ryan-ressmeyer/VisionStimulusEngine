//! Main-thread smoke test for buffered timing-only recording.
//!
//! Runs five windowed buffered flips without calling `record_frame()`. Clean
//! shutdown must confirm all five frames and write five timing-only CSV rows.

use anyhow::{ensure, Context, Result};
use std::cell::RefCell;
use std::rc::Rc;
use vision_stimulus_engine::prelude::*;

const FRAME_COUNT: u32 = 5;
const TIMING_COLUMN_COUNT: usize = 10;

fn main() -> Result<()> {
    let output = std::env::temp_dir().join(format!(
        "vse-buffered-recording-smoke-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&output);

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new(&output))
        .build()
        .context("create recording session")?;
    let context = VSEContext::builder()
        .with_window_size(320, 240)
        .with_title("VSE buffered recording smoke test")
        .with_session(session)
        .build()
        .context("create VSE context")?;

    let rendered = Rc::new(RefCell::new(0_u32));
    let presented = Rc::new(RefCell::new(0_u32));
    let rendered_in_callback = rendered.clone();
    let presented_in_callback = presented.clone();

    context
        .run_buffered::<(), _>(BufferedConfig::default(), move |event, vse| {
            match event {
                FlipEvent::Render => {
                    *rendered_in_callback.borrow_mut() += 1;
                    let frame = *rendered_in_callback.borrow();
                    vse.set_clear_color(frame as f32 / FRAME_COUNT as f32, 0.0, 0.0, 1.0);
                    vse.flip_with_payload(None, ())?;
                    if frame == FRAME_COUNT {
                        vse.close();
                    }
                }
                FlipEvent::Presented { .. } => {
                    *presented_in_callback.borrow_mut() += 1;
                    // Deliberately omit record_frame(): VSE must write timing-only data.
                }
                _ => {}
            }
            Ok(())
        })
        .context("run buffered presentation")?;

    let rendered = *rendered.borrow();
    let presented = *presented.borrow();
    ensure!(rendered == FRAME_COUNT, "rendered {rendered} frames");
    ensure!(presented == FRAME_COUNT, "confirmed {presented} frames");

    let mut reader =
        csv::Reader::from_path(output.join("frames.csv")).context("open recorded frames.csv")?;
    let header_columns = reader.headers()?.len();
    ensure!(
        header_columns == TIMING_COLUMN_COUNT,
        "expected timing-only header with {TIMING_COLUMN_COUNT} columns, found {header_columns}"
    );

    let rows: Vec<_> = reader.records().collect::<Result<_, _>>()?;
    ensure!(
        rows.len() == FRAME_COUNT as usize,
        "expected {FRAME_COUNT} timing-only rows, found {}",
        rows.len()
    );
    for (expected_frame, row) in rows.iter().enumerate() {
        ensure!(
            row.len() == TIMING_COLUMN_COUNT,
            "frame {expected_frame} contains user-data columns"
        );
        ensure!(
            row.get(0) == Some(expected_frame.to_string().as_str()),
            "expected frame {expected_frame}, found {:?}",
            row.get(0)
        );
    }

    println!(
        "buffered recording smoke passed: {rendered} rendered, {presented} confirmed, {} timing-only rows",
        rows.len()
    );
    std::fs::remove_dir_all(output).context("remove smoke-test output")?;
    Ok(())
}
