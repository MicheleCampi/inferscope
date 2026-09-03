#!/usr/bin/env python3
"""Analysis for ADR-016 stage one.

Validation runs before derivation, and derivation does not run if validation
fails. A session that violates a discard criterion has no curve: printing one
anyway would mean the numbers get read before the verdict does, which is the
failure the criteria exist to prevent.

Usage:  analyse.py <results-dir>
"""

import json
import sys
from pathlib import Path

BASELINE_TOLERANCE = 0.05   # discard criterion 1
LENGTH_TOLERANCE = 0.1      # discard criterion 2
ARM_AGREEMENT = 0.25        # falsification threshold on the instrument (D7)


def window(path):
    """Window deltas from one report, or a reason it cannot be read."""
    r = json.loads(path.read_text())

    gpu = r.get("gpu")
    if not gpu or not gpu.get("energy_millijoules"):
        return None, "no energy measured"
    energy_mj = gpu["energy_millijoules"]

    ph = r.get("phase_timeline") or {}
    ph_s = ph.get("samples") or []
    if len(ph_s) < 2:
        return None, "phase timeline too short to difference"
    gen = ph_s[-1]["generation_tokens"] - ph_s[0]["generation_tokens"]
    if gen <= 0:
        return None, "no generation tokens in the window"

    out = {"energy_mj": energy_mj, "generation_tokens": gen,
           "mj_per_token": energy_mj / gen}

    sp = r.get("spec_timeline") or {}
    sp_s = sp.get("samples") or []
    if len(sp_s) >= 2:
        drafts = sp_s[-1]["drafts"] - sp_s[0]["drafts"]
        accepted = sp_s[-1]["accepted_tokens"] - sp_s[0]["accepted_tokens"]
        draft_tokens = sp_s[-1]["draft_tokens"] - sp_s[0]["draft_tokens"]
        if drafts > 0:
            # ADR-016 D4: the length includes the bonus token.
            out["realized_L"] = 1 + accepted / drafts
            out["accept_rate"] = accepted / draft_tokens if draft_tokens else None
            out["tokens_per_draft"] = draft_tokens / drafts
    return out, None


def main():
    out_dir = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    reports = {p.stem: p for p in sorted(out_dir.glob("*.json"))}
    failures = []

    # --- discard criteria, before anything is derived --------------------
    print("=== validation ===")

    data = {}
    for name, path in reports.items():
        w, why = window(path)
        if w is None:
            failures.append(f"{name}: {why} (criterion 5)")
            continue
        if (out_dir / f"{name}.discard").exists():
            failures.append(f"{name}: {(out_dir / f'{name}.discard').read_text().strip()} (criterion 4)")
            continue
        data[name] = w

    # criterion 1: the two baselines bound the session
    b_open, b_close = data.get("baseline-open"), data.get("baseline-close")
    if not (b_open and b_close):
        failures.append("both baselines are required to validate the session (criterion 1)")
    else:
        drift = abs(b_close["mj_per_token"] - b_open["mj_per_token"]) / b_open["mj_per_token"]
        verdict = "ok" if drift <= BASELINE_TOLERANCE else "EXCEEDS TOLERANCE"
        print(f"  baseline drift: {drift:.1%} (tolerance {BASELINE_TOLERANCE:.0%}) — {verdict}")
        if drift > BASELINE_TOLERANCE:
            failures.append(f"baseline drift {drift:.1%} exceeds {BASELINE_TOLERANCE:.0%}: "
                            f"the session is unstable and every run in it is discarded (criterion 1)")

    # criterion 2: the knob took effect
    for name, w in sorted(data.items()):
        if not name.startswith("L"):
            continue
        configured = float(name[1:5])
        if "realized_L" not in w:
            failures.append(f"{name}: empty speculative section (criterion 3)")
            continue
        err = abs(w["realized_L"] - configured)
        flag = "" if err <= LENGTH_TOLERANCE else "  <-- MISSED"
        print(f"  {name}: configured {configured:.2f}, realized {w['realized_L']:.4f}{flag}")
        if err > LENGTH_TOLERANCE:
            failures.append(f"{name}: realized {w['realized_L']:.4f} vs configured "
                            f"{configured:.2f} (criterion 2)")

    if failures:
        print("\n=== SESSION INVALID ===")
        for f in failures:
            print(f"  - {f}")
        print("\nNo curve is derived. The protocol discards rather than adjusts.")
        return 1

    print("  all criteria passed\n")

    # --- derivation, only now --------------------------------------------
    baseline = (b_open["mj_per_token"] + b_close["mj_per_token"]) / 2
    print("=== energy per committed token ===")
    print(f"  baseline (no speculation): {baseline:.3f} mJ/token\n")
    print(f"  {'run':<18} {'L':>5} {'mJ/token':>10} {'vs baseline':>12}")

    arms = {"minvar": [], "geom": []}
    for name, w in sorted(data.items()):
        if not name.startswith("L"):
            continue
        ratio = w["mj_per_token"] / baseline
        arm = "geom" if name.endswith("geom") else "minvar"
        arms[arm].append((w["realized_L"], ratio))
        print(f"  {name:<18} {w['realized_L']:>5.2f} {w['mj_per_token']:>10.3f} {ratio:>11.3f}x")

    # --- falsification: crossover, and whether the arms agree -------------
    def crossover(points):
        """Lowest L at which the ratio crosses below 1, by linear interpolation."""
        pts = sorted(points)
        for (l0, r0), (l1, r1) in zip(pts, pts[1:]):
            if r0 > 1.0 >= r1:
                return l0 + (r0 - 1.0) * (l1 - l0) / (r0 - r1)
        return None

    print("\n=== crossover ===")
    xs = {}
    for arm, pts in arms.items():
        x = crossover(pts)
        xs[arm] = x
        if x is None:
            below = all(r < 1.0 for _, r in pts)
            print(f"  {arm}: none in the swept range — speculation is "
                  f"{'always cheaper' if below else 'never cheaper'} here")
        else:
            print(f"  {arm}: L = {x:.3f}")

    if xs["minvar"] is not None and xs["geom"] is not None:
        gap = abs(xs["minvar"] - xs["geom"])
        print(f"\n  arms differ by {gap:.3f} (threshold {ARM_AGREEMENT})")
        if gap <= ARM_AGREEMENT:
            print("  -> the scalar knob is a sufficient instrument; the geometric "
                  "arm is dropped from later campaigns (D7)")
        else:
            print("  -> the scalar knob alone is insufficient; the geometric arm "
                  "is retained, and that is the finding (D7)")
    else:
        print("\n  the instrument question is unanswered by this campaign, "
              "not assumed (D7)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
