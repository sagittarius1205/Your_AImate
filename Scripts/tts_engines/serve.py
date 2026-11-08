"""The product voice as a local OpenAI-compatible speech server.

The conversation core speaks one contract to every voice service it will ever use:
`POST {base}/audio/speech` with `{model, input, voice, response_format: "wav"}`, and a WAV body
back (Docs/DESIGN_Voice.md 3.3). This is the local end of that contract, running the frozen voice
(VoxCPM2 cloning ref_voice_lively_f.wav, adapters/voxcpm2_voice.py) on this machine's GPU, so the
character answers in the same voice its thinking fillers were rendered in.

It listens on 127.0.0.1 and nowhere else, and it also refuses a request whose Host header is not a
loopback name. Binding is what keeps other machines out; the Host check is what keeps out a web
page in this machine's browser that rebinds its own name to 127.0.0.1.

The body is 16 kHz mono PCM16, converted by harness.to_seam, the one format the core accepts
without resampling. Synthesis holds a lock because there is one GPU and one model.

`voice` picks the reference clip. The frozen product voice is `ref_voice_lively_f`, and a character
id (Docs/DESIGN_Characters.md) means that character's `voice.wav`, looked up in the user's
characters first and then the bundled ones. The request names an id and never a path, so a caller
cannot point the model at an arbitrary file.

  venv_voxcpm: python serve.py [--port 8880]
               python serve.py --selftest
"""

import argparse, io, json, os, re, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import soundfile as sf

WORK = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, WORK)
from harness import to_seam, SR_OUT, vram_peak_gb  # noqa: E402
from adapters import voxcpm2_voice as voice  # noqa: E402

HOST = "127.0.0.1"
PATH = "/v1/audio/speech"
WARMUP_TEXT = "暖身。"

DEFAULT_VOICE = "ref_voice_lively_f"
CHARACTER_ID = re.compile(r"[a-z0-9][a-z0-9_-]{0,47}")
VOICE_MAX_BYTES = 10 * 1024 * 1024
BUNDLED = os.path.normpath(os.path.join(WORK, "..", "..", "Pet", "ui", "characters"))


def user_characters():
    """The same home the pet uses (Pet/src-tauri/src/home.rs)."""
    home = os.environ.get("VRPET_HOME") or os.path.join(os.environ.get("USERPROFILE", ""), ".vrpet")
    return os.path.join(home, "characters")


def voice_ref(name, roots=None):
    """The reference clip for `name`, or the reason there is none."""
    if name in (None, "", DEFAULT_VOICE):
        return voice.REF_WAV, None
    if not isinstance(name, str) or not CHARACTER_ID.fullmatch(name):
        return None, "`voice` must be %r or a character id, got %r" % (DEFAULT_VOICE, name)
    for root in roots if roots is not None else (user_characters(), BUNDLED):
        folder = os.path.join(root, name)
        path = os.path.join(folder, "voice.wav")
        # A link could point the model at any file on the machine, so only a plain file that is
        # really inside the character's own folder counts.
        if os.path.islink(folder) or os.path.islink(path) or not os.path.isfile(path):
            continue
        if os.path.dirname(os.path.realpath(path)) != os.path.realpath(folder):
            continue
        if os.path.getsize(path) > VOICE_MAX_BYTES:
            return None, "%s is over %d bytes" % (path, VOICE_MAX_BYTES)
        return path, None
    return None, "character %r has no voice.wav" % name


def log(msg):
    print(msg, file=sys.stderr, flush=True)


class Voice:
    def __init__(self):
        t0 = time.perf_counter()
        self.model = voice.load()
        self.load_s = time.perf_counter() - t0
        self.lock = threading.Lock()
        t0 = time.perf_counter()
        self.synth(WARMUP_TEXT)
        self.warmup_s = time.perf_counter() - t0

    def synth(self, text, ref=None):
        """16 kHz mono float32 in [-1, 1]."""
        with self.lock:
            audio, sr = voice.synth(self.model, text, ref or voice.REF_WAV)
        return to_seam(audio, sr)


