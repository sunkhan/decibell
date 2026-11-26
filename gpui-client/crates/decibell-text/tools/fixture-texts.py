#!/usr/bin/env python3
"""Prints the 10k-message fixture's message sources as a JSON array (the corpus example's
--fixture input). Same generator and seed as the dev stack's #general (tools/dev/fixture.py)."""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../../tools/dev"))
import fixture  # noqa: E402

print(json.dumps([m["content"] for m in fixture.generate() if m["content"]], ensure_ascii=False))
