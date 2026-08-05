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
        .run_buffered(
            BufferedConfig::default(),
            move |vse| {
                *rc.borrow_mut() += 1;
                let n = *rc.borrow();
                vse.clear()?;
                if n >= 5 {
                    vse.request_exit();
                }
                Ok(BufferedFrame::new(n))
            },
            move |confirmed, _vse| {
                *pc.borrow_mut() += 1;
                assert!(confirmed.payload >= 1 && confirmed.payload <= 5);
                let _ = confirmed.flip_info;
                Ok(())
            },
        )
        .expect("run_buffered");

    assert_eq!(*render_count.borrow(), 5);
    // Clean shutdown drains every submitted frame, including the final one.
    assert_eq!(*presented_count.borrow(), 5);
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
        .run_buffered(
            BufferedConfig::default(),
            move |vse| {
                *fr.borrow_mut() += 1;
                let n = *fr.borrow();
                vse.clear()?;
                if n >= 10 {
                    vse.request_exit();
                }
                Ok(BufferedFrame::new(n))
            },
            move |confirmed, _vse| {
                ps.borrow_mut().push(confirmed.payload);
                Ok(())
            },
        )
        .expect("run_buffered");

    let seq = present_seq.borrow();
    for i in 1..seq.len() {
        assert!(seq[i] > seq[i - 1], "out of order: {:?}", *seq);
    }
}

/// `run_buffered_with_state` initializes its state before the first rendered
/// frame and hands that state to both buffered phases.
///
/// Pipeline compilation, asset loading, and experiment-state construction
/// belong off the presentation path.
///
/// Cannot run under the test harness at all, not merely without a display:
/// winit panics when an `EventLoop` is created off the main thread, and the
/// harness runs every test on a worker thread. What it buys is a compile-time
/// check of the signature. The ordering it asserts was verified by running the
/// equivalent from a `main`, where `setup` printed before frame 0.
#[test]
#[ignore = "winit requires the main thread; cannot run under the test harness"]
fn run_buffered_with_state_initializes_before_the_first_frame() {
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
        .run_buffered_with_state(
            BufferedConfig::default(),
            move |_vse| {
                setup_order.borrow_mut().push("setup");
                Ok(7u32)
            },
            move |from_setup, vse| {
                assert_eq!(*from_setup, 7, "setup's value must reach the render phase");
                render_order.borrow_mut().push("render");
                *frames.borrow_mut() += 1;
                let n = *frames.borrow();
                vse.clear()?;
                if n >= 3 {
                    vse.request_exit();
                }
                Ok(BufferedFrame::new(n))
            },
            |_from_setup, _confirmed, _vse| Ok(()),
        )
        .expect("run_buffered_with_state");

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

/// Buffered payloads do not need `Serialize` or `Send` unless user code sends
/// or records them. This ignored test exists as a compile-time contract.
#[test]
#[ignore = "winit requires the main thread; compile-time contract only"]
fn run_buffered_accepts_local_unserialized_payload() {
    struct LocalPayload(Rc<()>);

    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    context
        .run_buffered(
            BufferedConfig::default(),
            |_vse| Ok(BufferedFrame::new(LocalPayload(Rc::new(())))),
            |confirmed, vse| {
                assert_eq!(Rc::strong_count(&confirmed.payload.0), 1);
                vse.request_exit();
                Ok(())
            },
        )
        .expect("run_buffered");
}
