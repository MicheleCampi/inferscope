#!/usr/bin/env bash
# The discard criteria only bind if they have been seen to fire. These
# scenarios are built from the pilot's real report, perturbed one criterion
# at a time, and assert both the exit code and that no curve was printed.
#
# Usage:  ./test_analyse.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANALYSE="$HERE/../analyse.py"
PILOT="$HERE/../pilot/pilot.json"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fails=0

python3 - "$PILOT" "$WORK" << 'PYEOF'
import copy, json, shutil, sys
from pathlib import Path

src = json.loads(Path(sys.argv[1]).read_text())
work = Path(sys.argv[2])
valid = work / "valid"
valid.mkdir()

def make(d, name, scale=1.0, realized_L=None, empty_spec=False):
    r = copy.deepcopy(src)
    r["gpu"]["energy_millijoules"] = int(r["gpu"]["energy_millijoules"] * scale)
    if empty_spec:
        r["spec_timeline"]["samples"] = []
    elif realized_L is not None:
        s = r["spec_timeline"]["samples"]
        drafts = s[-1]["drafts"] - s[0]["drafts"]
        s[-1]["accepted_tokens"] = s[0]["accepted_tokens"] + int(drafts * (realized_L - 1))
    (d / f"{name}.json").write_text(json.dumps(r))

# A valid session: a crossover between L2 and L3, both arms.
make(valid, "baseline-open", 1.00)
make(valid, "baseline-close", 1.02)
for L, s in [(1.0, 1.30), (2.0, 1.10), (3.0, 0.95), (4.0, 0.85), (5.0, 0.80)]:
    make(valid, f"L{L:.2f}-minvar", s, realized_L=L)
for L, s in [(2.0, 1.12), (3.0, 0.97), (4.0, 0.86), (5.0, 0.81)]:
    make(valid, f"L{L:.2f}-geom", s, realized_L=L)

# One perturbation per criterion.
for name, mutate in [
    ("drift", lambda d: make(d, "baseline-close", 1.08)),
    ("missed_length", lambda d: make(d, "L4.00-minvar", 0.85, realized_L=3.4)),
    ("empty_spec", lambda d: make(d, "L3.00-geom", 0.97, empty_spec=True)),
]:
    d = work / name
    shutil.copytree(valid, d)
    mutate(d)
PYEOF

check() {
    local dir="$1" want_exit="$2" label="$3"
    local out; out="$(python3 "$ANALYSE" "$WORK/$dir" 2>&1)"; local got=$?
    if [[ "$got" != "$want_exit" ]]; then
        echo "FAIL $label: exit $got, expected $want_exit"; ((fails++)); return
    fi
    if [[ "$want_exit" == "1" ]] && grep -q "energy per committed token" <<< "$out"; then
        echo "FAIL $label: a curve was derived for an invalid session"; ((fails++)); return
    fi
    if [[ "$want_exit" == "0" ]] && ! grep -q "crossover" <<< "$out"; then
        echo "FAIL $label: a valid session produced no crossover section"; ((fails++)); return
    fi
    echo "ok   $label"
}

check valid         0 "a valid session derives a crossover"
check drift         1 "criterion 1: baseline drift discards the session"
check missed_length 1 "criterion 2: a missed acceptance length discards"
check empty_spec    1 "criterion 3: an empty speculative section discards"

echo
[[ $fails -eq 0 ]] && echo "all passed" || echo "$fails failed"
exit $fails
