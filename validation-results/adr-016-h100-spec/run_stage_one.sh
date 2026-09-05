#!/usr/bin/env bash
# Stage one of the ADR-016 campaign. Eleven runs, one session, indivisible.
#
# The discard criteria in PROTOCOL.md are applied here, in code, because a
# criterion applied by hand at 2am with the GPU billing is a criterion that
# gets negotiated. This script does not decide anything the protocol has not
# already declared; it only refuses to let the declaration slip.
#
# Usage:  ./run_stage_one.sh <results-dir>
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?usage: run_stage_one.sh <results-dir>}"
VENV="${VENV:-$HOME/venv}"
INFERSCOPE="${INFERSCOPE:-$HOME/inferscope/target/release/inferscope}"

# torch invokes ninja as an executable when it compiles kernels at engine
# start, and finds it on PATH rather than in the venv it was pip-installed
# into. Without this the engine dies with FileNotFoundError: 'ninja' and the
# server never comes up.
export PATH="$VENV/bin:$PATH"

TARGET="Qwen/Qwen2.5-3B-Instruct"
PORT=8000
# Overridable so a shortened rehearsal can exercise the whole flow without
# editing the file the real session runs. The defaults are the protocol's.
WARMUP_S="${WARMUP_S:-60}"
SAMPLE_S="${SAMPLE_S:-180}"
COOLDOWN_S="${COOLDOWN_S:-45}"
REQ_RATE="${REQ_RATE:-3}"
NUM_PROMPTS="${NUM_PROMPTS:-1600}"   # 240s of load at 3 req/s, with margin:
                                    # the load must outlive the window, not
                                    # finish inside it
INPUT_LEN="${INPUT_LEN:-512}"
OUTPUT_LEN="${OUTPUT_LEN:-256}"
SEED="${SEED:-20260903}"
STARTUP_TIMEOUT_TICKS="${STARTUP_TIMEOUT_TICKS:-90}"

echo "params: warmup=${WARMUP_S}s window=${SAMPLE_S}s cooldown=${COOLDOWN_S}s"
if [[ "$WARMUP_S" != "60" || "$SAMPLE_S" != "180" ]]; then
    echo "WARNING: non-default timings — this is a rehearsal, not campaign data"
fi

mkdir -p "$OUT"
exec > >(tee -a "$OUT/session.log") 2>&1

# --gpu only exists when inferscope was built with --features gpu-nvidia, and
# a binary built without it reports no energy at all - which is the whole
# measurement. Checking here costs a second; discovering it at analysis costs
# the session.
# --help exits 2 on this binary, and under `set -o pipefail` that status
# propagates through the pipe even when grep matches — so the gate fired on a
# binary that did have the flag. Capture the text first, test it after.
INFERSCOPE_HELP="$("$INFERSCOPE" --help 2>&1 || true)"
if ! printf %s "$INFERSCOPE_HELP" | grep -q -- "--gpu"; then
    echo "FATAL: $INFERSCOPE has no --gpu flag."
    echo "It was built without --features gpu-nvidia and cannot measure energy."
    echo "Rebuild on this host: cargo build --release --features gpu-nvidia"
    exit 1
fi

echo "=== ADR-016 stage one — $(date -Is) ==="
echo "seed=$SEED rate=$REQ_RATE prompts=$NUM_PROMPTS warmup=${WARMUP_S}s window=${SAMPLE_S}s"

