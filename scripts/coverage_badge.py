#!/usr/bin/env python3
"""Generate the README badge from llvm-cov's workspace LCOV line totals."""

import argparse
import json
from pathlib import Path


def line_totals(text):
    covered = total = 0
    for record in text.split("end_of_record"):
        if not record.strip():
            continue
        fields = {}
        for line in record.splitlines():
            key, _, value = line.partition(":")
            if key in ("LF", "LH"):
                if key in fields:
                    raise ValueError(f"duplicate {key} counter")
                fields[key] = int(value)
        if fields.keys() != {"LF", "LH"}:
            raise ValueError("each LCOV record must contain LF and LH")
        if not 0 <= fields["LH"] <= fields["LF"]:
            raise ValueError("invalid covered/total line counts")
        covered += fields["LH"]
        total += fields["LF"]
    if total == 0:
        raise ValueError("the report contains no executable lines")
    return covered, total


def render_badge(covered, total):
    percent = covered * 100 / total
    label = f"coverage: {percent:.1f}%"
    colour = "#4c1" if percent >= 80 else "#e05d44"
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="113" height="20" role="img" aria-label="{label}">
  <title>{label}</title>
  <clipPath id="corners"><rect width="113" height="20" rx="3"/></clipPath>
  <g clip-path="url(#corners)">
    <path fill="#555" d="M0 0h63v20H0z"/>
    <path fill="{colour}" d="M63 0h50v20H63z"/>
  </g>
  <g fill="#fff" text-anchor="middle" font-family="Verdana,DejaVu Sans,sans-serif" font-size="11">
    <text x="31.5" y="14">coverage</text>
    <text x="88" y="14">{percent:.1f}%</text>
  </g>
</svg>
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    covered, total = line_totals(args.report.read_text())
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "coverage.svg").write_text(render_badge(covered, total))
    metadata = {
        "covered": covered,
        "total": total,
        "percent": round(covered * 100 / total, 2),
        "commit": args.commit,
    }
    (args.output_dir / "coverage.json").write_text(
        json.dumps(metadata, indent=2) + "\n"
    )
    print(f"Line coverage: {covered}/{total} ({covered * 100 / total:.1f}%)")


if __name__ == "__main__":
    main()
