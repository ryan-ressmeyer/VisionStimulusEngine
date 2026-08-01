#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "numpy==2.3.2",
#   "trimesh==4.7.4",
# ]
# ///
"""Download and reproducibly convert the native 3D demo assets to GLB."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import re
import struct
import tarfile
import urllib.request
from pathlib import Path

import numpy as np
import trimesh

SOURCES = {
    "bunny": (
        "https://graphics.stanford.edu/pub/3Dscanrep/bunny.tar.gz",
        "a5720bd96d158df403d153381b8411a727a1d73cff2f33dc9b212d6f75455b84",
    ),
    "teapot": (
        "https://raw.githubusercontent.com/freeglut/freeglut/master/src/fg_teapot_data.h",
        "e6b5587e62acb59564c3801b18ed377c05e78083fbcc70b7bab2caeb4c573ed9",
    ),
    "suzanne_gltf": (
        "https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/Suzanne/glTF/Suzanne.gltf",
        "7e8ae013010aff530162ef2795cec74c2646019e224af17bcfb691664f0f0aec",
    ),
    "suzanne_bin": (
        "https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/Suzanne/glTF/Suzanne.bin",
        "b85c2727aa41318e00673d8892f5879d46fb6e476e280f28ee1febd07602b6b8",
    ),
    "benchy": (
        "https://raw.githubusercontent.com/CreativeTools/3DBenchy/master/Single-part/3DBenchy.stl",
        "6ab57f1c3f8e86bc3cbd302c6fa6270acf06277c6335454e922419c25d42e97e",
    ),
}

# Filled after conversion has been verified. Keeping these in the tool makes
# --check detect converter or dependency drift, not only truncated files.
OUTPUT_SHA256 = {
    "bunny.glb": "1abca3aa71883f43a94dbab1563cd539a432815d8f65cea748d7bd2985dbff1e",
    "teapot.glb": "c0d4443a1c19c053f99ebf1e4dae175e11e1ea650f6b1dc1353bc97742b13218",
    "suzanne.glb": "ec125a49105bae77092829bb2417329325e9427c021d5d18e11fe20ba5955aab",
    "benchy.glb": "bc50e8f709b26627afa4ecd95341c169d0f8fd21d6f077db988d31ce2f398743",
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def download(name: str) -> bytes:
    url, expected = SOURCES[name]
    print(f"download {name}: {url}")
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read()
    actual = sha256(data)
    if actual != expected:
        raise RuntimeError(f"{name} source SHA-256 mismatch: expected {expected}, got {actual}")
    return data


def pad4(data: bytes, byte: bytes = b"\0") -> bytes:
    return data + byte * ((-len(data)) % 4)


def make_glb(vertices: np.ndarray, faces: np.ndarray, generator: str) -> bytes:
    vertices = np.ascontiguousarray(vertices, dtype="<f4")
    faces = np.ascontiguousarray(faces.reshape(-1), dtype="<u4")
    vertex_bytes = vertices.tobytes()
    index_offset = len(pad4(vertex_bytes))
    binary = pad4(vertex_bytes) + faces.tobytes()
    document = {
        "asset": {"version": "2.0", "generator": generator},
        "buffers": [{"byteLength": len(binary)}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": len(vertex_bytes), "target": 34962},
            {"buffer": 0, "byteOffset": index_offset, "byteLength": faces.nbytes, "target": 34963},
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126,
                "count": len(vertices),
                "type": "VEC3",
                "min": vertices.min(axis=0).astype(float).tolist(),
                "max": vertices.max(axis=0).astype(float).tolist(),
            },
            {
                "bufferView": 1,
                "componentType": 5125,
                "count": len(faces),
                "type": "SCALAR",
                "min": [int(faces.min())],
                "max": [int(faces.max())],
            },
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 4}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}],
        "scene": 0,
    }
    json_bytes = pad4(json.dumps(document, separators=(",", ":")).encode(), b" ")
    binary = pad4(binary)
    total = 12 + 8 + len(json_bytes) + 8 + len(binary)
    return b"".join(
        [
            struct.pack("<III", 0x46546C67, 2, total),
            struct.pack("<II", len(json_bytes), 0x4E4F534A),
            json_bytes,
            struct.pack("<II", len(binary), 0x004E4942),
            binary,
        ]
    )


def mesh_geometry(mesh: trimesh.Trimesh) -> tuple[np.ndarray, np.ndarray]:
    if not isinstance(mesh, trimesh.Trimesh):
        mesh = mesh.dump(concatenate=True)
    return np.asarray(mesh.vertices), np.asarray(mesh.faces)


def close_boundaries(vertices: np.ndarray, faces: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Cap every open boundary loop with a centroid triangle fan.

    Scanned and Bezier-patch demo meshes leave holes at the base (the Stanford
    bunny's unscanned underside; the 28-patch Newell teapot's open bottom, spout
    and handle ends). Each boundary edge belongs to exactly one triangle, so its
    complement direction closes the surface: filling with ``(b, a, center)``
    inherits the mesh's already-consistent winding. Boundary edges are grouped
    into holes by connected component, which is robust to the non-manifold pinch
    at the teapot lid pole; sub-triangle specks there are skipped.
    """
    face_list = [tuple(int(i) for i in face) for face in faces]
    edge_counts: dict[tuple[int, int], int] = {}
    for i, j, k in face_list:
        for edge in ((i, j), (j, k), (k, i)):
            edge_counts[edge] = edge_counts.get(edge, 0) + 1
    boundary = [
        (a, b) for (a, b), count in edge_counts.items() if count == 1 and (b, a) not in edge_counts
    ]

    parent: dict[int, int] = {}

    def find(x: int) -> int:
        root = parent.setdefault(x, x)
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    def union(a: int, b: int) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[max(ra, rb)] = min(ra, rb)

    for a, b in boundary:
        union(a, b)
    holes: dict[int, list[tuple[int, int]]] = {}
    for a, b in boundary:
        holes.setdefault(find(a), []).append((a, b))

    verts = [np.asarray(v, dtype=np.float64) for v in vertices]
    out_faces = list(face_list)
    for root in sorted(holes):
        edges = holes[root]
        loop_vertices = sorted({v for edge in edges for v in edge})
        if len(loop_vertices) < 3:
            continue
        center = np.mean([verts[i] for i in loop_vertices], axis=0)
        center_index = len(verts)
        verts.append(center)
        for a, b in edges:
            out_faces.append((b, a, center_index))
    return np.asarray(verts), np.asarray(out_faces, dtype=np.uint32)


