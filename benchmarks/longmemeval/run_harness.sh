#!/usr/bin/env bash
# LongMemEval correctness harness for mempill — STUB (NOT YET RUN)
#
# This script is a documented skeleton. All steps marked with TODO must be
# implemented before the harness can produce scores. Do not run this script
# in CI until all TODOs are resolved.
#
# Prerequisites:
#   - Python 3.11+
#   - mempill Python binding installed: pip install mempill (or local editable install)
#   - HuggingFace account with access to xiaowu0162/longmemeval
#   - OPENAI_API_KEY (or equivalent LLM API key) set in environment
#
# Usage:
#   ./run_harness.sh [--subset temporal_reasoning|knowledge_update|all]
#
# Output:
#   results/longmemeval_scores.json — per-category F1 scores
#   results/longmemeval_detail.jsonl — per-question predictions + gold answers

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_DIR="${SCRIPT_DIR}/results"
mkdir -p "${RESULTS_DIR}"

SUBSET="${1:---subset all}"

echo "[longmemeval] Starting harness (NOT YET IMPLEMENTED)"

# TODO: Step 1 — Download dataset
# python "${SCRIPT_DIR}/src/load_dataset.py" \
#   --output "${RESULTS_DIR}/longmemeval_raw.jsonl"
echo "[TODO] Step 1: download LongMemEval dataset from HuggingFace"
echo "       Requires: huggingface_hub installed, HF_TOKEN set"
echo "       Command: python src/load_dataset.py --output results/longmemeval_raw.jsonl"

# TODO: Step 2 — Extract facts from chat turns using LLM
# python "${SCRIPT_DIR}/src/ingest_session.py" \
#   --input "${RESULTS_DIR}/longmemeval_raw.jsonl" \
#   --output "${RESULTS_DIR}/longmemeval_ingested.jsonl" \
#   --mempill-agent "longmemeval-eval-agent"
echo ""
echo "[TODO] Step 2: extract facts from sessions and ingest into mempill"
echo "       Requires: mempill Python binding, LLM API key for fact extraction"
echo "       Each chat turn maps to one or more IngestClaimRequest calls"
echo "       Session index → tx_time to preserve transaction-time ordering"

# TODO: Step 3 — Run queries against ingested facts
# python "${SCRIPT_DIR}/src/evaluate.py" \
#   --input "${RESULTS_DIR}/longmemeval_raw.jsonl" \
#   --agent "longmemeval-eval-agent" \
#   --subset "${SUBSET}" \
#   --output "${RESULTS_DIR}/longmemeval_detail.jsonl"
echo ""
echo "[TODO] Step 3: run queries and score against gold answers"
echo "       For temporal_reasoning questions: use valid_at=<question_time>"
echo "       For knowledge_update questions: use query_memory with valid_at=None"
echo "       Score: exact-match F1 against gold answers (no LLM judge)"

# TODO: Step 4 — Summarise results
# python "${SCRIPT_DIR}/src/summarise.py" \
#   --input "${RESULTS_DIR}/longmemeval_detail.jsonl" \
#   --output "${RESULTS_DIR}/longmemeval_scores.json"
echo ""
echo "[TODO] Step 4: aggregate per-category F1 scores"
echo "       Output format: {category: {questions, correct, f1}}"

echo ""
echo "[longmemeval] Harness stub complete. No scores produced."
echo "              See README.md for the full implementation plan."
