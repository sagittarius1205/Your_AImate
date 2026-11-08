"""The product's voice. Same engine and settings as voxcpm2_best, different reference clip.

voxcpm2_best stays pointed at ref_tw_best.wav because its numbers are the engine baseline in the
research doc: RTF 0.604 and mean CER 0.023 across six lines. Repointing it would quietly make
that row describe something it was never measured on. So the product gets its own name here, and
the baseline keeps its own.

The reference this one speaks through was synthesised from a written description rather than
recorded, so no person's voice is cloned. Provenance and the replication runs behind the choice
are in ref_voice_lively_f.json beside the clip.
"""
import os

# Set before the import below, because voxcpm2_best reads REF_WAV and REF_JSON at import time and
# builds STREAM_KWARGS from them. setdefault, so an explicit REF_WAV from the caller still wins.
# Safe only because harness.py imports exactly one adapter per process.
REF_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # the clip ships beside serve.py
os.environ.setdefault("REF_WAV", os.path.join(REF_DIR, "ref_voice_lively_f.wav"))
os.environ.setdefault("REF_JSON", os.path.join(REF_DIR, "ref_voice_lively_f.json"))

from adapters.voxcpm2_best import (  # noqa: E402  (import order is the point, see above)
    META, REFERENCE_SECONDS, REF_TEXT, REF_WAV, STREAM_KWARGS, load, synth)

__all__ = ["META", "REFERENCE_SECONDS", "REF_TEXT", "REF_WAV", "STREAM_KWARGS", "load", "synth"]