# Run order: baseline, the nine configs shuffled, baseline. Randomised so
# thermal drift does not correlate with L; the seed is recorded so the order
# is reproducible.
mapfile -t SHUFFLED < <(ls "$HERE"/configs/*.json | shuf --random-source=<(yes "$SEED"))
RUNS=("BASELINE" "${SHUFFLED[@]}" "BASELINE")
echo "run order:"
for i in "${!RUNS[@]}"; do echo "  $((i+1)). $(basename "${RUNS[$i]}")"; done
printf '%s\n' "${RUNS[@]}" > "$OUT/run-order.txt"

start_server() {
    local cfg="$1" t0 elapsed
    t0=$(date +%s)
    if [[ "$cfg" == "BASELINE" ]]; then
        nohup "$VENV/bin/vllm" serve "$TARGET" --port "$PORT" --max-model-len 4096 \
            > "$OUT/server.log" 2>&1 < /dev/null &
    else
        nohup "$VENV/bin/vllm" serve "$TARGET" --port "$PORT" --max-model-len 4096 \
            --speculative-config "$(cat "$cfg")" \
            > "$OUT/server.log" 2>&1 < /dev/null &
    fi
    disown
    for _ in $(seq 1 "$STARTUP_TIMEOUT_TICKS"); do
        sleep 5
        if [[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/v1/models")" == "200" ]]; then
            elapsed=$(( $(date +%s) - t0 ))
            echo "    server ready in ${elapsed}s"
            echo "$elapsed" > "$OUT/$(basename "${cfg%.json}")-startup.txt"
            return 0
        fi
    done
    echo "    SERVER FAILED TO START — aborting the session"
    grep -m 3 -E "Error|Traceback|FileNotFoundError" "$OUT/server.log" 2>/dev/null
    return 1
}

stop_server() {
    pkill -f "[v]llm serve" || true
    sleep 10
    for _ in $(seq 1 12); do
        [[ -z "$(pgrep -f '[v]llm serve')" ]] && break
        sleep 5
    done
}

for i in "${!RUNS[@]}"; do
    cfg="${RUNS[$i]}"
    n=$((i+1))
    if [[ "$cfg" == "BASELINE" ]]; then
        # Two baselines per session; the second must not overwrite the first.
        [[ -f "$OUT/baseline-open.json" ]] && name="baseline-close" || name="baseline-open"
    else
        name="$(basename "${cfg%.json}")"
    fi

    echo
    echo "--- run $n/${#RUNS[@]}: $name  $(date -Is) ---"
    stop_server
    # A run whose server never came up produces no report, which fails discard
    # criterion 5 and invalidates the session. Continuing would spend the rest
    # of the GPU time on data that is already discarded — which is exactly what
    # happened on the first launch: two runs burned before anyone looked.
    start_server "$cfg" || { stop_server; exit 1; }

    PID="$(pgrep -f '[v]llm serve' | head -1)"
    if [[ -z "$PID" ]]; then
        echo "    NO SERVER PID — aborting the session"
        stop_server
        exit 1
    fi

    # Load first, sampling starts after the warm-up has passed.
    nohup "$VENV/bin/vllm" bench serve --backend openai-chat \
        --endpoint /v1/chat/completions --model "$TARGET" \
        --dataset-name random --random-input-len "$INPUT_LEN" \
        --random-output-len "$OUTPUT_LEN" --num-prompts "$NUM_PROMPTS" \
        --request-rate "$REQ_RATE" --ignore-eos --seed "$SEED" \
        > "$OUT/$name-bench.log" 2>&1 < /dev/null &
    disown

    echo "    warm-up ${WARMUP_S}s"
    sleep "$WARMUP_S"
    echo "    sampling ${SAMPLE_S}s"
    "$INFERSCOPE" --sample-only --pid "$PID" --duration-secs "$SAMPLE_S" --gpu \
        --metrics-endpoint "http://127.0.0.1:$PORT/metrics" \
        --engine vllm --model "$TARGET" \
        > "$OUT/$name.json" 2> "$OUT/$name.err"
    rc=$?

    # A run that produced no readable report fails discard criterion 5, which
    # invalidates the whole session. Continuing would spend the remaining GPU
    # time producing data that has already been discarded, and the emptiness
    # would surface only at analysis.
    if [[ $rc -ne 0 ]] || [[ ! -s "$OUT/$name.json" ]]; then
        echo "    RUN PRODUCED NO REPORT (exit $rc) — aborting the session"
        sed -n '1,5p' "$OUT/$name.err"
        stop_server
        exit 1
    fi
    echo "    exit $rc, $(wc -c < "$OUT/$name.json") bytes"

    # The load must still be running when the window closes. If it finished
    # early, the tail of the window measured an idle GPU (discard criterion 4).
    if ! pgrep -f "[b]ench serve" > /dev/null; then
        echo "    WARNING: load finished before the window closed"
        echo "load-underran" > "$OUT/$name.discard"
    fi

    pkill -f "[b]ench serve" || true
    echo "    cooldown ${COOLDOWN_S}s"
    sleep "$COOLDOWN_S"
done

stop_server
echo
echo "=== stage one complete — $(date -Is) ==="
ls -1 "$OUT"/*.json
