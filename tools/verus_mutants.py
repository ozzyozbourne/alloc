#!/usr/bin/env python3
"""Plants one bug at a time in 09-verus-handle-map and checks that Verus
rejects every one. Prints a table; exits non-zero if any bug is accepted.

    VERUS=/path/to/verus VERUS_Z3_PATH=/path/to/z3 python3 tools/verus_mutants.py

Each run takes a few seconds and about 350 MB; they run one after another.
"""
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "09-verus-handle-map" / "src"
VERUS = os.environ.get("VERUS", "verus")

# Edits to src/handle_map.rs that keep retired slots in use, as slotmap and Odin do.
NO_RETIRE = (
    "if self.slots[i].generation != MAX_GEN {\n            self.free_head",
    "if true {\n            self.free_head",
)

# (name, [(find, replacement), ...]) applied to src/handle_map.rs.
# Each text to find must occur exactly once.
MUTANTS = [
    (
        "wrap generations like slotmap/Odin",
        [
            (
                "let generation = self.slots[i].generation + 1;",
                "let generation = self.slots[i].generation.wrapping_add(1);",
            ),
            NO_RETIRE,
        ],
    ),
    ("put retired slots back on the free list", [NO_RETIRE]),
    (
        "add does not bump the generation",
        [("let generation = self.slots[i].generation + 1;", "let generation = self.slots[i].generation;")],
    ),
    ("add does not advance free_head", [("self.free_head = next;", "")]),
    ("remove leaks the slot (not pushed on the free list)", [("self.free_head = h.idx;", "")]),
    ("remove does not decrement len", [("self.len = self.len - 1;", "")]),
    (
        "get ignores the generation",
        [
            (
                "if i < self.slots.len() && self.slots[i].generation == h.generation {",
                "if i < self.slots.len() {",
            )
        ],
    ),
    (
        "contains ignores occupancy",
        [
            (
                "self.slots[i].value.is_some() && self.slots[i].generation\n            == h.generation\n    }",
                "self.slots[i].generation\n            == h.generation\n    }",
            )
        ],
    ),
    (
        "new_in builds a cyclic free list",
        [
            (
                "(j + 1) as u32\n                        } else {\n                            NONE",
                "(j + 1) as u32\n                        } else {\n                            0",
            )
        ],
    ),
]


def verify(src_dir):
    r = subprocess.run(
        [VERUS, str(src_dir / "main.rs")], capture_output=True, text=True, timeout=300
    )
    out = r.stdout + r.stderr
    m = re.search(r"verification results:: (\d+) verified, (\d+) errors", out)
    if m:
        why = re.search(r"^error: (.*)\n\s*--> \S*?([a-z_]+\.rs:\d+)", out, re.M)
        why = f"  ({why.group(1)}, {why.group(2)})" if why else ""
        return f"{m.group(1)} verified, {m.group(2)} errors{why}", int(m.group(2)) > 0
    first = next((l for l in out.splitlines() if l.startswith("error")), "error")
    return first[:60], r.returncode != 0


def main():
    print(f"{'planted bug':55} {'caught':6} verus", flush=True)
    accepted = 0
    for name, edits in MUTANTS:
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            for f in SRC.glob("*.rs"):
                shutil.copy(f, tmp)
            text = (tmp / "handle_map.rs").read_text()
            for find, repl in edits:
                assert text.count(find) == 1, f"{name!r}: pattern found {text.count(find)} times"
                text = text.replace(find, repl)
            (tmp / "handle_map.rs").write_text(text)
            result, caught = verify(tmp)
        accepted += not caught
        print(f"{name:55} {'yes' if caught else 'NO':6} {result}", flush=True)
    sys.exit(1 if accepted else 0)


if __name__ == "__main__":
    main()