def weld_and_close(vertices: np.ndarray, faces: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Merge coincident vertices, drop degenerate triangles, then cap open holes."""
    mesh = trimesh.Trimesh(
        vertices=np.asarray(vertices, dtype=np.float64),
        faces=np.asarray(faces),
        process=False,
    )
    mesh.merge_vertices()
    welded_vertices = np.asarray(mesh.vertices)
    welded_faces = np.asarray(mesh.faces)
    keep = (
        (welded_faces[:, 0] != welded_faces[:, 1])
        & (welded_faces[:, 1] != welded_faces[:, 2])
        & (welded_faces[:, 0] != welded_faces[:, 2])
    )
    return close_boundaries(welded_vertices, welded_faces[keep])


def convert_bunny(source: bytes) -> bytes:
    with tarfile.open(fileobj=io.BytesIO(source), mode="r:gz") as archive:
        member = archive.getmember("bunny/reconstruction/bun_zipper.ply")
        ply = archive.extractfile(member)
        if ply is None:
            raise RuntimeError("Stanford archive lacks bun_zipper.ply")
        mesh = trimesh.load_mesh(file_obj=io.BytesIO(ply.read()), file_type="ply", process=False)
    vertices, faces = mesh_geometry(mesh)
    # Stanford's reconstructed bunny coordinates are conventionally interpreted
    # as meters and already use +Y up. The scan leaves the underside open, so
    # cap the base holes to make a closed solid.
    vertices, faces = weld_and_close(vertices, faces)
    return make_glb(vertices, faces, "VSE prepare.py; Stanford bun_zipper.ply; capped")


def parse_teapot(source: bytes) -> tuple[np.ndarray, np.ndarray]:
    text = source.decode("utf-8")
    start = text.index("Martin Newell's teapot made famous")
    lines = text[start:].splitlines()
    count_line = next(i for i, line in enumerate(lines) if re.fullmatch(r"\s*269\s+1\s+28\s+28\s*", line))
    points = []
    for line in lines[count_line + 1 : count_line + 1 + 269]:
        fields = line.split()
        points.append([float(fields[1]), float(fields[2]), float(fields[3])])
    patches = []
    for line in lines[count_line + 1 + 269 : count_line + 1 + 269 + 28]:
        indices = [int(value) for value in line.split()]
        indices[0] = abs(indices[0])
        patches.append([value - 1 for value in indices])

    control = np.asarray(points, dtype=np.float64)
    patch_indices = np.asarray(patches, dtype=np.int64)
    steps = 12
    t = np.linspace(0.0, 1.0, steps + 1)
    basis = np.stack(((1 - t) ** 3, 3 * t * (1 - t) ** 2, 3 * t**2 * (1 - t), t**3), axis=1)
    vertices: list[np.ndarray] = []
    faces: list[list[int]] = []
    for patch in patch_indices:
        grid = control[patch].reshape(4, 4, 3)
        evaluated = np.einsum("ui,ijc,vj->uvc", basis, grid, basis)
        base = len(vertices)
        vertices.extend(evaluated.reshape(-1, 3))
        for u in range(steps):
            for v in range(steps):
                a = base + u * (steps + 1) + v
                b = a + steps + 1
                c = b + 1
                d = a + 1
                faces.extend(([a, b, c], [a, c, d]))
    vertices_array = np.asarray(vertices)
    # The freeglut control mesh is already +Y up (its second coordinate is the
    # base-to-lid axis). Apply only the 0.1 meter per freeglut unit scale; a
    # uniform positive scale preserves triangle winding.
    vertices_array = vertices_array * 0.1
    return vertices_array, np.asarray(faces, dtype=np.uint32)


def convert_teapot(source: bytes) -> bytes:
    vertices, faces = parse_teapot(source)
    vertices, faces = weld_and_close(vertices, faces)
    return make_glb(vertices, faces, "VSE prepare.py; freeglut teapot; tessellation=12; capped")


def convert_suzanne(gltf_source: bytes, binary: bytes) -> bytes:
    document = json.loads(gltf_source)
    document["asset"]["generator"] = "VSE prepare.py; Khronos Suzanne"
    document["buffers"] = [{"byteLength": len(binary)}]
    # Native normal rendering ignores these fields. Removing them makes the GLB
    # self-contained without downloading unused textures.
    for field in ("images", "materials", "samplers", "textures"):
        document.pop(field, None)
    for mesh in document["meshes"]:
        for primitive in mesh["primitives"]:
            primitive.pop("material", None)
    json_bytes = pad4(json.dumps(document, separators=(",", ":")).encode(), b" ")
    binary = pad4(binary)
    total = 12 + 8 + len(json_bytes) + 8 + len(binary)
    return b"".join(
        [
            struct.pack("<III", 0x46546C67, 2, total),
            struct.pack("<II", len(json_bytes), 0x4E4F534A),
            json_bytes,
            struct.pack("<II", len(binary), 0x004E4942),
            binary,
        ]
    )


def convert_benchy(source: bytes) -> bytes:
    mesh = trimesh.load_mesh(file_obj=io.BytesIO(source), file_type="stl", process=False)
    vertices, faces = mesh_geometry(mesh)
    # Official STL coordinates are millimeters and Z-up.
    vertices = vertices[:, [0, 2, 1]] * np.array([0.001, 0.001, -0.001])
    return make_glb(vertices, faces, "VSE prepare.py; official 3DBenchy STL")


def validate_outputs(output: Path) -> bool:
    valid = True
    for filename, expected in OUTPUT_SHA256.items():
        path = output / filename
        if not path.is_file():
            print(f"missing: {path}")
            valid = False
            continue
        actual = sha256(path.read_bytes())
        if expected and actual != expected:
            print(f"hash mismatch: {path}: expected {expected}, got {actual}")
            valid = False
        else:
            print(f"ok: {actual}  {path}")
    return valid


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify prepared output without downloading")
    parser.add_argument("--output", type=Path, default=Path(__file__).parent / "models")
    args = parser.parse_args()
    if args.check:
        return 0 if validate_outputs(args.output) else 1

    args.output.mkdir(parents=True, exist_ok=True)
    outputs = {
        "bunny.glb": convert_bunny(download("bunny")),
        "teapot.glb": convert_teapot(download("teapot")),
        "suzanne.glb": convert_suzanne(download("suzanne_gltf"), download("suzanne_bin")),
        "benchy.glb": convert_benchy(download("benchy")),
    }
    for filename, data in outputs.items():
        path = args.output / filename
        path.write_bytes(data)
        print(f"wrote: {sha256(data)}  {path}")
    return 0 if validate_outputs(args.output) else 1


if __name__ == "__main__":
    raise SystemExit(main())
