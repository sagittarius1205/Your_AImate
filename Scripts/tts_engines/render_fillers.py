"""Render a character's thinking fillers into `<character>/fillers.json` (Docs/DESIGN_Characters.md).

The lines come from `fillers` in the character's `character.json`, and the voice is the character's
own: the request names the character id, so the running voice service (serve.py) clones its
`voice.wav`. Going through the service and not loading VoxCPM2 here keeps this to one model on the
GPU, the one that is already serving the pet.

Each take is shaped the way fillers.py shapes the product bank: the tail is cut to MAX_TAIL, the
level is matched to TARGET_RMS, and the envelope is measured from the PCM. The output is the same
bank format the pet reads (Pet/src-tauri/src/voice/mod.rs).

What this does not do is fillers_asr.py's Whisper check. `heard` is empty and `cer` is -1 in every
row, which says the take was not checked, so listen to them before relying on them.

  venv_voxcpm: python render_fillers.py <character folder> [--takes 2] [--port 8880]
"""

import argparse, base64, io, json, os, sys, urllib.request

import numpy as np
import soundfile as sf

WORK = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, WORK)
from harness import to_seam, SR_OUT  # noqa: E402
from fillers import FRAME, MAX_TAIL, TARGET_RMS, envelope, speech_and_tail  # noqa: E402

DEFAULT_VOICE = "ref_voice_lively_f"


def speak(port, text, voice):
    body = json.dumps({"model": "voxcpm2", "input": text, "voice": voice,
                       "response_format": "wav"}).encode("utf-8")
    req = urllib.request.Request("http://127.0.0.1:%d/v1/audio/speech" % port, data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        a, sr = sf.read(io.BytesIO(r.read()), dtype="float32")
    return to_seam(a, sr)


def shape(a):
    """Cut the tail, then match the level, as fillers.py does."""
    _, rms = envelope(a)
    speech, _, _ = speech_and_tail(rms)
    a = a[:min(len(a), int((speech + MAX_TAIL) * SR_OUT))]
    cur = float(np.sqrt(np.mean(a ** 2))) if len(a) else 0.0
    if cur > 0:
        a = a * (TARGET_RMS / cur)
    peak = float(np.max(np.abs(a))) if len(a) else 0.0
    if peak > 0.95:
        a = a * (0.95 / peak)
    # Whole frames only, because the page indexes the envelope by floor(sample / 320).
    a = a[:len(a) // FRAME * FRAME]
    env, rms = envelope(a)
    speech, _, _ = speech_and_tail(rms)
    return a, env, speech


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("folder")
    ap.add_argument("--takes", type=int, default=2)
    ap.add_argument("--port", type=int, default=8880)
    args = ap.parse_args()

    folder = os.path.abspath(args.folder)
    cid = os.path.basename(folder)
    profile = json.load(io.open(os.path.join(folder, "character.json"), encoding="utf-8"))
    lines = profile.get("fillers") or []
    assert lines, "%s/character.json has no fillers to render" % cid
    voice = cid if profile.get("voice") else DEFAULT_VOICE

    out = []
    for i, text in enumerate(lines):
        best = None
        for _ in range(args.takes):
            a, env, speech = shape(speak(args.port, text, voice))
            # A take with no voiced frame is the dropped-particle failure fillers.py describes.
            if speech <= 0:
                continue
            if best is None or speech < best[2]:
                best = (a, env, speech)
        if best is None:
            print("%-3d %-14s every take came out silent, skipped" % (i, text))
            continue
        a, env, speech = best
        pcm = (np.clip(a, -1, 1) * 32767).astype("<i2")
        out.append({
            "name": "line_%d" % i,
            "text": text,
            "secs": round(len(pcm) / SR_OUT, 3),
            "speech_secs": round(speech, 3),
            "samples": int(len(pcm)),
            "envelope": env,
            "pcm_b64": base64.b64encode(pcm.tobytes()).decode("ascii"),
            "heard": "",
            "cer": -1.0,
        })
        print("%-3d %-14s %.2f s" % (i, text, out[-1]["secs"]))

    assert out, "no take survived, nothing written"
    out.sort(key=lambda r: r["speech_secs"])
    doc = {
        "about": ("Thinking fillers for %s, rendered by Scripts/tts_engines/render_fillers.py "
                  "through the voice service. 16 kHz mono PCM16, little-endian, base64. One "
                  "envelope value per 320 samples. Not checked by ASR: heard is empty and cer "
                  "is -1." % cid),
        "sample_rate": SR_OUT,
        "frame_samples": FRAME,
        "voice": voice,
        "fillers": out,
    }
    dst = os.path.join(folder, "fillers.json")
    with io.open(dst, "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1)
    print("%d fillers written to %s" % (len(out), dst))


if __name__ == "__main__":
    main()
