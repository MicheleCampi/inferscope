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

TARGET="Qwen/Qwen2.5-3B-Instruct"
PORT=8000
WARMUP_S=60
SAMPLE_S=180
COOLDOWN_S=45
REQ_RATE=3
NUM_PROMPTS=800      # 240s of load at 3 req/s, with margin: the load must
                     # outlive the window, not finish inside it
INPUT_LEN=512
OUTPUT_LEN=256
SEED=20260903

mkdir -p "$OUT"
exec > >(tee -a "$OUT/session.log") 2>&1

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
    for _ in $(seq 1 60); do
        sleep 5
        if [[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/v1/models")" == "200" ]]; then
            elapsed=$(( $(date +%s) - t0 ))
            echo "    server ready in ${elapsed}s"
            echo "$elapsed" > "$OUT/$(basename "${cfg%.json}")-startup.txt"
            return 0
        fi
    done
    echo "    SERVER FAILED TO START"
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
    start_server "$cfg" || { echo "    run $n ABORTED"; continue; }

    PID="$(pgrep -f '[v]llm serve' | head -1)"

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
    echo "    exit $?"

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
