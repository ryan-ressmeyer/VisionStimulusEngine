# vse-3d

`vse-3d` provides controlled scientific 3D rendering for VisionStimulusEngine. It owns a separate vulkano renderer and sends completed color images to base VSE through `vse-external-frame`. VSE adds lightweight 2D overlays and retains exclusive control of timing and presentation.

The initial release supports static glTF meshes, perspective cameras, model transforms, depth testing, and geometric face-normal colors. It targets Linux Vulkan external-memory and external-semaphore file descriptors.

```rust,ignore
context.run_with_setup(
    |vse| {
        let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
        let model = renderer.load_model("model.glb")?;
        Ok((renderer, model))
    },
    |vse, (renderer, model)| {
        renderer.draw_normals(*model, transform, &camera)?;
        renderer.render_frame(vse)?;
        vse.draw_text("overlay", 16.0, 16.0, 2.0, Color::WHITE);
        vse.flip(None)?;
        Ok(())
    },
)?;
```

See [`docs/basic-3d-rendering.md`](../../docs/basic-3d-rendering.md) for coordinates, resource ownership, headless regeneration, metadata, and performance tradeoffs.
