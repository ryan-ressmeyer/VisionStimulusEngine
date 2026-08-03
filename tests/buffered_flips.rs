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

/// `run_buffered_with_setup` runs its setup before the first Render event, and
/// hands the setup's value to every frame.
///
/// Pipeline compilation and asset loading belong off the presentation path in
/// buffered mode exactly as they do in `run` — before this hook existed,
/// `run_buffered` was the one loop with nowhere to put them.
///
/// Cannot run under the test harness at all, not merely without a display:
/// winit panics when an `EventLoop` is created off the main thread, and the
/// harness runs every test on a worker thread. What it buys is a compile-time
/// check of the signature. The ordering it asserts was verified by running the
/// equivalent from a `main`, where `setup` printed before frame 0.
#[test]
#[ignore = "winit requires the main thread; cannot run under the test harness"]
fn run_buffered_with_setup_runs_setup_before_the_first_frame() {
    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    // Records the order of events: setup must appear exactly once, first.
    let order = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let setup_order = order.clone();
    let render_order = order.clone();
    let frames = Rc::new(RefCell::new(0u32));

    context
        .run_buffered_with_setup::<u32, _, _, _>(
            BufferedConfig::default(),
            move |_vse| {
                setup_order.borrow_mut().push("setup");
                Ok(7u32)
            },
            move |event, vse, from_setup| {
                if let FlipEvent::Render = event {
                    assert_eq!(*from_setup, 7, "setup's value must reach the render arm");
                    render_order.borrow_mut().push("render");
                    *frames.borrow_mut() += 1;
                    let n = *frames.borrow();
                    vse.clear()?;
                    vse.flip_with_payload(None, n)?;
                    if n >= 3 {
                        vse.close();
                    }
                }
                Ok(())
            },
        )
        .expect("run_buffered_with_setup");

    let order = order.borrow();
    assert_eq!(
        order.first(),
        Some(&"setup"),
        "setup must run before the first frame, got {order:?}"
    );
    assert_eq!(
        order.iter().filter(|e| **e == "setup").count(),
        1,
        "setup must run exactly once, got {order:?}"
    );
}
