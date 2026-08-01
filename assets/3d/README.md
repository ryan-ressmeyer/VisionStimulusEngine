# Native 3D demo assets

The native 3D demo uses four third-party models. They are not downloaded by `cargo build` and are not distributed under VSE's MIT license.

Prepare all four models with one opt-in command:

```bash
uv run assets/3d/prepare.py
```

The script downloads pinned authoritative sources, verifies their SHA-256 hashes, and writes meter-scaled, Y-up GLBs under the ignored `assets/3d/models/` directory. It uses pinned Python dependencies through PEP 723 metadata; it does not modify the Rust workspace or add network access to `cargo build`.

Verify an existing asset set without network access:

```bash
uv run assets/3d/prepare.py --check
```

Source provenance, conversion settings, and hashes are recorded in [`manifest.toml`](manifest.toml). The conversion preserves source triangles except for the freeglut Teapot, whose bicubic Bézier patches are tessellated deterministically at 12 subdivisions per axis.

The Bunny carries Stanford's research/non-commercial terms. Review those terms before downloading or redistributing it.

The demo loads and uploads all four files in its first startup callback. A missing or invalid file stops startup. Model switching performs no file access, decode, or GPU upload.
