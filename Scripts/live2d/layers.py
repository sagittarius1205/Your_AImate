"""Builds the Live2D layer PSD from the master, its part map and three fill images.

    python layers.py <workdir> [<layer dir for the pet>]

Visible pixels always come from master.png, so the layers stack back into the master exactly.
The fill images (nofront.png, backhair.png, face_states.png) only supply what is hidden or what
changes: the forehead under the bangs, the back hair behind the head, closed eyes, an open mouth.
The white under the iris and the iris under the lashes are painted in here, from the master's own
white and iris.
"""
import json
import sys
from pathlib import Path

import cv2
import numpy as np
from PIL import Image
from scipy.optimize import least_squares
from psd_tools import PSDImage
from psd_tools.api.layers import PixelLayer

from cut import PALETTE, clean_partmap, master_alpha

ID = {name: i for i, name in enumerate(PALETTE)}

# A layer drawn only where an earlier one is opaque, as the pet's `clip` draws it (rig.json).
CLIP = {"iris_image_left": "eye_white_image_left", "iris_image_right": "eye_white_image_right"}


def load_rgba(path: Path) -> np.ndarray:
    return master_alpha(np.array(Image.open(path).convert("RGB")))


def register(fill: np.ndarray, master: np.ndarray, keep: np.ndarray) -> tuple[np.ndarray, tuple[float, float]]:
    """Shift `fill` onto `master` using only pixels that the edit was told to leave alone."""
    g = lambda im: cv2.cvtColor(im[..., :3], cv2.COLOR_RGB2GRAY).astype(np.float32) * keep
    (dx, dy), _ = cv2.phaseCorrelate(g(master), g(fill))
    m = np.float32([[1, 0, -dx], [0, 1, -dy]])
    out = cv2.warpAffine(fill, m, fill.shape[1::-1], flags=cv2.INTER_LINEAR, borderValue=(0, 0, 0, 0))
    return out, (dx, dy)


def nearest_label(region: np.ndarray, labels: np.ndarray, candidates: list[str]) -> np.ndarray:
    """For each pixel in `region`, the candidate part whose visible pixels are closest."""
    dists = []
    for name in candidates:
        src = (labels != ID[name]).astype(np.uint8)
        dists.append(cv2.distanceTransform(src, cv2.DIST_L2, 5))
    pick = np.argmin(np.stack(dists), axis=0)
    out = np.full(labels.shape, 255, np.uint8)
    for k, name in enumerate(candidates):
        out[region & (pick == k)] = ID[name]
    return out


def layer(rgb_src: np.ndarray, mask: np.ndarray) -> np.ndarray:
    out = rgb_src.copy()
    out[..., 3] = (out[..., 3].astype(np.float32) * mask).astype(np.uint8)
    return out


