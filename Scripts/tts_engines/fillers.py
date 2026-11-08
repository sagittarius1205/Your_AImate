"""Pre-render the thinking fillers the pet plays the instant the user finishes speaking.

Why this exists. The character is silent for a measured 7 to 14 seconds after the user speaks:
the LLM turn alone is 3 to 10 s (ChatCore/src/claude.rs:29-31) and synthesising the whole reply
is about 4 s on top. Changing the seam was going to buy about one second of the smaller term, and
measuring it found a four-second hole mid-sentence instead (Docs/DESIGN_ChatCore.md section 11).
A short vocalisation played at once costs nothing at run time and covers the part that matters,
which is the silence right after the user stops.

This is NOT the synthetic envelope Docs/DESIGN_Voice.md:155 forbids. That ban is on faking mouth
movement from character counts. These are real VoxCPM2 takes in the product's own frozen voice, so
the envelope below is measured from real PCM and the page's mouth path is the same one real speech
will use.

What it writes, per surviving take: a 16 kHz mono PCM16 wav, and an envelope of one loudness value
per 320 samples, which is the frame length and rate Docs/DESIGN_Pet.md:424 says the page consumes
(`FVRSpeechFace::ChunkSamples` 320 at 16 kHz). The silence gate is the solver's own
`SilenceRms = 0.00316f`, about -50 dBFS (Source/VRPhysProto/VRSpeechFace.h:40).

Every take is checked rather than trusted, because this engine drops an utterance-initial
particle: four of the six designed references dropped one on a single run, and a bare 「嗯」 is
exactly that shape. A filler that came out empty would be a character that answers silence with
silence, and it would look like the feature simply not working.

  venv_voxcpm: python fillers.py [takes_per_line]
"""

import io, json, os, shutil, sys, time

import numpy as np
import soundfile as sf

WORK = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, WORK)
from harness import to_seam, SR_OUT  # noqa: E402

REF = "D:/tts_trials/ref/ref_voice_lively_f.wav"
OUT = "D:/tts_trials/fillers"
FRAME = 320                 # FVRSpeechFace::ChunkSamples, 20 ms at 16 kHz
SILENCE_RMS = 0.00316       # FVRSpeechFace::SilenceRms, about -50 dBFS
TARGET_RMS = 0.12           # the level blind.py matches takes at, so fillers sit where speech does
MAX_TAIL = 0.30             # a longer tail holds the mouth shut after the sound (section 17.7)

# Short, and none of them a bare particle on its own. 「嗯……」 is in anyway, as the control: it is
# the shape the engine is known to drop, so if it survives the checks that is worth knowing, and if
# it does not the check caught exactly what it is for.
LINES = {
    "hmm": "嗯……",
    "let_me_think": "嗯，我想一下。",
    "let_me_see": "我看看喔。",
    "wait_a_sec": "唔，等我一下。",
    "oh_that": "喔，這個喔。",
    "good_question": "欸，好問題。",
    "thinking_hmm": "嗯嗯，讓我想想。",
    "ok_so": "好，那個喔。",
}


def envelope(a):
    """One loudness value per 320 samples, 0..1, gated at the solver's own silence floor.

    Loudness, not a solved mouth weight. `cue/beats.rs` takes this same envelope to find prosodic
    cues (Docs/DESIGN_Pet.md:408) and `VRGesturePlan` uses per-frame loudness the same way, so
    shipping the solved weight instead would give the beat detector something it cannot use.
    Normalised by this file's own peak frame, which is what `VRGesturePlan.cpp:213` does within one
    plan.
    """
    n = len(a) // FRAME
    rms = np.array([float(np.sqrt(np.mean(a[i * FRAME:(i + 1) * FRAME] ** 2) + 1e-20))
                    for i in range(n)])
    peak = float(rms.max()) if n else 0.0
    if peak <= 0.0:
        return [0.0] * n, rms
    env = np.where(rms < SILENCE_RMS, 0.0, rms / peak)
    return [round(float(v), 4) for v in env], rms


def speech_and_tail(rms):
    voiced = np.flatnonzero(rms >= SILENCE_RMS)
    if not voiced.size:
        return 0.0, 0.0, 0
    speech_end = (voiced[-1] + 1) * FRAME / SR_OUT
    return speech_end, len(rms) * FRAME / SR_OUT - speech_end, int(voiced.size)


