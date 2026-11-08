"""One measurement台 for every candidate engine.

Each engine ships an adapter module in D:/tts_trials/work/adapters/<name>.py exposing:

    REFERENCE_SECONDS: float | None   # how much reference audio the engine was given, None if it clones nothing
    def load() -> object              # build the model, return whatever synth() needs
    def synth(model, text: str) -> tuple[np.ndarray, int]   # float32 mono, native sample rate

The harness owns everything else, so the numbers are comparable across engines: the same six
lines, the same resample to the seam's 16 kHz, the same warm-up discarded, the same two timed
runs with the better one kept, the same peak-VRAM accounting.

RTF here is wall time over OUTPUT seconds, and it is the coefficient of first-sound latency
because the seam hands over a complete buffer. An engine that streams internally gets no credit
for it here; if streaming turns out to be what saves us, the seam is what changes, not this
number.

Peak VRAM is torch's allocator high-water mark, which undercounts: the CUDA context, cuDNN
workspaces and any non-torch allocation sit outside it. Treat it as a floor, not the total.
"""

import importlib, io, json, os, sys, time

import numpy as np
import soundfile as sf
from scipy.signal import resample_poly

SR_OUT = 16000
WORK = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, WORK)

LINES = json.load(io.open(os.path.join(WORK, "lines.json"), encoding="utf-8"))
ORDER = ["L1", "L1n", "L2", "L3", "L4", "L5"]


def to_seam(a, sr):
    """Resample to 16 kHz mono float32 in [-1, 1], the only format the seam accepts."""
    a = np.asarray(a, dtype=np.float32)
    if a.ndim > 1:
        a = a.mean(axis=1)
    if sr != SR_OUT:
        from math import gcd
        g = gcd(int(sr), SR_OUT)
        a = resample_poly(a, SR_OUT // g, int(sr) // g)
    # Scale down rather than clip. The previous np.clip hard-limited anything over full scale,
    # which is audible distortion, and it showed up the moment a brighter reference pushed
    # VoxCPM2's output to a peak of exactly 1.000 on two lines. Resampling also overshoots
    # slightly, so a take can exceed full scale without the model being at fault. Clipping the
    # audio lin is asked to judge for naturalness would be measuring the harness, not the engine.
    peak = float(np.max(np.abs(a))) if a.size else 0.0
    if peak > 0.999:
        a = a * (0.999 / peak)
        print("    peak was %.3f, scaled down instead of clipped" % peak, flush=True)
    return a


def vram_peak_gb():
    try:
        import torch
        if torch.cuda.is_available():
            return torch.cuda.max_memory_reserved() / 1e9
    except Exception:
        pass
    return None


def main():
    name = sys.argv[1]
    out = os.path.join(WORK, "takes")
    os.makedirs(out, exist_ok=True)
    ad = importlib.import_module("adapters.%s" % name)

    try:
        import torch
        torch.cuda.reset_peak_memory_stats()
    except Exception:
        pass

    t0 = time.perf_counter()
    model = ad.load()
    load_s = time.perf_counter() - t0
    print("%s loaded in %.1f s" % (name, load_s), flush=True)

    rows = []
    for lid in ORDER:
        text = LINES[lid]
        try:
            ad.synth(model, text)  # warm-up, discarded
        except Exception as e:
            print("%s %s FAILED on warm-up: %r" % (name, lid, e), flush=True)
            rows.append({"engine": name, "line": lid, "error": repr(e)[:300]})
            continue
        walls, a16 = [], None
        for _ in range(2):
            t1 = time.perf_counter()
            audio, sr = ad.synth(model, text)
            walls.append(time.perf_counter() - t1)
            a16 = to_seam(audio, sr)
        secs = len(a16) / SR_OUT
        # Mandarin tops out near 8 characters a second, so a take under a third of that is not a
        # fast take, it is a broken one. Audio8's first run returned 0.09 to 0.28 s for sentences
        # that should run 5 to 9 s, and its RTF came out looking merely bad rather than void.
        # Refuse to write that number down.
        floor = len(text) / 24.0
        if secs < floor:
            msg = ("produced %.2f s for %d characters, under the %.2f s floor; broken, not fast"
                   % (secs, len(text), floor))
            print("%s %s %s" % (name, lid, msg), flush=True)
            rows.append({"engine": name, "line": lid, "chars": len(text),
                         "secs": round(secs, 2), "error": msg})
            continue
        best = min(walls)
        sf.write(os.path.join(out, "%s_%s.wav" % (name, lid)), a16, SR_OUT, subtype="PCM_16")
        rows.append({"engine": name, "line": lid, "chars": len(text), "secs": round(secs, 2),
                     "wall": round(best, 2), "rtf": round(best / secs, 3),
                     "peak": round(float(np.max(np.abs(a16))), 3),
                     "silent": bool(np.max(np.abs(a16)) < 0.01)})
        print(json.dumps(rows[-1], ensure_ascii=False), flush=True)

    meta = {"engine": name, "load_s": round(load_s, 1), "vram_peak_gb": vram_peak_gb(),
            "reference_seconds": getattr(ad, "REFERENCE_SECONDS", None), "rows": rows}
    with io.open(os.path.join(out, "%s.json" % name), "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=1)
    ok = [r for r in rows if "rtf" in r]
    if ok:
        print("%s: median RTF %.3f, peak VRAM %s GB, %d/%d lines produced"
              % (name, sorted(r["rtf"] for r in ok)[len(ok) // 2], meta["vram_peak_gb"], len(ok), len(ORDER)))


if __name__ == "__main__":
    main()
