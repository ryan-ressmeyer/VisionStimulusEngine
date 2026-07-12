//! Pixel-determinism harness for the Bevy → VSE external-frame ring.
//!
//! Renders the demo scene for 200 frames and hashes the **imported** external
//! image (readback recorded in the same VSE command buffer that consumes the
//! frame — so the hash covers the producer's output *and* the export/import +
//! semaphore handoff) at frame indices 120..123, one per ring slot.
//!
//! Run twice and compare the printed hashes; determinism holds iff they are
//! identical across runs:
//!
//! ```sh
//! for i in 1 2; do
//!   CARGO_INCREMENTAL=0 cargo run -p vse-bevy --release --example 02_verify_determinism \
//!     | grep '^hash'
//! done
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use vision_stimulus_engine::prelude::*;
use vse_bevy::{scene::build_demo_scene, BevyProducer, ProducerConfig};
use vse_external_frame::release_channel;

const FRAMES: u64 = 200;
const HASH_FRAMES: [u64; 3] = [120, 121, 122]; // covers all 3 ring slots

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let config = ProducerConfig::default();
    let extent = config.extent;
    let byte_len = (extent[0] * extent[1] * 4) as u64; // RGBA8

    let mut producer = BevyProducer::new(config, build_demo_scene)?;
    let (release_tx, release_rx) = release_channel();
    producer.set_release_rx(release_rx);

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("determinism harness")
        .build()?;

    // frame_number -> readback buffer, armed on the matching Render event.
    let pending: Rc<RefCell<BTreeMap<u64, vulkano::buffer::Subbuffer<[u8]>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let hashes: Rc<RefCell<BTreeMap<u64, u64>>> = Rc::new(RefCell::new(BTreeMap::new()));
    let captured: Rc<RefCell<Option<Vec<u8>>>> = Rc::new(RefCell::new(None));
    let (pend, hsh, cap) = (pending.clone(), hashes.clone(), captured.clone());
    let mut attached = false;

    context.run_buffered::<u64, _>(BufferedConfig::default(), move |event, vse| {
        match event {
            FlipEvent::Render => {
                if !attached {
                    vse.attach_external_frame_source(
                        producer
                            .export_ring()
                            .map_err(|e| VSEError::EventLoop(format!("producer: {e}")))?,
                        release_tx.clone(),
                    )?;
                    attached = true;
                }
                let n = vse.frame_number();
                let slot = producer
                    .render_frame(n)
                    .map_err(|e| VSEError::EventLoop(format!("producer: {e}")))?;
                vse.queue_external_frame(slot)?;
                if HASH_FRAMES.contains(&n) {
                    let buffer = vulkano::buffer::Buffer::new_slice::<u8>(
                        vse.memory_allocator(),
                        vulkano::buffer::BufferCreateInfo {
                            usage: vulkano::buffer::BufferUsage::TRANSFER_DST,
                            ..Default::default()
                        },
                        vulkano::memory::allocator::AllocationCreateInfo {
                            memory_type_filter:
                                vulkano::memory::allocator::MemoryTypeFilter::PREFER_HOST
                                    | vulkano::memory::allocator::MemoryTypeFilter::HOST_RANDOM_ACCESS,
                            ..Default::default()
                        },
                        byte_len,
                    )
                    .map_err(|e| VSEError::EventLoop(format!("readback alloc: {e}")))?;
                    vse.arm_external_readback(buffer.clone());
                    pend.borrow_mut().insert(n, buffer);
                }
                vse.flip_with_payload(None, n)?;
                if n + 1 >= FRAMES {
                    vse.close();
                }
            }
            FlipEvent::Presented { flip_info, payload } => {
                // Fence confirmed => the readback copy for this frame is complete.
                let _ = flip_info;
                if let Some(buffer) = pend.borrow_mut().remove(&payload) {
                    let content = buffer
                        .read()
                        .map_err(|e| VSEError::EventLoop(format!("readback read: {e}")))?;
                    let mut hasher = DefaultHasher::new();
                    content.hash(&mut hasher);
                    hsh.borrow_mut().insert(payload, hasher.finish());
                    if cap.borrow().is_none() {
                        *cap.borrow_mut() = Some(content.to_vec());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    })?;

    let hashes = hashes.borrow();
    for (frame, hash) in hashes.iter() {
        println!("hash frame {frame}: {hash:#018x}");
    }
    // Distinct animation frames must hash differently — identical hashes here
    // would mean the ring is carrying a stale/blank image, not the scene.
    let distinct: std::collections::BTreeSet<_> = hashes.values().collect();
    if hashes.len() > 1 && distinct.len() == 1 {
        println!("FAIL x  all sampled frames hash identically — no animated content in the ring");
        return Ok(());
    }
    // Visual evidence: dump the first captured frame as PPM (view with any image viewer).
    if let Some(buffer) = captured.borrow().as_ref() {
        let path = "bevy_ring_frame.ppm";
        write_ppm(path, extent, buffer)?;
        println!("frame image written: {path}");
    }
    if hashes.len() == HASH_FRAMES.len() {
        println!("OK  captured {} frame hashes — compare across two runs", hashes.len());
    } else {
        println!(
            "FAIL x  captured {} of {} frame hashes (frame skipped or readback lost)",
            hashes.len(),
            HASH_FRAMES.len()
        );
    }
    Ok(())
}

/// Minimal PPM (P6) writer: RGBA8 bytes → RGB, no dependencies.
fn write_ppm(path: &str, extent: [u32; 2], rgba: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(f, "P6\n{} {}\n255\n", extent[0], extent[1])?;
    for px in rgba.chunks_exact(4) {
        f.write_all(&px[..3])?;
    }
    Ok(())
}
