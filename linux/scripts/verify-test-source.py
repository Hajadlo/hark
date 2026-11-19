#!/usr/bin/env python3
"""Verify synthetic routing before playing a test clip; reads graph metadata only."""
import json
import sys

objects = json.load(sys.stdin)
nodes = {o["id"]: o.get("info", {}).get("props", {}) for o in objects}
source = next((i for i, p in nodes.items() if p.get("node.name") == "hark-linux-source"), None)
capture = next(
    (i for i, p in nodes.items() if p.get("node.name") == "alsa_capture.hark" and p.get("media.class") == "Stream/Input/Audio"), None
)
assert source is not None and capture is not None, "synthetic source or hark capture missing"
links = [o.get("info", {}).get("props", {}) for o in objects if "Link" in o["type"]]
incoming = [p for p in links if p.get("link.input.node") == capture]
assert incoming and all(p.get("link.output.node") == source for p in incoming), "capture is not exclusively linked to the synthetic source"
print("Synthetic routing verified: hark-linux-source → alsa_capture.hark")
