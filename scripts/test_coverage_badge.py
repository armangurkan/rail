import json
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

from coverage_badge import line_totals, render_badge


class CoverageBadgeTests(unittest.TestCase):
    def test_totals_are_weighted_by_lines_across_files(self):
        lcov = "SF:small.rs\nLF:2\nLH:1\nend_of_record\nSF:large.rs\nLF:100\nLH:99\nend_of_record\n"
        self.assertEqual(line_totals(lcov), (100, 102))
        root = ET.fromstring(render_badge(*line_totals(lcov)))
        self.assertEqual(root.attrib["aria-label"], "coverage: 98.0%")

    def test_invalid_or_incomplete_reports_are_rejected(self):
        for lcov in [
            "",
            "SF:empty.rs\nLF:0\nLH:0\nend_of_record",
            "LF:2\nLH:3",
            "LF:-1\nLH:0",
            "LF:1",
            "LF:one\nLH:1",
            "LF:1\nLH:1\nLH:1",
        ]:
            with self.subTest(lcov=lcov), self.assertRaises(ValueError):
                line_totals(lcov)

    def test_zero_and_full_coverage_are_valid_and_have_distinct_colours(self):
        empty = ET.fromstring(render_badge(0, 10))
        full = ET.fromstring(render_badge(10, 10))
        self.assertEqual(empty.attrib["aria-label"], "coverage: 0.0%")
        self.assertEqual(full.attrib["aria-label"], "coverage: 100.0%")
        self.assertNotEqual(ET.tostring(empty), ET.tostring(full))
        self.assertIn("#e05d44", ET.tostring(empty).decode())
        self.assertIn("#4c1", ET.tostring(full).decode())

    def test_cli_writes_svg_and_auditable_provenance(self):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp)
            report = folder / "lcov.info"
            report.write_text("SF:test.rs\nLF:10\nLH:9\nend_of_record\n")
            subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("coverage_badge.py")),
                    str(report),
                    "--output-dir",
                    str(folder),
                    "--commit",
                    "a" * 40,
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            metadata = json.loads((folder / "coverage.json").read_text())
            self.assertEqual(
                metadata,
                {"covered": 9, "total": 10, "percent": 90.0, "commit": "a" * 40},
            )
            root = ET.parse(folder / "coverage.svg").getroot()
            self.assertEqual(root.attrib["aria-label"], "coverage: 90.0%")


if __name__ == "__main__":
    unittest.main()