def split_lr(mask: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Image-left and image-right halves of a two-blob part, split at the face's vertical midline."""
    xs = np.nonzero(mask.any(axis=0))[0]
    mid = (xs.min() + xs.max()) // 2 if len(xs) else mask.shape[1] // 2
    left, right = mask.copy(), mask.copy()
    left[:, mid:] = False
    right[:, :mid] = False
    return left, right


def eye_colours(master: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """The white of the eye (pale, unsaturated) and the bright orange of the iris."""
    hsv = cv2.cvtColor(np.ascontiguousarray(master[..., :3]), cv2.COLOR_RGB2HSV_FULL).astype(int)
    h, s, v = hsv[..., 0], hsv[..., 1], hsv[..., 2]
    return (s < 70) & (v > 150), (h >= 10) & (h <= 42) & (s > 110) & (v > 140)


def components(mask: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    _, cc, stats, _ = cv2.connectedComponentsWithStats(mask.astype(np.uint8), 8)
    return cc, stats[:, cv2.CC_STAT_AREA]


def sclera_of(master: np.ndarray, eye: np.ndarray) -> np.ndarray:
    # 15 px keeps the white either side of the iris and drops the highlight in it.
    cc, area = components(eye & eye_colours(master)[0])
    return np.isin(cc, [i for i in range(1, len(area)) if area[i] >= 15])


def iris_discs(master: np.ndarray, eyes: list[np.ndarray]) -> list[tuple[float, float, float, float]]:
    """Each iris as an upright ellipse (cx, cy, rx, ry), fitted to where it meets the white.

    Its top is under the lashes, so only the sides (rows with white on both sides) and the bottom of
    the bright orange are seen. Both eyes share one pair of radii: fitted alone, the right eye's
    shorter arc gives an iris a fifth taller than the left one's.
    """
    orange = eye_colours(master)[1]
    sets = []
    for eye in eyes:
        sclera = sclera_of(master, eye)
        cc, area = components(eye & orange)
        core = cc == 1 + int(np.argmax(area[1:]))
        cols = np.nonzero(core.any(axis=0))[0]
        mid = (cols.min() + cols.max()) // 2
        pts = []
        for y in np.nonzero(core.any(axis=1))[0]:
            left, right = np.nonzero(sclera[y, :mid])[0], np.nonzero(sclera[y, mid:])[0]
            if len(left) and len(right):
                pts += [(left.max() + 1, y), (mid + right.min() - 1, y)]
        # The dark rim under the orange is about a pixel and a half.
        third = (cols.max() - cols.min()) // 3
        pts += [(x, np.nonzero(core[:, x])[0].max() + 1.5) for x in range(cols.min() + third, cols.max() - third + 1)]
        sets.append(np.array(pts, np.float64))

    def residual(p: np.ndarray) -> np.ndarray:
        rx, ry = p[:2]
        return np.concatenate([
            (np.hypot((q[:, 0] - p[2 + 2 * k]) / rx, (q[:, 1] - p[3 + 2 * k]) / ry) - 1) * min(rx, ry)
            for k, q in enumerate(sets)
        ])

    start = [11.0, 14.0] + [c for q in sets for c in (q[:, 0].mean(), q[:, 1].mean())]
    p = least_squares(residual, start).x
    return [(p[2 + 2 * k], p[3 + 2 * k], p[0], p[1]) for k in range(len(eyes))]


def painted_in(img: np.ndarray, known: np.ndarray, region: np.ndarray, pad: int = 6) -> np.ndarray:
    """`img` with `region` painted from the `known` pixels alone, so nothing else bleeds in."""
    ys, xs = np.nonzero(region | known)
    y0, y1, x0, x1 = max(ys.min() - pad, 0), ys.max() + pad + 1, max(xs.min() - pad, 0), xs.max() + pad + 1
    unknown = (~known[y0:y1, x0:x1]).astype(np.uint8) * 255
    painted = cv2.inpaint(np.ascontiguousarray(img[y0:y1, x0:x1, :3]), unknown, 4, cv2.INPAINT_TELEA)
    out = img.copy()
    r = region[y0:y1, x0:x1]
    out[y0:y1, x0:x1][r, :3] = painted[r]
    return out


def eye_split(master: np.ndarray, eye: np.ndarray, disc: tuple[float, float, float, float]) -> list[np.ndarray]:
    """One open eye as its white, its iris and its lashes, which stack back into the master.

    The white is the opening, with the iris painted out of it in the white's own colours, lid
    shadow included. The iris is the whole disc, the part under the lashes painted from the part
    that shows, so it can move under them. The lashes are the rest of the eye, and stay put.
    """
    cx, cy, rx, ry = disc
    sclera = sclera_of(master, eye)
    yy, xx = np.mgrid[: eye.shape[0], : eye.shape[1]]
    # A pixel past the fitted edge, so the rim's anti-aliasing goes with the iris and leaves no
    # ring behind in the white.
    grow = 1.7
    disc_px = np.hypot((xx - cx) / (rx + grow), (yy - cy) / (ry + grow)) <= 1
    # Where the lashes end: the top of the white in each column, and across the iris, where there
    # is no white, a parabola through the 8 columns either side. One pixel lower over the iris, so
    # the lashes' soft lower edge stays with the lashes and does not ride down on the iris.
    tops = {int(x): int(np.nonzero(sclera[:, x])[0].min()) for x in np.nonzero(sclera.any(axis=0))[0]}
    x0, x1 = int(np.floor(cx - rx - grow)), int(np.ceil(cx + rx + grow))
    near = [x for x in tops if x0 - 8 <= x < x0 or x1 < x <= x1 + 8]
    curve = np.polyval(np.polyfit(near, [tops[x] for x in near], 2), np.arange(x0, x1 + 1))
    top = np.full(eye.shape[1], eye.shape[0])
    for x, t in tops.items():
        top[x] = t
    top[x0 : x1 + 1] = np.minimum(top[x0 : x1 + 1], np.round(curve).astype(int) + 1)
    seen = disc_px & eye & (yy >= top[None, :])
    # Everything the white and the seen iris enclose is the opening.
    cnts, _ = cv2.findContours((sclera | seen).astype(np.uint8), cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_NONE)
    opening = (cv2.drawContours(np.zeros(eye.shape, np.uint8), cnts, -1, 1, -1) > 0) & eye

    clean = sclera & ~(cv2.dilate(disc_px.astype(np.uint8), np.ones((3, 3), np.uint8)) > 0)
    white = painted_in(master, clean, opening & disc_px)
    white[..., 3] = np.where(opening, master[..., 3], 0)
    iris = painted_in(master, seen, disc_px & ~seen)
    iris[..., 3] = np.where(disc_px, 255, 0)
    return [white, iris, layer(master, (eye & ~opening).astype(np.float32))]


def main(work: Path) -> None:
    master = load_rgba(work / "master.png")
    labels = clean_partmap(np.array(Image.open(work / "partmap.png").convert("RGB").resize(master.shape[1::-1], Image.NEAREST)))
    is_ = lambda *names: np.isin(labels, [ID[n] for n in names])
    report = []

    # What each edit was told not to touch, used to measure and undo any drift.
    lower = is_("skirt", "legs")
    nofront, s1 = register(load_rgba(work / "nofront.png"), master, lower)
    backhair, s2 = register(load_rgba(work / "backhair.png"), master, is_("back_hair") & (master[..., 3] > 200))
    states, s3 = register(load_rgba(work / "face_states.png"), master, lower | is_("torso"))
    report += [f"shift nofront {s1[0]:+.2f},{s1[1]:+.2f}  backhair {s2[0]:+.2f},{s2[1]:+.2f}  face_states {s3[0]:+.2f},{s3[1]:+.2f} px"]

    # Under the bangs and the clip: give each hidden pixel to the nearest part, fill it from nofront.
    hidden = is_("front_hair", "hair_clip") & (nofront[..., 3] > 127)
    owner = nearest_label(hidden, labels, ["face", "torso", "arm_image_left", "arm_image_right", "back_hair"])
    # Nearness alone hands the top of the head to the face. What is under the bangs there is hair,
    # and the back-hair layer already has it from backhair.png, so hair-coloured pixels leave the face.
    med = lambda m: np.median(master[m & (master[..., 3] > 200), :3].astype(np.float32), axis=0)
    skin, hair = med(is_("face")), med(is_("back_hair"))
    px = nofront[..., :3].astype(np.float32)
    hairish = np.linalg.norm(px - hair, axis=2) < np.linalg.norm(px - skin, axis=2)
    owner[(owner == ID["face"]) & hairish] = ID["back_hair"]

    def part(name: str, extra: np.ndarray | None = None, fill: np.ndarray | None = None) -> np.ndarray:
        vis = is_(name)
        img = master.copy()
        if extra is not None and fill is not None:
            img[extra & ~vis] = fill[extra & ~vis]
            vis = vis | extra
        return layer(img, vis.astype(np.float32))

    out: list[tuple[str, np.ndarray]] = []

    # Back hair: visible from master, the rest of the mass from backhair.png.
    bh_extra = (backhair[..., 3] > 127) & ~is_("back_hair")
    out.append(("back_hair", part("back_hair", bh_extra, backhair)))
    # Each leg on its own, so the pet can turn each from its hip. The two are apart all the way up to
    # the skirt, so the midline split parts them.
    leg_l, leg_r = split_lr(is_("legs"))
    out += [("leg_image_left", part_mask(master, leg_l)), ("leg_image_right", part_mask(master, leg_r))]
    out.append(("skirt", part("skirt")))
    out.append(("torso", part("torso", owner == ID["torso"], nofront)))
    out.append(("arm_image_left", part("arm_image_left", owner == ID["arm_image_left"], nofront)))
    out.append(("arm_image_right", part("arm_image_right", owner == ID["arm_image_right"], nofront)))

    # Face skin: forehead from nofront, and skin painted in under the eyes, brows and mouth.
    face_img = master.copy()
    face_mask = is_("face") | (owner == ID["face"])
    face_img[owner == ID["face"]] = nofront[owner == ID["face"]]
    # Thin strands drawn across the forehead are labelled face in the part map and are too faint to
    # tell from skin by colour. nofront drew the same face with no hair on it, so the skin comes
    # from there; eyes, brows and mouth are layers of their own and come from the master.
    skin_vis = is_("face") & (master[..., 3] > 200) & (nofront[..., 3] > 200)
    face_diff = np.abs(master[..., :3].astype(int) - nofront[..., :3].astype(int))[skin_vis].mean()
    report.append(f"face skin, master vs nofront: mean abs difference {face_diff:.2f} / 255")
    face_img[is_("face")] = nofront[is_("face")]
    # Only the face itself: specks of outline under the bangs, away from it, are not face.
    n, cc, stats, _ = cv2.connectedComponentsWithStats(face_mask.astype(np.uint8), 8)
    face_mask = cc == (1 + int(np.argmax(stats[1:, cv2.CC_STAT_AREA])))
    under = is_("eyes", "brows", "mouth")
    under_d = cv2.dilate(under.astype(np.uint8), np.ones((5, 5), np.uint8)) > 0
    rgb = np.ascontiguousarray(face_img[..., :3])
    face_img[..., :3] = cv2.inpaint(rgb, under_d.astype(np.uint8) * 255, 7, cv2.INPAINT_TELEA)
    face_img[under_d, 3] = 255
    out.append(("face", layer(face_img, (face_mask | under_d).astype(np.float32))))

    eye_l, eye_r = split_lr(is_("eyes"))
    brow_l, brow_r = split_lr(is_("brows"))
    discs = iris_discs(master, [eye_l, eye_r])
    for side, eye, disc in zip(["left", "right"], [eye_l, eye_r], discs):
        report.append(f"iris {side}: centre {disc[0]:.1f},{disc[1]:.1f}  radii {disc[2]:.1f} x {disc[3]:.1f} px")
        parts = eye_split(master, eye, disc)
        out += [(f"{kind}_image_{side}", im) for kind, im in zip(["eye_white", "iris", "eye_lash"], parts)]

    # Closed eyes and the open mouth: the edited pixels around each feature, over its own skin.
    k = lambda n: np.ones((n, n), np.uint8)
    for name, base, grow in [("eye_image_left_closed", eye_l, 9), ("eye_image_right_closed", eye_r, 9)]:
        region = cv2.dilate(base.astype(np.uint8), k(grow)) > 0
        out.append((name, part_mask(states, region)))
    out += [("brow_image_left", part_mask(master, brow_l)), ("brow_image_right", part_mask(master, brow_r))]
    mouth = is_("mouth")
    out.append(("mouth_closed", part_mask(master, cv2.dilate(mouth.astype(np.uint8), k(3)) > 0)))
    diff = np.abs(states[..., :3].astype(int) - master[..., :3].astype(int)).sum(axis=2) > 60
    open_region = (cv2.dilate(mouth.astype(np.uint8), k(41)) > 0) & is_("face", "mouth")
    out.append(("mouth_open", part_mask(states, cv2.dilate((diff & open_region).astype(np.uint8), k(5)) > 0)))

    out.append(("front_hair", part("front_hair")))
    out.append(("hair_clip", part("hair_clip")))

    # A character without a part, such as one with no hair clip, has nothing to cut for it. Its
    # layer is left out, and its entry in rig.json's layers has to go too: the pet refuses a rig that
    # names a layer layers.json does not have.
    empty = [n for n, im in out if not im[..., 3].any()]
    out = [(n, im) for n, im in out if n not in empty]
    report += [f"{n} is empty and left out; remove it from rig.json's layers" for n in empty]

    # Every visible master pixel must land in exactly the layer that owns it at rest.
    rest = [n for n, _ in out if not n.endswith("_closed") and n != "mouth_open"]
    stack = np.zeros_like(master, np.float32)
    for name, im in out:
        if name not in rest:
            continue
        a = im[..., 3:4].astype(np.float32) / 255
        if name in CLIP:
            a = a * (dict(out)[CLIP[name]][..., 3:4].astype(np.float32) / 255)
        stack[..., :3] = im[..., :3] * a + stack[..., :3] * (1 - a)
        stack[..., 3:4] = a * 255 + stack[..., 3:4] * (1 - a)
    on = master[..., 3] > 200
    err = np.abs(stack[..., :3] - master[..., :3])[on].mean()
    report.append(f"rest stack vs master: mean abs error {err:.2f} / 255 over {on.sum()} opaque px")

    # The pet's puppet reads the layers as PNGs and this list, bottom first (Pet/ui/puppet.js).
    if len(sys.argv) > 2:
        dest = Path(sys.argv[2])
        dest.mkdir(parents=True, exist_ok=True)
        entries = []
        for name, im in out:
            ys, xs = np.nonzero(im[..., 3])
            top, left, bottom, right = int(ys.min()), int(xs.min()), int(ys.max()) + 1, int(xs.max()) + 1
            Image.fromarray(np.ascontiguousarray(im[top:bottom, left:right]), "RGBA").save(dest / f"{name}.png", optimize=True)
            entries.append({"name": name, "file": f"{name}.png", "x": left, "y": top, "w": right - left, "h": bottom - top})
        (dest / "layers.json").write_text(
            json.dumps({"canvas": [int(master.shape[1]), int(master.shape[0])], "layers": entries}, indent=2) + "\n",
            encoding="utf-8",
        )

    psd = PSDImage.new(mode="RGBA", size=master.shape[1::-1])
    for name, im in out:
        # Each layer only as large as what it holds, placed at its offset on the canvas.
        ys, xs = np.nonzero(im[..., 3])
        top, left, bottom, right = ys.min(), xs.min(), ys.max() + 1, xs.max() + 1
        crop = Image.fromarray(np.ascontiguousarray(im[top:bottom, left:right]), "RGBA")
        psd_layer = PixelLayer.frompil(crop, psd, name, top=int(top), left=int(left))
        # A clipping mask clips to the layer straight below it, which is where `out` puts the white.
        psd_layer.clipping = name in CLIP
        psd.append(psd_layer)
    psd.save(work / "character.psd")
    Image.fromarray(stack.astype(np.uint8), "RGBA").save(work / "rest_stack.png")

    for name, im in out:
        a = im[..., 3] > 0
        ys, xs = np.nonzero(a)
        box = f"x {xs.min()}-{xs.max()} y {ys.min()}-{ys.max()}" if len(xs) else "empty"
        report.append(f"  {name:24s} {int(a.sum()):7d} px  {box}")
    print("\n".join(report))


def part_mask(src: np.ndarray, mask: np.ndarray) -> np.ndarray:
    return layer(src, mask.astype(np.float32))


if __name__ == "__main__":
    main(Path(sys.argv[1]))