def main():
    takes = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    from voxcpm import VoxCPM
    assert os.path.exists(REF), REF
    if os.path.isdir(OUT):
        shutil.rmtree(OUT)
    os.makedirs(OUT)
    m = VoxCPM.from_pretrained("openbmb/VoxCPM2", load_denoiser=False, optimize=True,
                               device="cuda")
    m.generate(text="暖身。", reference_wav_path=REF, cfg_value=2.0, inference_timesteps=10)

    rows = []
    for name, text in LINES.items():
        for take in range(1, takes + 1):
            t0 = time.perf_counter()
            wav = m.generate(text=text, reference_wav_path=REF, cfg_value=2.0,
                             inference_timesteps=10)
            wall = time.perf_counter() - t0
            a = to_seam(wav, m.tts_model.sample_rate)

            # Trim the tail before measuring the level, so a padded take does not read as quiet.
            env, rms = envelope(a)
            speech, tail, voiced_frames = speech_and_tail(rms)
            keep = min(len(a), int((speech + MAX_TAIL) * SR_OUT))
            a = a[:keep]
            env, rms = envelope(a)
            speech, tail, voiced_frames = speech_and_tail(rms)

            cur = float(np.sqrt(np.mean(a ** 2))) if len(a) else 0.0
            if cur > 0:
                a = a * (TARGET_RMS / cur)
            peak = float(np.max(np.abs(a))) if len(a) else 0.0
            if peak > 0.95:
                a = a * (0.95 / peak)
            env, rms = envelope(a)

            stem = "%s_%d" % (name, take)
            path = os.path.join(OUT, stem + ".wav")
            sf.write(path, a, SR_OUT, subtype="PCM_16")
            row = {"name": name, "take": take, "text": text, "wav": path,
                   "secs": round(len(a) / SR_OUT, 3), "speech_secs": round(speech, 3),
                   "tail_secs": round(tail, 3), "voiced_frames": voiced_frames,
                   "frames": len(env), "peak": round(float(np.max(np.abs(a))), 3),
                   "rms": round(float(np.sqrt(np.mean(a ** 2))), 4),
                   "wall": round(wall, 2), "envelope": env}
            rows.append(row)
            print("%-14s take %d  %.2f s (%.2f speech, %.2f tail)  %d voiced frames  peak %.2f"
                  % (name, take, row["secs"], speech, tail, voiced_frames, row["peak"]),
                  flush=True)

    with io.open(os.path.join(OUT, "fillers.json"), "w", encoding="utf-8") as f:
        json.dump(rows, f, ensure_ascii=False, indent=1)

    # Mechanical rejections. The ear decides between what survives; these only throw out what is
    # broken, which is the only job measurement has left in this study (RESEARCH_TTS_Engines
    # section 17.6).
    print("\nmechanical checks, before anyone listens:")
    print("%-14s %-5s %8s %8s %8s %6s %s" % ("name", "take", "secs", "speech", "tail", "peak",
                                             "verdict"))
    for r in rows:
        why = []
        if r["voiced_frames"] < 5:
            why.append("under 5 voiced frames, it is effectively silent")
        if r["speech_secs"] < 0.3:
            why.append("speech under 0.3 s, too short to register")
        if r["speech_secs"] > 2.5:
            why.append("over 2.5 s, a filler this long delays the real answer")
        if r["tail_secs"] > MAX_TAIL + 0.01:
            why.append("tail over %.2f s" % MAX_TAIL)
        if r["peak"] >= 0.999:
            why.append("clipping")
        r["rejected"] = why
        print("%-14s %-5d %8.2f %8.2f %8.2f %6.2f %s"
              % (r["name"], r["take"], r["secs"], r["speech_secs"], r["tail_secs"], r["peak"],
                 "; ".join(why) or "ok"))

    with io.open(os.path.join(OUT, "fillers.json"), "w", encoding="utf-8") as f:
        json.dump(rows, f, ensure_ascii=False, indent=1)
    ok = [r for r in rows if not r["rejected"]]
    print("\n%d of %d takes pass the mechanical checks, over %d distinct lines"
          % (len(ok), len(rows), len({r["name"] for r in ok})))
    print("readings are NOT settled here. Run asr_check.py over %s next: the engine drops an"
          % OUT)
    print("utterance-initial particle, and 嗯 on its own is exactly that shape.")


if __name__ == "__main__":
    main()
