"""Copies one character's open-eye layers into another character drawn from the same face.

    python reuse_eyes.py <from character dir> <to character dir> <dy>

layers.py splits an open eye into white, iris and lashes by colour thresholds that were set on the
first master. An outfit made by editing that master keeps the face, but the redraw shifts the eye
colours enough that the split can go wrong: on the gown (2026-11-08) one eye's white came out a
sliver, and the renderer clips the iris to the white, so the iris was cut even at rest. The eyes
of the two pictures are the same drawing within a couple of pixels, so the clean split is reused,
moved by `dy` so the irises line up (measured from both runs' fitted iris centres).
"""
import json
import shutil
import sys
from pathlib import Path

PARTS = [f"{kind}_image_{side}" for side in ("left", "right") for kind in ("eye_white", "iris", "eye_lash")]


def main(src: Path, dst: Path, dy: int) -> None:
    have = {l["name"]: l for l in json.loads((src / "layers.json").read_text(encoding="utf-8"))["layers"]}
    path = dst / "layers.json"
    art = json.loads(path.read_text(encoding="utf-8"))
    for entry in art["layers"]:
        if entry["name"] in PARTS:
            given = have[entry["name"]]
            shutil.copyfile(src / given["file"], dst / entry["file"])
            entry.update({k: given[k] for k in ("x", "w", "h")}, y=given["y"] + dy)
            print(f"{entry['name']}: from {src.name}, at {entry['x']},{entry['y']}")
    path.write_text(json.dumps(art, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3]))
