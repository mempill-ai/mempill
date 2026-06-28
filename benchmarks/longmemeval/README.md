# LongMemEval / LOCOMO Correctness Harness — Plan (NOT YET RUN)

> **Status: SKELETON — no scores have been produced.**
> This document describes the plan, the dataset, the mempill subset under test,
> and the harness shape. No benchmark run has been executed. Do not cite any numbers
> from this file as results.

## Which benchmark

**LongMemEval** — "Evaluating Long-Context Memory of Chat Assistants" (2024).
Paper: https://arxiv.org/abs/2410.10813  
Dataset: https://huggingface.co/datasets/xiaowu0162/longmemeval

LongMemEval provides ~500 QA pairs across five categories derived from multi-session
chat histories. It measures whether a memory system surfaces the correct fact at
query time, including facts that have been updated, contradicted, or time-bounded.

**Why LongMemEval over LOCOMO:**
LOCOMO (2024) is a full conversation dataset with temporal ordering but its
evaluation requires LLM-as-judge scoring, adding non-determinism. LongMemEval
provides gold answers that can be matched without an LLM judge, making it more
tractable for a deterministic correctness harness.

## mempill subset under test

The five LongMemEval question categories and their relevance to mempill:

| Category | Relevance to mempill | mempill feature exercised |
|---|---|---|
| Single-session QA | Baseline fact retrieval | `query_memory` (no time-travel) |
| Multi-session QA | Fact persistence across sessions | `agent_id` isolation + persistent store |
| **Temporal reasoning** | "What was X at time T?" | `valid_at` (independent valid-time axis) |
| **Knowledge update** | "X changed from A to B — what is X now?" | Succession fold, `Disposition::CommittedCheap` |
| Adversarial / negation | "X is NOT Y" | Contested / Superseded disposition surfacing |

The **temporal reasoning** and **knowledge update** categories directly exercise
mempill's bi-temporal query capabilities.

## Planned harness shape

```
benchmarks/longmemeval/
  README.md          ← this file
  run_harness.sh     ← stub (see below); TODOs marked clearly
  src/
    load_dataset.py  ← stub: download + parse LongMemEval JSONL
    ingest_session.py ← stub: map chat turns to mempill IngestClaimRequest
    evaluate.py      ← stub: run queries, score against gold answers
```

## How the harness would work (plan)

1. **Dataset download** — `load_dataset.py` downloads the LongMemEval dataset from
   HuggingFace and converts each session's turns into a flat list of
   `(subject, predicate, value, valid_time, session_index)` tuples.

2. **Ingest** — `ingest_session.py` calls the mempill MCP server (or directly invokes
   the Python binding) to ingest each extracted fact as an `IngestClaimRequest`.
   Session index maps to `tx_time` to preserve temporal ordering on the
   transaction-time axis.

3. **Query** — for each LongMemEval QA pair:
   - Extract the `valid_at` instant from the question (if temporal category).
   - Call `query_memory(valid_at=T, as_of_tx_time=None)` to exercise the valid-time axis.
   - For knowledge-update questions, call `query_memory(valid_at=None)` to test
     succession correctness.

4. **Score** — compare `belief.primary.fact.value` against the gold answer.
   Exact-match F1 is the primary metric (matching LongMemEval paper methodology).
   Track per-category accuracy separately to isolate the temporal/update categories.

## Stub script

See `run_harness.sh` for the entry point with clear TODOs.

## Expected output format (when run)

```
Category               | Questions | Correct | F1
-----------------------|-----------|---------|-----
single_session         |       150 |     ???  | ???
multi_session          |       100 |     ???  | ???
temporal_reasoning     |       100 |     ???  | ???
knowledge_update       |       100 |     ???  | ???
adversarial            |        50 |     ???  | ???
-----------------------|-----------|---------|-----
TOTAL                  |       500 |     ???  | ???
```

## Prerequisites for running

- LongMemEval dataset access (HuggingFace token or local copy)
- mempill Python binding (`pip install mempill`) or local build
- An LLM for question-to-structured-fact extraction (GPT-4o or similar)
- Estimated runtime: 2–4 hours for the full 500-question eval

## Why LongMemEval

LongMemEval's temporal-reasoning and knowledge-update categories directly exercise
mempill's bi-temporal query capabilities (`valid_at` axis for valid-time point queries,
succession fold for knowledge-update questions). The gold-answer format allows
deterministic exact-match scoring without an LLM judge, making it a tractable
correctness harness.

**This harness is the vehicle for empirical validation of those capabilities.**
