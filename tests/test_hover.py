from dataclasses import replace
import unittest
from clash_monitor.storage import HourTraffic
from clash_monitor.ui import hover_text


class HoverTests(unittest.TestCase):
    def test_quality_states_and_units(self):
        row = HourTraffic(3)
        self.assertIn("未采集", hover_text(row, "日期"))
        self.assertIn("尚未开始", hover_text(row, "日期", future=True))
        self.assertIn("采样基线", hover_text(replace(row, samples=1), "日期"))
        self.assertIn("代理流量 0 B", hover_text(replace(row, covered_seconds=1), "日期"))
        text = hover_text(replace(row, uploaded=2, downloaded=1024, covered_seconds=1,
                                 legacy_coverage=True, gap_seconds=60), "完整日期")
        for expected in ("完整日期", "下载 1.00 KiB", "上传 2 B", "精确覆盖未知", "缺口"):
            self.assertIn(expected, text)