def wav_bytes(a):
    buf = io.BytesIO()
    sf.write(buf, a, SR_OUT, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def parse_request(body):
    """The spoken text and the reference clip, or the reason the request is refused."""
    try:
        req = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as e:
        return None, None, "the body is not UTF-8 JSON: %s" % e
    if not isinstance(req, dict):
        return None, None, "the body is JSON but not an object"
    text = req.get("input")
    if not isinstance(text, str) or not text.strip():
        return None, None, "`input` must be a non-empty string"
    fmt = req.get("response_format")
    if fmt != "wav":
        return None, None, "`response_format` must be \"wav\", this server makes nothing else (got %r)" % (fmt,)
    for key in ("model", "voice"):
        if key in req and not isinstance(req[key], str):
            return None, None, "`%s` must be a string" % key
    ref, why = voice_ref(req.get("voice"))
    if why:
        return None, None, why
    return text, ref, None


def loopback_host(header, port):
    return header in ("%s:%d" % (HOST, port), "localhost:%d" % port, "[::1]:%d" % port)


def handler(engine, port):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def reply(self, status, body, ctype):
            self.send_response(status)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def error(self, status, message):
            self.reply(status, json.dumps({"error": {"message": message}}).encode("utf-8"),
                       "application/json")

        def do_GET(self):
            self.error(404, "only POST %s exists here" % PATH)

        def do_POST(self):
            if not loopback_host(self.headers.get("Host", ""), port):
                self.error(403, "Host must be %s:%d" % (HOST, port))
                return
            if self.path != PATH:
                self.error(404, "only POST %s exists here" % PATH)
                return
            try:
                length = int(self.headers.get("Content-Length", ""))
            except ValueError:
                self.error(400, "Content-Length is required")
                return
            text, ref, why = parse_request(self.rfile.read(length))
            if why:
                self.error(400, why)
                return
            t0 = time.perf_counter()
            try:
                a = engine.synth(text, ref)
            except Exception as e:  # the model's own failure, reported rather than dropped
                log("synth failed on %r: %r" % (text, e))
                self.error(500, "synthesis failed: %r" % e)
                return
            wall = time.perf_counter() - t0
            secs = len(a) / SR_OUT
            log("synth %d chars -> %.2f s audio in %.2f s, RTF %s"
                % (len(text), secs, wall, "%.3f" % (wall / secs) if secs else "n/a"))
            self.reply(200, wav_bytes(a), "audio/wav")

        def log_message(self, fmt, *args):
            log("%s %s" % (self.address_string(), fmt % args))

    return Handler


def selftest():
    """Which clip a request gets, without loading the model. Prints one SelfTest line."""
    import tempfile
    failed = []

    def check(ok, what):
        if not ok:
            failed.append(what)

    with tempfile.TemporaryDirectory() as tmp:
        user, bundled = os.path.join(tmp, "user"), os.path.join(tmp, "bundled")
        for root, cid in ((user, "mine"), (bundled, "mine"), (bundled, "shipped")):
            os.makedirs(os.path.join(root, cid))
            with open(os.path.join(root, cid, "voice.wav"), "wb") as f:
                f.write(b"RIFF")
        os.makedirs(os.path.join(user, "big"))
        with open(os.path.join(user, "big", "voice.wav"), "wb") as f:
            f.truncate(VOICE_MAX_BYTES + 1)
        roots = (user, bundled)

        check(voice_ref(None, roots) == (voice.REF_WAV, None), "no voice is the product voice")
        check(voice_ref(DEFAULT_VOICE, roots) == (voice.REF_WAV, None), "the default name is the product voice")
        check(voice_ref("mine", roots)[0] == os.path.join(user, "mine", "voice.wav"), "the user's copy wins")
        check(voice_ref("shipped", roots)[0] == os.path.join(bundled, "shipped", "voice.wav"), "a bundled one is found")
        check(voice_ref("nobody", roots)[0] is None, "an id with no voice.wav is refused")
        check(voice_ref("big", roots)[0] is None, "an oversize clip is refused")
        for bad in ("../mine", "Mine", "a/b", "a\\b", "C:\\x", "", "-x"):
            if bad:
                check(voice_ref(bad, roots)[0] is None, "%r is not an id" % bad)
        try:
            os.symlink(os.path.join(bundled, "shipped"), os.path.join(user, "linked"))
            check(voice_ref("linked", roots)[0] is None, "a linked folder is refused")
        except OSError:
            pass  # creating links needs a privilege Windows may not grant; the check is then skipped

    ok_body = json.dumps({"input": "你好", "response_format": "wav"}).encode("utf-8")
    check(parse_request(ok_body) == ("你好", voice.REF_WAV, None), "a request with no voice parses")
    bad_body = json.dumps({"input": "你好", "response_format": "wav", "voice": "../../x"}).encode("utf-8")
    check(parse_request(bad_body)[2] is not None, "a request with a bad voice is refused")

    if failed:
        print("SelfTest: FAIL serve: " + " | ".join(failed))
        return 1
    print("SelfTest: PASS serve: voice lookup")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--port", type=int, default=8880)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        sys.exit(selftest())

    try:
        import torch
        torch.cuda.reset_peak_memory_stats()
    except Exception:
        pass
    engine = Voice()
    server = ThreadingHTTPServer((HOST, args.port), handler(engine, args.port))
    peak = vram_peak_gb()
    print("ready: http://%s:%d/v1 load %.1f s, warm-up %.1f s, torch peak VRAM %s GB"
          % (HOST, args.port, engine.load_s, engine.warmup_s,
             "%.2f" % peak if peak is not None else "?"), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
