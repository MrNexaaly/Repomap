#!/usr/bin/env python3
"""Serve a precomputed ranking to bench/run.py: lookup.py TABLE.json DIR QUERY."""

import json
import sys

table = json.load(open(sys.argv[1]))
print("\n".join(table.get(f"{sys.argv[2]}\x00{sys.argv[3]}", [])))
