"""VoxCPM2 cloning the best-recorded Taiwanese reference in the corpus.

lin's complaint about the previous take was that it sounded like a phone call. That turned out to
be measurable and to be my fault, not the engine's: the reference clip's energy stopped at
5142 Hz with 0.10% above 7 kHz, and the clone inherited 4816 Hz. Zero-shot cloning copies the
recording channel along with the voice.

The clip it uses now is 10.74 s at 14121 Hz with 15.73% of its energy above 7 kHz, built from the
widest clips of the contributor with the most wideband material in the corpus (73 of 93 clips,
202 s). Taipei accent rather than the Pingtung speaker who has more material still, because lin
asked for Mandarin without a dialect flavour.

The seam was never the constraint here. 16 kHz carries content up to 8 kHz, and Ming's take
reached 7918 Hz through the same seam; the old reference simply had nothing above 5 kHz to carry.
"""
import io, json, os

REF_DIR = "D:/tts_trials/ref"
REF_WAV = os.environ.get("REF_WAV") or os.path.join(REF_DIR, "ref_tw_best.wav")
META_JSON = os.environ.get("REF_JSON") or os.path.join(REF_DIR, "ref_tw_best.json")

with io.open(META_JSON, encoding="utf-8") as f:
    META = json.load(f)
REF_TEXT = META["ref_text"]
REFERENCE_SECONDS = META["secs"]

STREAM_KWARGS = {"cfg_value": 2.0, "inference_timesteps": 10, "reference_wav_path": REF_WAV}


def load():
    import torch
    from voxcpm import VoxCPM
    assert torch.cuda.is_available(), "torch has no CUDA, the RTF would be a CPU number"
    assert os.path.exists(REF_WAV), REF_WAV
    m = VoxCPM.from_pretrained("openbmb/VoxCPM2", load_denoiser=False, optimize=True, device="cuda")
    dev = str(next(m.tts_model.parameters()).device)
    assert dev.startswith("cuda"), "model landed on %s, not cuda" % dev
    print("device %s, reference %.2f s, 99.5%% at %s Hz, %.2f%% above 7 kHz"
          % (dev, REFERENCE_SECONDS, META.get("f995_hz"), META.get("energy_above_7k_pct", 0.0)))
    print("ref_text: %s" % REF_TEXT)
    return m


def synth(model, text, ref=REF_WAV):
    wav = model.generate(text=text, reference_wav_path=ref,
                         cfg_value=2.0, inference_timesteps=10)
    return wav, model.tts_model.sample_rate
