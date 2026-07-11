//! Headless Bevy producer for VSE's external-renderer handoff seam.
//!
//! Renders on Bevy's own stock wgpu device into a ring of exported images
//! (OPAQUE_FD external memory); VSE imports the ring and stays sole present
//! authority. Populated at the spike step.
