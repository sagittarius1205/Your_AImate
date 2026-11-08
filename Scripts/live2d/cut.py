"""Master alpha, part-map cleanup and alignment numbers for the Live2D layer prototype.

    python cut.py <workdir>

Writes master_rgba.png, partmap_clean.png and prints how well the part map sits on the master.
"""
import sys
from pathlib import Path

import cv2
import numpy as np
from PIL import Image

MAGENTA = np.array([255, 0, 255], np.float32)
PALETTE = {
    "background": (255, 0, 255),
    "front_hair": (255, 0, 0),
    "back_hair": (0, 0, 255),
    "hair_clip": (255, 255, 0),
    "face": (255, 160, 64),
    "eyes": (0, 255, 0),
    "brows": (0, 128, 0),
    "mouth": (0, 255, 255),
    "torso": (128, 128, 128),
    "arm_image_left": (128, 0, 128),
    "arm_image_right": (255, 128, 0),
    "skirt": (128, 64, 0),
    "legs": (0, 0, 128),
}


def master_alpha(rgb: np.ndarray) -> np.ndarray:
    """Colour key on magenta, then un-mix the magenta out of the edge pixels."""
    f = rgb.astype(np.float32)
    d = np.linalg.norm(f - MAGENTA, axis=2)
    a = np.clip((d - 60.0) / (160.0 - 60.0), 0.0, 1.0)
    safe = np.maximum(a, 1e-3)[..., None]
    fg = np.clip((f - (1.0 - a[..., None]) * MAGENTA) / safe, 0, 255)
    # The generator anti-aliases outlines against the magenta, so the rim keeps a purple cast the
    # un-mixing above does not remove. Within 3 px of the edge, red and blue are pulled down to green
    # where both exceed it. Only the rim: lips and blush are pinkish on purpose.
    solid = (a > 0.5).astype(np.uint8)
    rim = (cv2.distanceTransform(solid, cv2.DIST_L2, 3) <= 3.0) & (a > 0)
    excess = np.clip(np.minimum(fg[..., 0], fg[..., 2]) - fg[..., 1], 0, None) * rim
    fg[..., 0] -= excess
    fg[..., 2] -= excess
    return np.dstack([fg, a * 255]).astype(np.uint8)


def clean_partmap(rgb: np.ndarray) -> np.ndarray:
    """Nearest palette colour per pixel, then a mode filter to remove speckle. Returns label ids."""
    pal = np.array(list(PALETTE.values()), np.float32)
    d = np.linalg.norm(rgb.astype(np.float32)[:, :, None, :] - pal[None, None], axis=3)
    labels = d.argmin(axis=2).astype(np.uint8)
    # 5x5 mode filter via per-label box counts.
    counts = np.stack([cv2.boxFilter((labels == i).astype(np.float32), -1, (5, 5)) for i in range(len(pal))])
    labels = counts.argmax(axis=0).astype(np.uint8)
    # Anti-aliasing between two flat colours lands on a third palette colour: a few-pixel speck of
    # "skirt" in the hair. Specks go to whatever surrounds them. Features that are small for real
    # (eyes, brows, mouth) are left alone.
    keep_small = {list(PALETTE).index(n) for n in ("eyes", "brows", "mouth")}
    stray = np.zeros(labels.shape, bool)
    for i in range(len(pal)):
        if i in keep_small:
            continue
        n, cc, stats, _ = cv2.connectedComponentsWithStats((labels == i).astype(np.uint8), 8)
        for c in range(1, n):
            if stats[c, cv2.CC_STAT_AREA] < 64:
                stray |= cc == c
    while stray.any():
        votes = np.stack([cv2.boxFilter(((labels == i) & ~stray).astype(np.float32), -1, (3, 3)) for i in range(len(pal))])
        best = votes.argmax(axis=0)
        fixed = stray & (votes.max(axis=0) > 0)
        labels[fixed] = best[fixed]
        stray &= ~fixed
    return labels


def main(work: Path) -> None:
    master = np.array(Image.open(work / "master.png").convert("RGB"))
    rgba = master_alpha(master)
    Image.fromarray(rgba).save(work / "master_rgba.png")
    sil_m = rgba[..., 3] > 127

    pm = Image.open(work / "partmap.png").convert("RGB")
    if pm.size != (master.shape[1], master.shape[0]):
        print(f"partmap size {pm.size} differs from master {master.shape[1::-1]}; resized for measuring")
        pm = pm.resize((master.shape[1], master.shape[0]), Image.NEAREST)
    labels = clean_partmap(np.array(pm))
    pal = np.array(list(PALETTE.values()), np.uint8)
    Image.fromarray(pal[labels]).save(work / "partmap_clean.png")
    sil_p = labels != 0

    inter, union = (sil_m & sil_p).sum(), (sil_m | sil_p).sum()
    print(f"silhouette IoU {inter / union:.4f}  master {sil_m.sum()} px, partmap {sil_p.sum()} px")

    edge_m = cv2.Canny(sil_m.astype(np.uint8) * 255, 50, 150) > 0
    edge_p = cv2.Canny(sil_p.astype(np.uint8) * 255, 50, 150) > 0
    dist_to_m = cv2.distanceTransform((~edge_m).astype(np.uint8), cv2.DIST_L2, 5)
    d = dist_to_m[edge_p]
    print(f"partmap edge to master edge: mean {d.mean():.2f} px, median {np.median(d):.1f}, p95 {np.percentile(d, 95):.1f}, max {d.max():.1f}")

    for i, name in enumerate(PALETTE):
        n = int((labels == i).sum())
        print(f"  {name:16s} {n:8d} px")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
