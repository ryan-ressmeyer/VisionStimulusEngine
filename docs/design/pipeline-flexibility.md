# Design: Flexible & User-Defined Rendering Pipelines

**Status:** Historical design record. Tiers 1 and 2 shipped with the call-order change in §4-5. Audit 7 removed Tier 0 suite subselection and made the seven base 2D pipelines unconditional. Audit 7b moved controlled 3D into the separate `vse-3d` external-frame producer. Sections 1 and 3-8 retain the original problem statement and design sequence. `docs/guides/pipelines.md` describes current behavior.

**Goal at the time:** Let users subselect built-ins, register their own pipelines, and record raw Vulkan draws inside VSE's frame while preserving timing determinism and teaching the pipeline model.

---

## 1. Original architecture

VSE's renderer is a **closed, batched, type-ordered immediate-mode renderer**.

### Pipelines are hardcoded fields

`Renderer` (`src/drawing/renderer.rs`) owns eight `Arc<GraphicsPipeline>` fields,
all built once in `Renderer::new()`:

`flat_color`, `textured`, `grating`, `gabor`, `additive_gabor`,
`subtractive_gabor`, `dot`, `mesh_normals`.

There is no collection, no registry, no way to add or remove one.

### Shaders are compiled into the binary at build time

Each pipeline's GLSL is pulled in by `vulkano_shaders::shader!{ path: "…" }` in a
private module inside `renderer.rs`. The macro also **generates the matching
`PushConstants` struct**, so each pipeline's Rust push-constant type is welded to
its GLSL at compile time. Adding a pipeline today means editing VSE's source and
recompiling VSE.

### `DrawCommand` is a closed `pub(crate)` enum

`src/drawing/primitives.rs` defines one variant per primitive family
(`Rect`, `Circle`, `Line`, `Arc`, `Texture`, `Grating`, `Gabor`, `Noise`,
`Dots`). `RenderContext::draw_*` pushes a variant; `render()` drains the queue.
Users cannot extend the enum.

### `render()` runs fixed passes in a fixed order

`render_with_underlay()` records, in this order:

1. Native 3D meshes (depth pass), if any.
2. Flat-color batch — **all** flat commands coalesced into one vertex buffer,
   one bind, one draw.
3. Textured loop — one buffer + bind + draw per command.
4. Grating/Gabor loop — one buffer + bind + draw (or two, for additive Gabor)
   per command.
5. Dots loop — one instanced draw per command.

**Draw order is by primitive type, not call order.** A texture always
composites over all flat shapes regardless of the order `draw_rect` vs
`draw_texture` was called. This is a load-bearing semantic, not an accident.

> **Superseded.** This was the behavior before the branch; §4-5 replace it with
> call-order compositing, which is what ships. Adjacent draws sharing a pipeline
> still coalesce, but a draw issued between them splits the run rather than
> being reordered around it.

### The escape hatches that exist today

`RenderContext` exposes `device()`, `queue()`, and `swapchain()` "for advanced
users," and the external-frame ring (`core::external_frame`) lets an entirely
separate renderer hand VSE a finished image as an underlay. But there is **no
seam to participate in VSE's own frame**: you cannot register a pipeline VSE will
bind, nor inject command-buffer recording into VSE's render pass. The
"Writing Your Own Pipeline" section of `docs/guides/pipelines.md` is really
*"how to fork VSE's internals"* — seven steps editing four files. Closing that
gap is the purpose of this document.

---

## 2. Constraints any redesign must respect

These are the load-bearing facts that shape the whole design:

1. **A stimulus is not always one pipeline or one pass.** Additive Gabor already
   uses *two* pipelines (add + reverse-subtract) driven by one shared
   push-constant struct with a `composite_mode` selector. The abstraction must
   let one registered stimulus own N pipelines and N passes.
2. **Pipeline creation state genuinely varies.** Most pipelines share
   `create_graphics_pipeline` (alpha blend, no depth, `TriangleList`), but `dot`
   needs custom two-binding vertex input (per-vertex quad + per-instance
   position) and `mesh_normals` needs depth test, back-face cull, and a depth
   attachment format. A generic descriptor must expose blend, depth,
   vertex-input, topology, and color/depth formats.
3. **Timing determinism is the point of VSE.** Pipelines must be built at
   startup or between trials, never on the presentation path. The registry must
   make "build once" the *only* easy path and make hot-path compilation hard to
   do by accident.
4. **Draw order is a semantic users depend on.** Any registration model forces a
   decision about where custom draws land relative to built-ins (see §4).
5. **Vertex formats are already public** (`Vertex2D`, `TexturedVertex`,
   `DotInstance`, `Vertex3D` are in the prelude). Users have the vocabulary; they
   lack the verbs (allocators, the command-buffer builder, and `DrawCommand` are
   private).

---

## 3. Target model: three tiers

A single "register a pipeline" call is the wrong shape, because two goals —
*teach how pipelines work* and *provide easy defaults* — live at different
altitudes. The proposal is three layers that share machinery. They compose;
each is independently useful.

### Tier 0 — Suite subselection (historical design)

