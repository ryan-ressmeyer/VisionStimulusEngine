# /// script
# requires-python = ">=3.10"
# dependencies = ["pandas", "matplotlib", "numpy"]
# ///
"""Analyze a VSE metacontrast-masking session.

Produces two panels from the per-trial CSV written by
`examples/14_metacontrast_masking.rs`:

  1. The masking function — proportion correct vs SOA. A Type-B result shows
     a U-shaped dip at intermediate SOA (~40-64 ms).
  2. Timing fidelity — realized SOA (from recorded scanout onset times) vs
     the requested SOA. Points on the identity line mean the engine presented
     each target and mask exactly when scheduled.

Usage:
    uv run examples/plot_metacontrast.py [path/to/metacontrast_*.csv]

With no argument, the most recent metacontrast_*.csv in the working
directory is used.
"""

import glob
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd


def find_csv(argv: list[str]) -> Path:
    if len(argv) > 1:
        return Path(argv[1])
    candidates = sorted(glob.glob("metacontrast_*.csv"))
    if not candidates:
        sys.exit(
            "No CSV given and no metacontrast_*.csv found. "
            "Run the demo first: cargo run --release --example 14_metacontrast_masking"
        )
    return Path(candidates[-1])


def main() -> None:
    csv_path = find_csv(sys.argv)
    df = pd.read_csv(csv_path)
    print(f"Loaded {len(df)} trials from {csv_path}")

    # --- Masking function: proportion correct per requested SOA ---
    grouped = df.groupby("requested_soa_ms")["correct"]
    acc = grouped.mean()
    n = grouped.size()
    # Wald standard error of a proportion, for error bars.
    se = np.sqrt(acc * (1.0 - acc) / n)

    overall = df["correct"].mean()
    print(f"Overall accuracy: {overall:.3f}  (chance = 0.5)")
    print("Masking function (proportion correct by SOA):")
    for soa, a, k in zip(acc.index, acc.values, n.values):
        print(f"  SOA {soa:6.1f} ms:  {a:.3f}  (n={k})")

    # --- Timing fidelity: realized vs requested SOA ---
    resid = df["realized_soa_ms"] - df["requested_soa_ms"]
    mae = resid.abs().mean()
    print(
        f"Timing: realized−requested SOA = {resid.mean():+.3f} ms "
        f"(mean), |err| {mae:.3f} ms, sd {resid.std():.3f} ms"
    )

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4.5))

    # Panel 1 — masking function
    ax1.axhline(0.5, ls="--", lw=1, color="0.6", label="chance")
    ax1.errorbar(
        acc.index, acc.values, yerr=se.values,
        marker="o", ms=6, lw=1.8, capsize=3, color="#2b6cb0",
    )
    ax1.set_xlabel("SOA (ms)")
    ax1.set_ylabel("Proportion correct")
    ax1.set_title("Metacontrast masking function")
    ax1.set_ylim(0.4, 1.02)
    ax1.legend(frameon=False, loc="lower right")

    # Panel 2 — timing fidelity
    lo = min(df["requested_soa_ms"].min(), df["realized_soa_ms"].min()) - 5
    hi = max(df["requested_soa_ms"].max(), df["realized_soa_ms"].max()) + 5
    ax2.plot([lo, hi], [lo, hi], ls="--", lw=1, color="0.6", label="ideal (y = x)")
    ax2.scatter(
        df["requested_soa_ms"], df["realized_soa_ms"],
        s=18, alpha=0.35, color="#dd6b20", edgecolor="none",
    )
    ax2.set_xlabel("Requested SOA (ms)")
    ax2.set_ylabel("Realized SOA (ms)")
    ax2.set_title(f"Timing fidelity  (|err| = {mae:.2f} ms)")
    ax2.set_xlim(lo, hi)
    ax2.set_ylim(lo, hi)
    ax2.set_aspect("equal", adjustable="box")
    ax2.legend(frameon=False, loc="upper left")

    fig.tight_layout()
    out = csv_path.with_name(csv_path.stem + "_analysis.png")
    fig.savefig(out, dpi=130)
    print(f"Wrote {out}")


if __name__ == "__main__":
    main()
