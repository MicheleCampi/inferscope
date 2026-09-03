#!/usr/bin/env python3
"""Generate the speculative-config JSONs for the ADR-016 energy campaign.

Two arms at matched mean acceptance length, so they differ in the dispersion
of acceptance and in nothing else (ADR-016 D7):

  min-variance  the schedule vLLM itself resolves from a scalar
                synthetic_acceptance_length, reproduced here as an explicit
                vector so both arms travel the same code path
  geometric     p^(i+1) with p solved so the mean matches

Plus a baseline with speculation off. The crossover is defined against that
baseline; without it the sweep points only compare to each other and there is
no crossover to find.

Every vector is checked against the conditions
SpeculativeConfig._resolve_synthetic_acceptance_rates enforces (length,
range, monotonicity), verified at source against vllm-project/vllm d410fc1.
"""

import json
from pathlib import Path

K = 5
TARGET = "meta-llama/Llama-3.1-8B-Instruct"
DRAFT = "meta-llama/Llama-3.2-1B-Instruct"
SWEEP = [1.0, 2.0, 3.0, 4.0, 5.0]
OUT = Path(__file__).parent / "configs"


def min_variance(L, n=K):
    """vLLM's own resolution, from _acceptance_length_to_rates."""
    d = L - 1.0
    full = int(d)
    return ([1.0] * full + [d - full] + [0.0] * (n - full - 1))[:n]


def geometric(L, n=K, iters=200):
    """p such that sum(p^i, i=1..n) == L-1, by bisection."""
    target = L - 1.0
    if target <= 0:
        return [0.0] * n
    lo, hi = 0.0, 1.0
    for _ in range(iters):
        p = (lo + hi) / 2
        if sum(p**i for i in range(1, n + 1)) < target:
            lo = p
        else:
            hi = p
    p = (lo + hi) / 2
    return [round(p**i, 6) for i in range(1, n + 1)]


def validate(rates, L, n=K):
    """The three conditions vLLM enforces, plus the pairing this campaign needs."""
    assert len(rates) == n, f"length {len(rates)} != {n}"
    assert all(0.0 <= r <= 1.0 for r in rates), f"outside [0,1]: {rates}"
    assert all(rates[i] <= rates[i - 1] for i in range(1, n)), f"not non-increasing: {rates}"
    realized = 1.0 + sum(rates)
    assert abs(realized - L) < 1e-4, f"arms not matched: asked {L}, vector gives {realized}"
    return realized


def spec_config(rates):
    return {
        "method": "draft_model",
        "model": DRAFT,
        "num_speculative_tokens": K,
        "rejection_sample_method": "synthetic",
        "synthetic_acceptance_rates": rates,
    }


def main():
    OUT.mkdir(exist_ok=True)
    written = []

    for L in SWEEP:
        for arm, fn in (("minvar", min_variance), ("geom", geometric)):
            rates = fn(L)
            # At L=1.0 both arms are the all-zero vector: there is no
            # dispersion to differ in. Keep one, not two runs of the same
            # configuration.
            if L == 1.0 and arm == "geom":
                continue
            validate(rates, L)
            path = OUT / f"L{L:.2f}-{arm}.json"
            path.write_text(json.dumps(spec_config(rates), indent=2) + "\n")
            written.append((path.name, L, rates))

    print(f"{'file':<22} {'L':>5}  rates")
    for name, L, rates in written:
        print(f"{name:<22} {L:>5.2f}  {rates}")
    print(f"\n{len(written)} speculative configs in {OUT}")
    print("Baseline: no --speculative-config at all. Not a file; the absence is the arm.")


if __name__ == "__main__":
    main()