> **Superseded by Audits 7 and 7b.** Runtime subselection saved approximately 40 ms with a cold driver cache but introduced configurations where a valid `draw_*` call rendered nothing. VSE now constructs its seven base 2D pipelines unconditionally. Controlled 3D lives in `vse-3d` and supplies complete images through the external-frame boundary.

The original proposal was to turn the eight hardcoded fields into a `PipelineSuite` assembled from built-in
`PipelineModule`s, chosen at build time:

```rust
// Everything (today's behavior, the default):
VSEContext::builder().with_pipelines(PipelineSuite::default());

// Only what this experiment uses:
VSEContext::builder().with_pipelines(
    PipelineSuite::minimal()          // flat_color only
        .with(builtin::gabor())
        .with(builtin::dots()),
);
```

This directly answers *"subselect the pipelines that come included with VSE"* and
introduces no new user concepts. Its real value is internal: it forces `render()`
to iterate over modules instead of hardcoded passes — the prerequisite for
Tiers 1 and 2.

### Tier 1 — `StimulusPipeline` trait + registry (the teaching layer, ~90% case)

A user implements a trait that builds one-or-more pipelines once and records
draws for its queued commands. Registration returns a typed handle; enqueue with
`draw_with`.

```rust
pub trait StimulusPipeline {
    /// The user's per-draw parameter type (kept concrete for the user).
    type Command: Send + 'static;

    /// Build pipeline(s) once, at startup / between trials. Never on the hot path.
    fn build(&mut self, cx: &PipelineBuildCtx) -> Result<(), PipelineError>;

    /// Record draws for this frame's queued commands into the active render pass.
    fn record(&self, cx: &mut RecordCtx, commands: &[Self::Command]);
}

let checker = vse.register_pipeline(CheckerPipeline::new())?; // -> Pipeline<Checker>
vse.draw_with(checker, CheckerParams { rect, check_size });   // enqueues
```

`PipelineBuildCtx` hands over the device, allocators, and swapchain/depth
formats. `RecordCtx` exposes exactly the safe recording verbs (bind pipeline,
set push constants, bind vertex/index/instance buffers, draw) without leaking the
command-buffer lifetime soup. This collapses the docs' seven-step fork into
"implement one trait, register it," and it teaches the genuine model:
build-once → bind → push-constants → draw.

**Type erasure vs. push-constant safety.** vulkano-shaders' generated structs
give compile-time push-constant checking; a dynamic registry keyed by
`PipelineId` with `Box<dyn Any>` payloads would throw that away. Keeping the
trait generic over its own `Command` associated type and type-erasing only at the
*storage* boundary preserves the user's concrete params inside `record()`. The
built-in modules (Tier 0) can be re-expressed as `StimulusPipeline` impls, so
there is one code path, not two.

### Tier 2 — Raw record hook (the honest low-level escape)

```rust
vse.draw_custom(|builder, frame| {
    // Runs inside VSE's active render pass, viewport already set,
    // in call order. Full Vulkan access for anything the trait can't model.
});
```

This is the smallest thing that satisfies the "expose lower-level Vulkan API"
design goal. It sidesteps draw ordering entirely (the user controls their own
recording), and it is the pressure-release valve for multi-pass effects, a
compute pre-pass, or custom descriptor sets. **If we shipped only Tier 2, most
advanced users would already be unblocked** — it is the highest
power-to-effort ratio of the three, and a good candidate for the first
implementation milestone once this doc is approved.

---

## 4. Draw ordering: call-order

**Decision: move to call-order compositing.**

Today, compositing order is fixed by primitive type. The moment users add
pipelines, *"where does my stimulus draw relative to the built-in Gabor and the
flat shapes?"* becomes unanswerable under the type-ordered model. Call-order —
draws composite in the order `draw_*` / `draw_with` / `draw_custom` were called,
batching only *consecutive* same-pipeline commands — is what every other
immediate-mode API does and what users expect. It also enables true interleaving
(draw A, then a custom stimulus, then A on top).

**Migration impact.** This is a behavior change. Existing examples rely, however
implicitly, on the current type order (e.g. textures always landing over flat
shapes). Each example must be checked to confirm its intended compositing still
holds; most draw non-overlapping stimuli and are unaffected, but any that layer
a texture/grating over a flat background need a look. The
`renderer-draw-order` behavior note is superseded by this section once
implemented.

**Batching under call-order.** VSE groups runs of consecutive commands that share
a pipeline into a single coalesced draw where possible (see §5). Interleaving
pipelines breaks those runs — the intuitive cost of intuitive ordering.

---

## 5. Performance & batching semantics

Call-order composites intuitively, but pipeline switches have a cost. The
documentation must state the guidance plainly: **group same-pipeline draws
together for throughput.** The precise picture, grounded in the current renderer:

1. **Lost draw-call coalescing — the dominant cost.** Today
   `fill_flat_color_vertices()` coalesces *every* flat command
   (rect/circle/line/arc, and `draw_text`, which explodes into one rect per lit
   font pixel) into a single vertex buffer → **one bind, one draw**. That batch
   is the big win in the current renderer. Call-order preserves it only while
   flat draws stay *consecutive*; interleaving a texture between two rects splits
   the batch into two uploads + two draws. Fragmenting this coalesced flat draw —
   not the pipeline bind — is the real cost of unbatched call-order.

