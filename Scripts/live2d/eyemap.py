"""An eye map drawn on a close-up, for faces whose eyes the part map is too coarse to split.

    python eyemap.py crop <workdir>    writes eyes_crop.png, a close-up for brief_eyemap.md
    python eyemap.py paste <workdir>   reads eyes_map.png, writes eyemap.png for layers.py

The eyes are about 40 px wide on the 1024 px master, so a part map drawn on the whole picture is off
by a large share of an eye, and colour alone cannot tell a shaded white from skin. The close-up is
the eyes scaled up about eight times; whatever the map is off by there shrinks by as much.
"""
import json
import sys
from pathlib import Path

import cv2
import numpy as np
from PIL import Image

from cut import PALETTE, clean_partmap

# What brief_eyemap.md asks for.
EYE_PALETTE = {"other": (255, 0, 255), "white": (0, 255, 255), "iris": (0, 0, 255), "lash": (0, 0, 0)}
SIZE = (1536, 1024)


def crop(work: Path) -> None:
    labels = clean_partmap(np.array(Image.open(work / "partmap.png").convert("RGB")))
    ys, xs = np.nonzero(labels == list(PALETTE).index("eyes"))
    cx, cy = (xs.min() + xs.max()) / 2, (ys.min() + ys.max()) / 2
    # Both eyes with room around them, in the close-up's 3:2.
    w = int(round((xs.max() - xs.min()) * 1.5))
    h = w * SIZE[1] // SIZE[0]
    box = [int(round(cx - w / 2)), int(round(cy - h / 2)), w, h]
    master = Image.open(work / "master.png").convert("RGB")
    x, y = box[:2]
    master.crop((x, y, x + w, y + h)).resize(SIZE, Image.LANCZOS).save(work / "eyes_crop.png")
    (work / "eyes_crop.json").write_text(json.dumps({"box": box}) + "\n", encoding="utf-8")
    print(f"eyes_crop.png: {w}x{h} at {x},{y} scaled {SIZE[0] / w:.2f}x")


def paste(work: Path) -> None:
    x, y, w, h = json.loads((work / "eyes_crop.json").read_text(encoding="utf-8"))["box"]
    big = np.array(Image.open(work / "eyes_map.png").convert("RGB").resize(SIZE, Image.NEAREST), np.float32)
    pal = np.array(list(EYE_PALETTE.values()), np.float32)
    labels = np.linalg.norm(big[:, :, None] - pal[None, None], axis=3).argmin(axis=2)
    # Each master pixel takes the label that covers most of it.
    share = np.stack([cv2.resize((labels == i).astype(np.float32), (w, h), interpolation=cv2.INTER_AREA) for i in range(len(pal))])
    small = share.argmax(axis=0)
    master = Image.open(work / "master.png")
    out = np.zeros((master.height, master.width, 3), np.uint8)
    out[:] = EYE_PALETTE["other"]
    out[y : y + h, x : x + w] = pal[small].astype(np.uint8)
    Image.fromarray(out).save(work / "eyemap.png")
    for i, name in enumerate(EYE_PALETTE):
        print(f"  {name:6s} {int((small == i).sum()):6d} px")


if __name__ == "__main__":
    {"crop": crop, "paste": paste}[sys.argv[1]](Path(sys.argv[2]))
