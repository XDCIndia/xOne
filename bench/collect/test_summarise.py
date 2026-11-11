"""Check: python3 -I bench/collect/test_summarise.py"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from summarise import summarise
s = summarise(range(1, 1001))
assert s == {"n": 1000, "p50": 500, "p95": 950, "p99": 990, "p99.9": 999, "max": 1000}, s
assert summarise([7])["p99.9"] == 7
print("ok")