2. **Per-draw vertex-buffer allocation — already the status quo.** Every
   textured/grating/gabor/dots command already does a fresh `Buffer::from_iter` +
   bind + draw per call, every frame (`renderer.rs` loops at the textured,
   parametric, and dots passes). For those primitives, call-order changes nothing
   about batching; reordering them costs nothing new. A future pooled/persistent
   vertex-buffer optimization could remove this per-frame allocation — orthogonal
   to call-order, noted here so we don't imply ordering is the only performance
   lever.

3. **`vkCmdBindPipeline` state change — real but secondary.** On the reference
   hardware (Intel/ANV/Mesa) a pipeline bind is modest, and at typical stimulus
   counts (a handful of patches per frame) it is negligible in absolute terms. It
   matters pedagogically and at the margins (dense fields, many distinct
   pipelines), not as a headline number. Dense same-pipeline stimuli (RDK,
   many-Gabor fields) already batch via instancing/one pipeline and are
   unaffected.

**Guidance to publish:** batch same-pipeline draws together — primarily so the
flat-color coalescing survives, secondarily to avoid redundant pipeline binds.
This is a best practice, not a cliff. **Batching (speed) and interleaving
(compositing) pull in opposite directions**; the doc must say so, so users
understand the tradeoff they are choosing.

---

## 6. Shader story

Build-time `vulkano_shaders::shader!` means a "user-defined pipeline" today
requires editing VSE's source tree. Truly external pipelines need one of:

- **Build-time macro in the user's own crate** — the user's experiment binary
  already depends on VSE and is built from source; the same macro works there and
  keeps compile-time push-constant struct generation. *Recommended default.*
- **Accept SPIR-V bytes at registration** — the user brings a `.spv` compiled
  however they like. Zero new deps; pushes toolchain choice to the user; loses
  the generated push-constant struct (user supplies a `#[repr(C)]` type and
  asserts the layout). *Recommended runtime option.*
- **Runtime GLSL → SPIR-V** via `shaderc`/`naga` — nicest UX, heavy build
  dependency, also loses compile-time struct generation. *Optional future
  nice-to-have, not a must.*

For VSE's users (vision scientists compiling an experiment binary), the
pragmatic pair is **build-time macro in the user crate + SPIR-V bytes as the
runtime option.**

---

## 7. Migration path

The `draw_*` public API on `RenderContext` must not break. Sequence:

1. **Refactor built-ins into modules (internal only).** Re-express the eight
   pipelines as `PipelineModule`/`StimulusPipeline` impls behind the existing
   `Renderer` fields. `render()` iterates modules; `draw_*` methods still push the
   same commands. No public API change, no behavior change yet.
2. **Introduce call-order recording** behind the module iteration; audit and fix
   examples (§4). This is the one behavior change and should land as its own
   reviewable step.
3. **Expose Tier 0** (`with_pipelines` / `PipelineSuite`) — additive; default
   stays "all built-ins."
4. **Expose Tier 2** (`draw_custom`) — smallest new surface, unblocks advanced
   users early.
5. **Expose Tier 1** (`register_pipeline` / `StimulusPipeline` / `draw_with`) —
   the largest new surface; lands last, on top of the module refactor.
6. **Rewrite `docs/guides/pipelines.md`** — replace the seven-step fork with the
   trait + registry workflow, and add the §5 performance guidance.

---

## 8. Open questions & sequencing

> **Resolved and later revised.** The work shipped in the order: keyed registry → `PipelineSuite`
> → Tier 2 (`draw_custom`) → call-order → unified draw queue → Tier 1
> (`StimulusPipeline`). Audit 7 later removed `PipelineSuite` after measurement and made the standard built-ins unconditional. Audit 7b extracted 3D into the `vse-3d` crate rather than adding an in-core capability. The questions below remain as a record of the original design process.

- **First implementation milestone:** Tier 2 (raw hook) vs. the Tier 0 module
  refactor. Tier 2 unblocks users soonest; the module refactor is the structural
  prerequisite for everything and de-risks call-order. Likely order: module
  refactor → call-order → Tier 2 → Tier 0 → Tier 1.
- **`RecordCtx` surface:** exactly which recording verbs to expose, and whether
  descriptor-set binding (needed for custom textures/UBOs) is in Tier 1 or
  reserved for Tier 2.
- **Vertex input for custom pipelines:** constrain Tier 1 to the existing vertex
  formats (quad/instance conventions) at first, or allow arbitrary
  `VertexInputState` from the start?
- **Push-constant safety for SPIR-V path:** how much layout validation VSE should
  do vs. trust the user's `#[repr(C)]` type.
- **Resolved by Audit 7b:** `StimulusPipeline` remains 2D-overlay only. External
  renderers own depth and complete their frames before VSE's 2D pass. A future
  direct-underlay seam, if measurements justify it, will be a separate advanced API.
