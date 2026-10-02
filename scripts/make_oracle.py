"""Writes tests/oracle.json: expected statistics computed with NumPy and SciPy.

    PYTHONPATH=<dir with scipy and numpy> python scripts/make_oracle.py

The Rust tests (tests/oracle.rs) compare benchlab's own implementation against these values.
"""
import json
import random
from pathlib import Path

import numpy as np
from scipy import stats

rng = random.Random(2026)


def lognormalish(n, mu, spread):
    return [round(rng.lognormvariate(mu, spread), 3) for _ in range(n)]


cases = []
datasets = {
    "float_a": lognormalish(40, 4.6, 0.05),
    "float_b": lognormalish(35, 4.65, 0.05),
    "ties_a": [rng.randint(100, 110) for _ in range(30)],
    "ties_b": [rng.randint(103, 113) for _ in range(25)],
    "same_a": lognormalish(50, 5.0, 0.03),
    "same_b": lognormalish(50, 5.0, 0.03),
    "small_a": [10.1, 10.4, 9.9, 10.2, 10.0],
    "small_b": [10.9, 11.2, 10.7, 11.0, 11.4, 10.8],
    "spiky": lognormalish(60, 4.6, 0.02) + [400.0, 520.0, 20.0],
}

summaries = {}
for name, data in datasets.items():
    a = np.array(data)
    q1, q3 = np.percentile(a, [25, 75])
    iqr = q3 - q1
    fences = (q1 - 3 * iqr, q1 - 1.5 * iqr, q3 + 1.5 * iqr, q3 + 3 * iqr)
    summaries[name] = {
        "data": data,
        "mean": float(a.mean()),
        "stddev": float(a.std(ddof=1)),
        "mad": float(stats.median_abs_deviation(a, scale=1.0)),
        "percentiles": {str(p): float(np.percentile(a, p)) for p in (1, 5, 25, 50, 75, 95, 99)},
        "outliers": {
            "low_severe": int((a < fences[0]).sum()),
            "low_mild": int(((a >= fences[0]) & (a < fences[1])).sum()),
            "high_mild": int(((a > fences[2]) & (a <= fences[3])).sum()),
            "high_severe": int((a > fences[3]).sum()),
        },
    }

pairs = []
for x, y in [("float_a", "float_b"), ("ties_a", "ties_b"), ("same_a", "same_b"), ("small_a", "small_b"), ("float_b", "float_a")]:
    r = stats.mannwhitneyu(datasets[x], datasets[y], use_continuity=True, alternative="two-sided", method="asymptotic")
    pairs.append({"x": x, "y": y, "u": float(r.statistic), "p": float(r.pvalue)})

Path(__file__).resolve().parent.parent.joinpath("tests", "oracle.json").write_text(
    json.dumps({"scipy": __import__("scipy").__version__, "numpy": np.__version__, "summaries": summaries, "mann_whitney": pairs}, indent=1)
)
print("wrote tests/oracle.json with scipy", __import__("scipy").__version__)
