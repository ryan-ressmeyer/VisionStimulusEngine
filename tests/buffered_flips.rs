//! Integration tests for run_buffered(). Require a display — marked #[ignore].

use std::cell::RefCell;
use std::rc::Rc;
use vision_stimulus_engine::prelude::*;

/// Smoke test: run_buffered fires Render events and terminates cleanly.
#[test]
#[ignore = "requires display"]
fn run_buffered_fires_render_events() {
    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    let render_count = Rc::new(RefCell::new(0u32));
    let presented_count = Rc::new(RefCell::new(0u32));
    let rc = render_count.clone();
    let pc = presented_count.clone();

    context
        .run_buffered::<u32, _>(BufferedConfig::default(), move |event, vse| {
            match event {
                FlipEvent::Render => {
                    *rc.borrow_mut() += 1;
                    let n = *rc.borrow();
                    vse.clear()?;
                    vse.flip_with_payload(None, n)?;
                    if n >= 5 {
                        vse.close();
                    }
                }
                FlipEvent::Presented { flip_info, payload } => {
                    *pc.borrow_mut() += 1;
                    assert!(payload >= 1 && payload <= 5);
                    let _ = flip_info;
                }
                _ => {}
            }
            Ok(())
        })
        .expect("run_buffered");

    assert_eq!(*render_count.borrow(), 5);
    // With depth=1, first frame has no Presented; remaining 4 do
    assert_eq!(*presented_count.borrow(), 4);
}

/// Payload arrives in the correct order (FIFO).
#[test]
#[ignore = "requires display"]
fn run_buffered_payload_fifo_order() {
    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    let present_seq = Rc::new(RefCell::new(Vec::<u32>::new()));
    let frame = Rc::new(RefCell::new(0u32));
    let ps = present_seq.clone();
    let fr = frame.clone();

    context
        .run_buffered::<u32, _>(BufferedConfig::default(), move |event, vse| {
            match event {
                FlipEvent::Render => {
                    *fr.borrow_mut() += 1;
                    let n = *fr.borrow();
                    vse.clear()?;
                    vse.flip_with_payload(None, n)?;
                    if n >= 10 {
                        vse.close();
                    }
                }
                FlipEvent::Presented { payload, .. } => {
                    ps.borrow_mut().push(payload);
                }
                _ => {}
            }
            Ok(())
        })
        .expect("run_buffered");

    let seq = present_seq.borrow();
    for i in 1..seq.len() {
        assert!(seq[i] > seq[i - 1], "out of order: {:?}", *seq);
    }
}
