#!/usr/bin/env python3
"""Validate the Phase 2 reuse oracle label ledger.

Phase 1 ships no oracle gate. This stdlib-only validator fixes the arithmetic
before any Phase 2 model is tuned: 50 tune labels, 60 confirmation positives
(30 eligible + 15 D4 + 15 D3), 30 confirmation negatives, and 20 separate
holdout labels, 160 total.

CSV columns: pair_id,split,label,tier
  split: tune | confirmation | holdout
  label: duplicate | distinct
  tier:  eligible | d4 | d3 | negative | holdout

Optional `--predictions FILE` reads `pair_id,confidence` and reports
confirmation precision, recall, confusion counts, and the Wilson upper bound.
`--baseline FILE` adds McNemar discordant counts, chi-square, p-value, power,
and minimum detectable difference at `--alpha`/`--power`.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import sys
from collections import Counter
from pathlib import Path
from statistics import NormalDist
from typing import NamedTuple, Optional

EXPECTED_LEDGER = {
    "total": 160,
    "tune": 50,
    "confirmation": 90,
    "confirmation_duplicate": 60,
    "confirmation_distinct": 30,
    "eligible": 30,
    "d4": 15,
    "d3": 15,
    "negative": 30,
    "holdout": 20,
}


class EvalConfig(NamedTuple):
    threshold: float = 0.5
    alpha: float = 0.05
    target_power: float = 0.8

    def validate(self) -> None:
        if not 0.0 <= self.threshold <= 1.0:
            raise ValueError("threshold must be 0..1")
        if not 0.0 < self.alpha < 1.0:
            raise ValueError("alpha must be in (0, 1)")
        if not 0.0 < self.target_power < 1.0:
            raise ValueError("power must be in (0, 1)")


def read_rows(path: Path) -> list[dict]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def validate_row_tiers(rows: list[dict]) -> None:
    for row in rows:
        split_name = row["split"]
        if split_name == "confirmation":
            if row["tier"] in {"eligible", "d4", "d3"} and row["label"] != "duplicate":
                raise ValueError("confirmation tier rows must be duplicate")
            if row["tier"] == "negative" and row["label"] != "distinct":
                raise ValueError("confirmation negative rows must be distinct")
        elif split_name == "tune" and row["tier"] != "tune":
            raise ValueError("tune rows must use tier=tune")
        elif split_name == "holdout" and row["tier"] != "holdout":
            raise ValueError("holdout rows must use tier=holdout")


def validate(path: Path) -> dict:
    rows = read_rows(path)
    required = {"pair_id", "split", "label", "tier"}
    if not rows:
        raise ValueError("labels file is empty")
    missing = required.difference(rows[0])
    if missing:
        raise ValueError(f"missing columns: {sorted(missing)}")

    ids = {row["pair_id"] for row in rows}
    if len(ids) != len(rows):
        raise ValueError("pair_id values must be unique")

    validate_row_tiers(rows)
    split = Counter(row["split"] for row in rows)
    confirmation = [row for row in rows if row["split"] == "confirmation"]
    confirmation_labels = Counter(row["label"] for row in confirmation)
    confirmation_tiers = Counter(row["tier"] for row in confirmation)

    actual = {
        "total": len(rows),
        "tune": split["tune"],
        "confirmation": split["confirmation"],
        "confirmation_duplicate": confirmation_labels["duplicate"],
        "confirmation_distinct": confirmation_labels["distinct"],
        "eligible": confirmation_tiers["eligible"],
        "d4": confirmation_tiers["d4"],
        "d3": confirmation_tiers["d3"],
        "negative": confirmation_tiers["negative"],
        "holdout": split["holdout"],
    }
    mismatches = {
        k: {"expected": EXPECTED_LEDGER[k], "actual": actual[k]}
        for k in EXPECTED_LEDGER
        if EXPECTED_LEDGER[k] != actual[k]
    }
    if mismatches:
        raise ValueError(f"oracle ledger mismatch: {json.dumps(mismatches, sort_keys=True)}")
    if set(split) - {"tune", "confirmation", "holdout"}:
        raise ValueError(f"unknown split values: {sorted(set(split) - {'tune', 'confirmation', 'holdout'})}")
    tier_values = {row["tier"] for row in rows}
    if tier_values - {"eligible", "d4", "d3", "negative", "holdout", "tune"}:
        raise ValueError(
            f"unknown tier values: {sorted(tier_values - {'eligible', 'd4', 'd3', 'negative', 'holdout', 'tune'})}"
        )
    return {"labels": str(path), **actual}


def wilson_upper(successes: int, total: int, z: float = 1.96) -> float:
    if total == 0:
        return 1.0
    p = successes / total
    denom = 1 + z * z / total
    centre = p + z * z / (2 * total)
    margin = z * math.sqrt(p * (1 - p) / total + z * z / (4 * total * total))
    return min(1.0, (centre + margin) / denom)


def discordant_counts(
    confirmation: list[dict],
    predictions: dict[str, float],
    baseline: dict[str, float],
    threshold: float,
) -> tuple[int, int, int]:
    b = c = missing = 0
    for row in confirmation:
        current = predictions.get(row["pair_id"])
        reference = baseline.get(row["pair_id"])
        if current is None or reference is None:
            missing += 1
            continue
        actual = row["label"] == "duplicate"
        model_correct = (current >= threshold) == actual
        base_correct = (reference >= threshold) == actual
        if model_correct and not base_correct:
            b += 1
        elif base_correct and not model_correct:
            c += 1
    return b, c, missing


def mcnemar(
    confirmation: list[dict],
    predictions: dict[str, float],
    baseline: dict[str, float],
    config: EvalConfig,
) -> dict:
    normal = NormalDist()
    b, c, missing = discordant_counts(confirmation, predictions, baseline, config.threshold)
    discordant = b + c
    if discordant == 0:
        return {
            "b": b,
            "c": c,
            "discordant": 0,
            "missing": missing,
            "chi2": 0.0,
            "p_value": 1.0,
            "power": 0.0,
            "mdd": 1.0,
            "alpha": config.alpha,
            "target_power": config.target_power,
        }
    chi2 = (abs(b - c) - 1) ** 2 / discordant
    p_value = 2 * (1 - normal.cdf(math.sqrt(chi2)))
    delta = abs(b - c) / discordant
    z_alpha = normal.inv_cdf(1 - config.alpha / 2)
    z_power = normal.inv_cdf(config.target_power)
    power = normal.cdf(math.sqrt(discordant) * delta - z_alpha)
    mdd = min(1.0, (z_alpha + z_power) / math.sqrt(discordant))
    return {
        "b": b,
        "c": c,
        "discordant": discordant,
        "missing": missing,
        "chi2": round(chi2, 4),
        "p_value": round(p_value, 6),
        "power": round(power, 4),
        "mdd": round(mdd, 4),
        "alpha": config.alpha,
        "target_power": config.target_power,
    }


def confusion_counts(
    confirmation: list[dict],
    predictions: dict[str, float],
    threshold: float,
) -> tuple[int, int, int, int, int]:
    tp = fp = fn = tn = missing = 0
    for row in confirmation:
        confidence = predictions.get(row["pair_id"])
        if confidence is None:
            missing += 1
            continue
        predicted = confidence >= threshold
        actual = row["label"] == "duplicate"
        if predicted and actual:
            tp += 1
        elif predicted and not actual:
            fp += 1
        elif not predicted and actual:
            fn += 1
        else:
            tn += 1
    return tp, fp, fn, tn, missing


def evaluate(
    labels_path: Path,
    predictions_path: Path,
    threshold: float = 0.5,
    baseline_path: Optional[Path] = None,
    alpha: float = 0.05,
    target_power: float = 0.8,
    split_name: str = "confirmation",
    config: Optional[EvalConfig] = None,
) -> dict:
    cfg = config if config is not None else EvalConfig(threshold, alpha, target_power)
    labels = read_rows(labels_path)
    predictions = {
        row["pair_id"]: float(row["confidence"]) for row in read_rows(predictions_path)
    }
    confirmation = [row for row in labels if row["split"] == split_name]
    tp, fp, fn, tn, missing = confusion_counts(confirmation, predictions, cfg.threshold)
    precision = tp / (tp + fp) if tp + fp else 0.0
    recall = tp / (tp + fn) if tp + fn else 0.0
    result = {
        "labels": str(labels_path),
        "predictions": str(predictions_path),
        "threshold": cfg.threshold,
        "split": split_name,
        "confirmation_pairs": len(confirmation),
        "missing_predictions": missing,
        "tp": tp,
        "fp": fp,
        "fn": fn,
        "tn": tn,
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "precision_wilson_upper": round(wilson_upper(tp, tp + fp), 4),
    }
    if baseline_path is not None and split_name == "confirmation":
        baseline = {
            row["pair_id"]: float(row["confidence"]) for row in read_rows(baseline_path)
        }
        result["mcnemar"] = mcnemar(confirmation, predictions, baseline, cfg)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--labels", required=True, type=Path)
    parser.add_argument("--predictions", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--threshold", default=0.5, type=float)
    parser.add_argument("--split", default="confirmation", choices=["confirmation", "holdout"])
    parser.add_argument("--alpha", default=0.05, type=float)
    parser.add_argument("--power", dest="target_power", default=0.8, type=float)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        result = validate(args.labels)
        if args.baseline is not None and args.predictions is None:
            raise ValueError("--baseline requires --predictions")
        if args.predictions is not None:
            config = EvalConfig(args.threshold, args.alpha, args.target_power)
            config.validate()
            result = evaluate(
                args.labels,
                args.predictions,
                baseline_path=args.baseline,
                split_name=args.split,
                config=config,
            )
    except (OSError, ValueError) as exc:
        print(f"reuse_oracle: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True) if args.json else result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
