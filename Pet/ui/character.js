// The only file that knows how the figure is drawn (Docs/DESIGN_Pet.md 2). Above this seam the page
// talks about sessions, rows and a hit rectangle; below it, a layered puppet on a 2D canvas.
//
// The figure is lin's own illustration cut into layers, and this file moves the layers itself
// (Docs/DESIGN_Pet.md 3.4). There is no Cubism model and no runtime underneath: lin chose not to rig
// her in Cubism, and the Cubism Core could not have gone out with an open-source pet in any case.
// `rig.json` beside the layers says how she moves, and nothing outside this file reads it.
//
// A classic script and not a module, for the same reason as the rest of this page: there is no npm
// and no bundler here, so the page reads `Character` off `window`.
window.Character = (function () {
  "use strict";

  // The rig being drawn, as `mount` was given it. Rust names it (cast.rs, the `characters`
  // command), and only from the characters bundled with the pet.
  let model = null;

  // A string and not a set of booleans, so no reader can hold a state that is both ready and
  // failed. `Absent` is before `mount` ran, which is distinct from `Missing`: one is our own
  // timing, the other is a file the bundle does not have.
  const State = {
    Absent: "absent",
    Loading: "loading",
    Ready: "ready",
    Missing: "missing",
    Failed: "failed",
  };

  let state = State.Absent;
  let error = null;
  let canvas = null;
  let ctx = null;
  // Where a clipped layer is put together with its mask before it goes on `canvas` (`drawClipped`),
  // the same size as `canvas`.
  let clipCanvas = null;
  let clipCtx = null;
  // The rig once it is built: `nodes` in parent-first order, `layers` in draw order, `idle` and
  // `physics` with their parameters resolved. Null until `mount` has every file.
  let rig = null;
  // Rig space to canvas pixels, from `layout`.
  let fit = null;
  let lastFrame = null;

  // Every parameter of the puppet, in the rig's order, as `{ id, index, value, low, high, rest }`,
  // and the same objects by id. `idle` sets every value first thing in each frame, so whatever a
  // later writer puts there is gone by the next one, as Cubism's `loadParameters` made it before.
  let params = [];
  let byId = new Map();

  // The mouth has one writer, `writeMouth`, and one source of how far it opens, which the page
  // registers with `mouth()`. `mouthTap` is how the probe looks at a frame once it is drawn, and it
  // is null the rest of the time.
  let mouthSource = null;
  let mouthSourceError = null;
  let mouthTap = null;
  // What `wireMouth` found. `why` is the whole state: null means `index` and the range are the
  // parameter's own, a string means the mouth is not wired and says why. A string and not a boolean
  // beside a message, for the reason `State` is one.
  const mouthWiring = { id: null, index: -1, low: 0, high: 1, why: "the character is not mounted" };

  // The gestures have one writer too, `writeGestures`, and one source, which the page registers with
  // `gestures()`. `gestureWiring.why` is the whole state, as `mouthWiring.why` is.
  let gestureSource = null;
  let gestureSourceError = null;
  const gestureWiring = { why: "the character is not mounted" };
  // The lanes of the plan that is playing, looked up once per plan and not once per frame. Keyed by
  // the plan's actions array, which is one per clip.
  let planLanes = { actions: null, lanes: [] };
  // Every lane a plan asked for and this file would not write, with the reason, for `status`.
  const refusedLanes = new Set();

  // Where she looks has one source too, which the page registers with `look()`: a function that
  // answers the cursor in window CSS pixels, or null when nothing is known. `lookEye` is the point
  // she looks from, in the same pixels, from `layout`.
  let lookSource = null;
  let lookSourceError = null;
  let lookEye = null;

  // A file that is not there is `Missing` and names the file; anything else that goes wrong while
  // mounting is `Failed`. The fix differs: one is the install, the other is this commit.
  function missing(file, why) {
    const e = new Error(file + " could not be loaded: " + why);
    e.missing = true;
    return e;
  }

  async function fetchJson(path) {
    let r;
    try {
      r = await fetch(path);
    } catch (e) {
      throw missing(path, String((e && e.message) || e));
    }
    if (!r.ok) throw missing(path, "HTTP " + r.status);
    return r.json();
  }

  function loadImage(path) {
    return new Promise(function (resolve, reject) {
      const img = new Image();
      // A user's character is served from another origin (the `character` scheme), and an image
      // from another origin taints the canvas unless it is fetched with CORS: `drawn()` and `shot()`
      // read the canvas back and would throw. Rust answers that scheme with the header this needs.
      img.crossOrigin = "anonymous";
      img.onload = function () {
        resolve(img);
      };
      img.onerror = function () {
        reject(missing(path, "the image did not load"));
      };
      img.src = path;
    });
  }

  // The rig as `draw` reads it, checked against the art and against its own parameter table. A
  // drive on a parameter the rig does not declare is a broken rig, so it throws here and the figure
  // is `Failed` with the name in the message, rather than drawing a body that ignores one of its
  // joints.
  function build(spec, art, images) {
    params = spec.parameters.map(function (p, index) {
      return { id: p.id, index: index, value: p.default, low: p.min, high: p.max, rest: p.default };
    });
    byId = new Map(
      params.map(function (p) {
        return [p.id, p];
      }),
    );
    const param = function (id, where) {
      const p = byId.get(id);
      if (!p) throw new Error(where + " drives " + id + ", which rig.json does not declare");
      return p;
    };
    const drives = function (list, where) {
      return (list || []).map(function (d) {
        const p = param(d.param, where);
        return {
          p: p,
          at: d.at === undefined ? p.rest : d.at,
          x: d.x || 0,
          y: d.y || 0,
          rotate: d.rotate || 0,
          sx: d.sx || 0,
          sy: d.sy || 0,
        };
      });
    };

    const nodes = [];
    const named = new Set();
    spec.nodes.forEach(function (n) {
      if (n.parent && !named.has(n.parent)) throw new Error("node " + n.name + " comes before its parent " + n.parent);
      nodes.push({ name: n.name, parent: n.parent || null, pivot: n.pivot, drive: drives(n.drive, "node " + n.name) });
      named.add(n.name);
    });

    const inArt = new Set(
      art.layers.map(function (l) {
        return l.name;
      }),
    );
    Object.keys(spec.layers).forEach(function (name) {
      if (!inArt.has(name)) throw new Error("rig.json rigs " + name + ", which layers.json does not have");
    });
    const order = art.layers.map(function (l) {
      return l.name;
    });
    const layers = art.layers.map(function (l, index) {
      const r = spec.layers[l.name] || {};
      if (r.node && !named.has(r.node)) throw new Error("layer " + l.name + " hangs from " + r.node + ", which is not a node");
      const show = r.show
        ? { p: param(r.show.param, "layer " + l.name), from: r.show.from, to: r.show.to }
        : null;
      // A layer can only be clipped to one that is already drawn this frame, so a clip naming a
      // later layer is as broken as one naming no layer at all.
      const clip = r.clip === undefined ? -1 : order.indexOf(r.clip);
      if (r.clip !== undefined && !(clip >= 0 && clip < index)) {
        throw new Error("layer " + l.name + " is clipped to " + r.clip + ", which is not a layer drawn before it");
      }
      return {
        name: l.name,
        image: images.get(l.name),
        x: l.x,
        y: l.y,
        node: r.node || null,
        pivot: r.pivot || [l.x + l.w / 2, l.y + l.h / 2],
        drive: drives(r.drive, "layer " + l.name),
        show: show,
        clip: clip,
      };
    });

    // The figure's box at rest, for `layout`. The layers' own boxes and not their pixels this frame,
    // so the figure does not breathe in and out of its own rectangle.
    const extent = art.layers.reduce(
      function (e, l) {
        return {
          left: Math.min(e.left, l.x),
          top: Math.min(e.top, l.y),
          right: Math.max(e.right, l.x + l.w),
          bottom: Math.max(e.bottom, l.y + l.h),
        };
      },
      { left: Infinity, top: Infinity, right: -Infinity, bottom: -Infinity },
    );

    return {
      nodes: nodes,
      layers: layers,
      extent: extent,
      blink: (spec.eyeBlink || []).map(function (id) {
        return param(id, "eyeBlink");
      }),
      idle: (spec.idle || []).map(function (m) {
        const p = param(m.param, "idle");
        return { p: p, mid: m.mid === undefined ? p.rest : m.mid, size: m.size, period: m.period, phase: m.phase };
      }),
      physics: (spec.physics || []).map(function (h) {
        return {
          p: param(h.param, "physics"),
          input: h.input.map(function (i) {
            return { p: param(i.param, "physics"), by: i.by };
          }),
          hz: h.hz,
          damping: h.damping,
          gain: h.gain,
          follow: null,
          speed: 0,
        };
      }),
      lipSync: spec.lipSync,
      look: spec.look
        ? {
            at: spec.look.at,
            reach: spec.look.reach,
            speed: spec.look.speed,
            x: 0,
            y: 0,
            lanes: spec.look.lanes.map(function (k) {
              if (k.axis !== "x" && k.axis !== "y") throw new Error("look turns " + k.param + " on axis " + k.axis + ", not x or y");
              return { p: param(k.param, "look"), axis: k.axis, by: k.by };
            }),
          }
        : null,
    };
  }

  // The figure fitted into the box by its height, centred across it.
  //
  // The top is held `TOP` of the box's height into it because the choices' band ends where the
  // figure starts: in a 320 box the box begins at window y 224 and `pet_page` holds the cards above
  // 250.4, her highest point in motion (Docs/DESIGN_Pet.md 4.4.1 and 4.4.6). 33 of 320 puts her hair
  // at 257 at rest and leaves the head room to nod up and tilt without reaching the band. `BOTTOM`
  // keeps her boots inside the dialogue box, which covers her from the knees down. Fractions and not
  // pixels, so a box the settings make larger (index.html `--figure`) makes all of her larger by the
  // same factor, head room included, and the band's arithmetic holds at every size.
  const TOP = 33 / 320;
  const BOTTOM = 6 / 320;

  function layout() {
    if (!canvas || !rig) return;
    const box = canvas.parentElement || canvas;
    const w = box.clientWidth;
    const h = box.clientHeight;
    const dpr = window.devicePixelRatio || 1;
    // The backing store at device pixels, so the figure is as sharp as the screen and the alpha
    // read-back in `drawn` counts the pixels the window composites.
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    clipCanvas.width = canvas.width;
    clipCanvas.height = canvas.height;
    const e = rig.extent;
    const scale = (h * (1 - TOP - BOTTOM)) / (e.bottom - e.top);
    fit = new DOMMatrix()
      .scaleSelf(dpr)
      .translateSelf((w - (e.right - e.left) * scale) / 2, h * TOP)
      .scaleSelf(scale)
      .translateSelf(-e.left, -e.top);
    if (rig.look) {
      const r = canvas.getBoundingClientRect();
      const q = fit.transformPoint(new DOMPoint(rig.look.at[0], rig.look.at[1]));
      lookEye = { x: r.left + q.x / dpr, y: r.top + q.y / dpr };
    }
  }

  async function mount(el, rigPath) {
    state = State.Loading;
    canvas = el;
    model = rigPath;
    try {
      ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("this page cannot get a 2D canvas");
      clipCanvas = document.createElement("canvas");
      clipCtx = clipCanvas.getContext("2d");
      if (!clipCtx) throw new Error("this page cannot get a 2D canvas to clip layers on");
      const base = model.slice(0, model.lastIndexOf("/") + 1);
      const spec = await fetchJson(model);
      const art = await fetchJson(base + spec.art);
      const images = new Map(
        await Promise.all(
          art.layers.map(function (l) {
            return loadImage(base + l.file).then(function (img) {
              return [l.name, img];
            });
          }),
        ),
      );
      rig = build(spec, art, images);
      layout();
      // The box can change size and the screen its density, and either leaves the backing store
      // the wrong size until something says so.
      new ResizeObserver(layout).observe(canvas.parentElement || canvas);
      window.addEventListener("resize", layout);
      // `wireMouth` does not throw: a mouth that cannot be wired is a line in `status`, not a reason
      // to lose the character.
      wireMouth();
      wireGestures();
      state = State.Ready;
      requestAnimationFrame(frame);
    } catch (e) {
      state = e && e.missing ? State.Missing : State.Failed;
      error = String((e && e.message) || e);
      rig = null;
    }
    return status();
  }

  // One frame, in the order the Cubism hooks used to give it (Docs/DESIGN_Pet.md 4.3): the idle sets
  // every parameter, she turns toward the cursor on top of it, the gestures blend over what those
  // gave, the hair answers where the head ended up, and the mouth is written last, straight before
  // the figure is drawn. The look comes before the gestures so a glance or a nod still wins.
  //
  // The next frame is asked for first, so a throw below costs one frame and not the figure.
  function frame(now) {
    requestAnimationFrame(frame);
    const dt = lastFrame === null ? 0 : Math.min((now - lastFrame) / 1000, 0.05);
    lastFrame = now;
    idle(now / 1000);
    writeLook(dt);
    writeGestures();
    physics(dt);
    const said = writeMouth();
    draw();
    if (mouthTap && said !== undefined) mouthTap(said);
  }

  // The puppet's own life while nothing else drives her, which Mao's idle motion was: breathing, a
  // slow sway of head and body, a blink every few seconds. Every parameter is set back to its rest
  // first, so this is the base every later writer of the frame blends from.
  function idle(t) {
    params.forEach(function (p) {
      p.value = p.rest;
    });
    rig.idle.forEach(function (m) {
      m.p.value = within(m.mid + m.size * Math.sin((2 * Math.PI * t) / m.period + m.phase), m.p);
    });
    const open = lid(t);
    rig.blink.forEach(function (p) {
      p.value = p.low + (p.rest - p.low) * open;
    });
  }

  // A blink closes in 70 ms, stays shut for 40 and opens over 120, the way a real one is quick down
  // and slower up. The gap is 2 to 6 s and drawn afresh each time, because a metronome blink is the
  // first thing that reads as a machine.
  const BLINK = { close: 0.07, shut: 0.04, open: 0.12 };
  let blinkAt = 1.5;

  function lid(t) {
    const s = t - blinkAt;
    if (s >= BLINK.close + BLINK.shut + BLINK.open) {
      blinkAt = t + 2 + Math.random() * 4;
      return 1;
    }
    if (s < 0) return 1;
    if (s < BLINK.close) return 1 - s / BLINK.close;
    if (s < BLINK.close + BLINK.shut) return 0;
    return (s - BLINK.close - BLINK.shut) / BLINK.open;
  }

  // The hair, on a damped spring that follows the head. It reads the head after the gestures so a
  // nod or a glance swings it as much as the idle does, which is what physics reading the gesture
  // parameters was for on Mao (Docs/DESIGN_Pet.md 4.3.2). The hair is written as how far it trails
  // behind: zero while the head holds still, and the opposite way to a sudden turn.
  function physics(dt) {
    rig.physics.forEach(function (h) {
      const head = h.input.reduce(function (sum, i) {
        return sum + i.by * i.p.value;
      }, 0);
      if (h.follow === null) h.follow = head;
      const w = 2 * Math.PI * h.hz;
      h.speed += (w * w * (head - h.follow) - 2 * h.damping * w * h.speed) * dt;
      h.follow += h.speed * dt;
      h.p.value = within(h.p.value + h.gain * (h.follow - head), h.p);
    });
  }

  // She turns her head and eyes toward the cursor. How far is a tanh of the distance over `reach`,
  // so a cursor beside her turns her a little and one across the screen does not snap her round
  // further than the rig allows. It eases there at `speed` per second, and back to straight ahead
  // when the source has nothing to say, as a gaze does and a pointer does not.
  function writeLook(dt) {
    const l = rig.look;
    if (!l) return;
    let at = null;
    if (lookSource) {
      try {
        at = lookSource();
      } catch (e) {
        if (lookSourceError === null) lookSourceError = String((e && e.message) || e);
      }
    }
    let x = 0;
    let y = 0;
    if (at && lookEye && Number.isFinite(at.x) && Number.isFinite(at.y)) {
      x = Math.tanh((at.x - lookEye.x) / l.reach);
      y = Math.tanh((at.y - lookEye.y) / l.reach);
    }
    const ease = 1 - Math.exp(-l.speed * dt);
    l.x += (x - l.x) * ease;
    l.y += (y - l.y) * ease;
    l.lanes.forEach(function (k) {
      k.p.value = within(k.p.value + k.by * (k.axis === "x" ? l.x : l.y), k.p);
    });
  }

  function look(fn) {
    lookSource = typeof fn === "function" ? fn : null;
    lookSourceError = null;
  }

  function within(v, p) {
    return Math.min(Math.max(v, p.low), p.high);
  }

  // A node's or a layer's own move, about its pivot: translated, rotated in degrees and scaled, each
  // by how far its parameters are from where the drive counts from.
  function local(part) {
    let x = 0;
    let y = 0;
    let turn = 0;
    let sx = 1;
    let sy = 1;
    part.drive.forEach(function (d) {
      const v = d.p.value - d.at;
      x += d.x * v;
      y += d.y * v;
      turn += d.rotate * v;
      sx += d.sx * v;
      sy += d.sy * v;
    });
    return new DOMMatrix()
      .translateSelf(part.pivot[0] + x, part.pivot[1] + y)
      .rotateSelf(turn)
      .scaleSelf(sx, sy)
      .translateSelf(-part.pivot[0], -part.pivot[1]);
  }

  // How much of a layer shows: 0 below `from`, 1 past `to`, a ramp between. `from` above `to` is the
  // other way round, which is how a closed eye fades in as the open one fades out.
  function shown(show) {
    if (!show) return 1;
    const t = (show.p.value - show.from) / (show.to - show.from);
    return Math.min(Math.max(t, 0), 1);
  }

  function draw() {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    // The context forgets this whenever the backing store is resized, so it is set where it is used.
    ctx.imageSmoothingQuality = "high";
    const world = new Map();
    rig.nodes.forEach(function (n) {
      world.set(n.name, (n.parent ? world.get(n.parent) : fit).multiply(local(n)));
    });
    // Every layer's place, hidden ones too, because a layer is clipped to its mask wherever the
    // mask is this frame, whether or not the mask is showing.
    const placed = rig.layers.map(function (l) {
      return (l.node ? world.get(l.node) : fit).multiply(local(l));
    });
    rig.layers.forEach(function (l, i) {
      const alpha = shown(l.show);
      if (!(alpha > 0)) return;
      if (l.clip >= 0) {
        drawClipped(l, placed[i], rig.layers[l.clip], placed[l.clip], alpha);
        return;
      }
      ctx.setTransform(placed[i]);
      ctx.globalAlpha = alpha;
      ctx.drawImage(l.image, l.x, l.y);
    });
    ctx.globalAlpha = 1;
  }

  // A clipped layer shows only where its mask is opaque. The iris moves and the white it is clipped
  // to does not, so looking aside takes the iris under the corners of the eye instead of past them.
  // The two are put together off screen at full strength and the result is laid on at the layer's
  // own alpha.
  function drawClipped(l, at, mask, maskAt, alpha) {
    const c = clipCtx;
    c.globalCompositeOperation = "source-over";
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.clearRect(0, 0, clipCanvas.width, clipCanvas.height);
    c.imageSmoothingQuality = "high";
    c.setTransform(maskAt);
    c.drawImage(mask.image, mask.x, mask.y);
    c.globalCompositeOperation = "source-in";
    c.setTransform(at);
    c.drawImage(l.image, l.x, l.y);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = alpha;
    ctx.drawImage(clipCanvas, 0, 0);
  }

  // What the rig declares, which is the input the binding step needs (Docs/DESIGN_Pet.md 1.3).
  function inventory() {
    if (!rig) return null;
    return { layers: rig.layers.length, params: params.length, nodes: rig.nodes.length };
  }

  // Every parameter by index, as the last frame left it.
  //
  // By index and not by id because the question here is not "what is ParamAngleX doing", it is "is
  // anything moving at all", and an index sweep is the same measurement for any rig.
  function parameters() {
    if (!rig) return null;
    return params.map(function (p) {
      return p.value;
    });
  }

  // Whether the idle is driving the figure, measured over a real interval. Expected is not measured,
  // which is the whole reason this function exists: if it comes back `moved: 0` then the figure is
  // on screen and dead.
  async function sampleMotion(ms) {
    const before = parameters();
    if (!before) return null;
    await new Promise(function (r) {
      setTimeout(r, ms);
    });
    const after = parameters();
    let moved = 0;
    let largest = 0;
    for (let i = 0; i < before.length; i++) {
      const d = Math.abs(after[i] - before[i]);
      // 1e-4 and not 0: these are floats driven by an interpolator, so an exactly equal pair is
      // the thing worth noticing, not a near-equal one.
      if (d > 1e-4) moved++;
      if (d > largest) largest = d;
    }
    return { parameters: before.length, moved: moved, largest: +largest.toFixed(4), overMs: ms };
  }

  // The mouth.
  //
  // The page hands over a function that says how far the mouth should open right now, as a number
  // in 0..1 while something is speaking and null while nothing is. This file does the rest: it
  // is the only one that knows which parameter that is and when in the frame it may be written.
  function mouth(fn) {
    mouthSource = typeof fn === "function" ? fn : null;
    mouthSourceError = null;
  }

  // Resolve the mouth parameter once. The id is the rig's own `lipSync`, so the parameter the voice
  // opens is named by the file that draws the mouth, as Mao's `LipSync` group named `ParamA`
  // (Docs/DESIGN_Pet.md 4.3.1). When the rig does not declare it the mouth stays unwired and
  // `status().mouth.failure` says why: the mouth not moving has to be a line someone can read and
  // not a clean console.
  function wireMouth() {
    const id = rig.lipSync;
    mouthWiring.id = id || null;
    const param = byId.get(id);
    if (!param) {
      mouthWiring.why = "the rig has no parameter " + id + " (" + params.length + " declared)";
      return;
    }
    mouthWiring.index = param.index;
    mouthWiring.low = param.low;
    mouthWiring.high = param.high;
    mouthWiring.why = null;
  }

  // The one place the mouth is written, once per frame, last before the figure is drawn.
  //
  // While nothing is speaking the source answers null or undefined and this writes nothing, so the
  // mouth keeps what the idle gave it. The mouth belongs to the voice only while there is a voice.
  //
  // The source runs inside the frame. A throw from it is caught, nothing is written that frame, and
  // the first failure is kept in `mouthSourceError`: a flag and not a log, because this runs at the
  // display rate. Returns what it wrote, for the probe.
  function writeMouth() {
    if (!mouthSource || mouthWiring.why !== null) return undefined;
    let level;
    try {
      level = mouthSource();
    } catch (e) {
      noteMouthSourceError(String((e && e.message) || e));
      return undefined;
    }
    if (level === null || level === undefined) return undefined;
    // A number the parameter cannot hold would reach the drawing as NaN and take the frame with it.
    if (typeof level !== "number" || !Number.isFinite(level)) {
      noteMouthSourceError("the source answered " + String(level) + ", not a number");
      return undefined;
    }
    // The source's contract is 0..1 and `opening` is only defined there, so a source that answers
    // outside it is brought back first. Then the parameter's own range, which is the rig's.
    const given = Math.min(Math.max(level, 0), 1);
    const value = Math.min(Math.max(opening(given), mouthWiring.low), mouthWiring.high);
    params[mouthWiring.index].value = value;
    return value;
  }

  // How far the mouth opens for a given loudness.
  //
  // The envelope is RMS amplitude, and RMS amplitude of speech sits low: measured over the six
  // clips in the shipped bank, the mean frame is 0.14 to 0.36 and only 10 to 30 per cent of frames
  // are above a half. Written straight to the parameter that is a mouth which stays nearly shut
  // through a clip, opens fully for a tenth of a second and reads as not moving at all. That is
  // what it was reported as on 2026-11-05, and instrumenting the real window found the chain
  // working and the openings that small.
  //
  // So the loudness is measured data and this is the face's own reading of it. The square root is
  // the mildest curve that lifts the body of the distribution (the same six clips come out at 0.24
  // to 0.50) while leaving silence at nothing and a peak at wide open, and it has no constant in it
  // to drift. A decibel mapping was rendered beside it and lifts near-silence into a twitch.
  //
  // It belongs here and not in the envelope, because the envelope is also what Unreal's face reads
  // and that face is a solver with its own idea of a mouth (VRSpeechFace.h). One measurement, two
  // readings of it.
  function opening(level) {
    return Math.sqrt(level);
  }

  // The gestures.
  //
  // The page hands over a function that says where the answer that is sounding has got to, as
  // `{ actions, at }`: the plan Rust made for that clip and the seconds since the clip started, on
  // the same audio clock the mouth reads. Null while nothing with a plan is sounding. Everything
  // else was decided in Rust (Docs/DESIGN_Pet.md 4): which cues play, when, and how far, so this
  // file only blends.
  function gestures(fn) {
    gestureSource = typeof fn === "function" ? fn : null;
    gestureSourceError = null;
  }

  function wireGestures() {
    swayLanes = lanesOfSway();
    gestureWiring.why = null;
  }

  // How much of a lane is on at `t` seconds from its stroke: a rise over `rise` before it, held for
  // `hold` after it, and a fall that ends `fall` after it, with `fall` counted from the stroke as in
  // VRBodyLanguageComponent.cpp:113-129. Both ramps are half a cosine, so a joint gathers speed and
  // settles instead of starting and stopping at full speed, which is what read as mechanical in the
  // straight ramps the C++ uses. A nod (rise 0.3, no hold, fall 0.3) comes out as the C++ nod itself,
  // `0.5 * (1 + cos(PI * t / NodHalf))` (VRBodyLanguageComponent.cpp:203).
  function weight(t, rise, hold, fall) {
    if (t > -rise && t <= 0) return 0.5 * (1 - Math.cos(Math.PI * (1 + t / rise)));
    if (t > 0 && t <= hold) return 1;
    if (t > hold && t < fall) return 0.5 * (1 + Math.cos(Math.PI * (t - hold) / (fall - hold)));
    return 0;
  }

  // The plan's lanes with each parameter's index and range. A lane naming a parameter the rig does
  // not declare is refused here, said in `status` and never written.
  function lanesOf(actions) {
    if (planLanes.actions === actions) return planLanes.lanes;
    const lanes = [];
    actions.forEach(function (a) {
      if (!a || !a.action || a.action.do !== "driveParams") return;
      a.action.lanes.forEach(function (l) {
        const lag = l.lag === undefined ? 0 : l.lag;
        const numbers = [a.at, l.to, l.rise, l.hold, l.fall, lag];
        if (!numbers.every(Number.isFinite)) {
          refusedLanes.add(l.id + ": not all numbers");
          return;
        }
        const param = byId.get(l.id);
        if (!param) {
          refusedLanes.add(l.id + ": the rig has no such parameter (" + params.length + " declared)");
          return;
        }
        lanes.push({
          index: param.index,
          low: param.low,
          high: param.high,
          at: a.at + lag,
          to: l.to,
          relative: l.relative === true,
          rise: l.rise,
          hold: l.hold,
          fall: l.fall,
        });
      });
    });
    planLanes = { actions: actions, lanes: lanes };
    return lanes;
  }

  // The one place the gestures are written, once per frame, straight after the idle.
  //
  // Each parameter is blended from what the idle gave it this frame: `idle + W * (to - idle)`, or
  // `idle + W * to` for a relative lane, so the idle keeps breathing through a gesture and takes the
  // parameter back as W falls. A parameter with W at 0 and no sway is not written at all, so outside
  // an answer the idle is the only writer, the way silence leaves the mouth to it. The value stays
  // inside the parameter's own range.
  //
  // This writes on every frame a lane is on, because the idle sets every parameter again at the
  // start of the next one; a throw from the source is caught for the reason `writeMouth` gives.
  function writeGestures() {
    if (!gestureSource) return;
    let now;
    try {
      now = gestureSource();
    } catch (e) {
      noteGestureSourceError(String((e && e.message) || e));
      return;
    }
    if (now === null || now === undefined) return;
    if (!Array.isArray(now.actions) || !Number.isFinite(now.at)) {
      noteGestureSourceError("the source answered something that is not { actions, at }");
      return;
    }
    // Every live lane of a parameter takes part, each by its own W. Letting the strongest one win
    // outright snapped the head 30 units in one frame when two glances crossed (measured
    // 2026-11-05, ParamAngleX -16.7 to +13.3 at 2.62 s), because the winner changes in a single
    // frame while each W only changes a little. Here the absolute targets are averaged by W and
    // reached by the largest W, and relative lanes add up, so the value moves only as fast as the
    // weights do.
    const live = new Map();
    lanesOf(now.actions).forEach(function (lane) {
      const w = weight(now.at - lane.at, lane.rise, lane.hold, lane.fall);
      if (!(w > 0)) return;
      let p = live.get(lane.index);
      if (!p) live.set(lane.index, (p = { low: lane.low, high: lane.high, w: 0, reach: 0, pull: 0, toward: 0, add: 0 }));
      p.w = Math.max(p.w, w);
      if (lane.relative) {
        p.add += w * lane.to;
      } else {
        p.reach = Math.max(p.reach, w);
        p.pull += w;
        p.toward += w * lane.to;
      }
    });
    const sway = swayOf(now);
    sway.forEach(function (s, index) {
      if (!live.has(index)) live.set(index, { low: s.low, high: s.high, w: 0, reach: 0, pull: 0, toward: 0, add: 0 });
    });
    live.forEach(function (p, index) {
      const idle = params[index].value;
      const drift = sway.has(index) ? sway.get(index).offset : 0;
      const target = p.pull > 0 ? p.toward / p.pull : idle;
      const value = idle + p.reach * (target - idle) + p.add + (1 - p.w) * drift;
      params[index].value = Math.min(Math.max(value, p.low), p.high);
    });
  }

  // While an answer with a plan is sounding, the arms drift a little and slowly, so they are not
  // held still between gestures; the idle moves them by a unit and this adds about twice as much
  // again. Each joint has its own period, so the arms never move in step. It fades in and out over
  // `SWAY_FADE` at the clip's ends, and a gesture on the same joint takes it over by its W: the
  // drift is added at `1 - W`. Nothing here is decided per reply, so it is the page's and not the
  // plan's, as the idle is.
  const SWAY = [
    { id: "ParamArmRA01", size: 2, period: 3.3, phase: 0 },
    { id: "ParamArmLA01", size: 2, period: 3.9, phase: 1.7 },
  ];
  const SWAY_FADE = 0.8;
  let swayLanes = [];

  // The sway's parameters with their indices and ranges, looked up once at mount and refused as a
  // plan's lanes are when the rig lacks one.
  function lanesOfSway() {
    const lanes = [];
    SWAY.forEach(function (s) {
      const param = byId.get(s.id);
      if (!param) {
        refusedLanes.add(s.id + ": the rig has no such parameter (" + params.length + " declared), so it does not sway");
        return;
      }
      lanes.push(Object.assign({ index: param.index, low: param.low, high: param.high }, s));
    });
    return lanes;
  }

  // Parameter index to `{ offset, low, high }` at `now`, or nothing when the source does not say how
  // long the clip is.
  function swayOf(now) {
    const out = new Map();
    if (!Number.isFinite(now.length)) return out;
    const edge = Math.min(now.at, now.length - now.at) / SWAY_FADE;
    if (!(edge > 0)) return out;
    const fade = edge >= 1 ? 1 : 0.5 * (1 - Math.cos(Math.PI * edge));
    swayLanes.forEach(function (s) {
      const offset = fade * s.size * Math.sin((2 * Math.PI * now.at) / s.period + s.phase);
      out.set(s.index, { offset: offset, low: s.low, high: s.high });
    });
    return out;
  }

  function noteGestureSourceError(why) {
    if (gestureSourceError !== null) return;
    gestureSourceError = why;
    console.error("gesture source: " + why);
  }

  function noteMouthSourceError(why) {
    if (mouthSourceError !== null) return;
    mouthSourceError = why;
    console.error("mouth source: " + why);
  }

  // Proves what DESIGN_Pet 4.3 asks for: the parameter really changed between frames, and the frame
  // that was drawn holds what was given. It installs a source of its own for the interval, a ramp
  // that is different on every frame, and puts the page's source back afterwards. The page's source
  // is out of the loop for that long, which is why this runs in the report and not while anyone is
  // speaking.
  //
  // The read-back is taken after the figure is drawn, so a writer that ran after the mouth and
  // changed it would show up here as a mismatch.
  async function sampleMouth(ms) {
    const base = { param: mouthWiring.id, index: mouthWiring.index, overMs: ms };
    if (mouthWiring.why !== null) {
      return Object.assign(base, { frames: 0, gave: [], saw: [], mismatches: 0, span: 0, error: mouthWiring.why });
    }
    const gave = [];
    const saw = [];
    const previous = mouthSource;
    // 0, 0.1 ... 1.0 and back down. Neighbouring frames never repeat, so a mouth stuck at one value
    // cannot match it by luck.
    mouthSource = function () {
      const step = gave.length % 20;
      return (step <= 10 ? step : 20 - step) / 10;
    };
    mouthTap = function (value) {
      gave.push(value);
      saw.push(params[mouthWiring.index].value);
    };
    try {
      await new Promise(function (r) {
        setTimeout(r, ms);
      });
    } finally {
      mouthSource = previous;
      mouthTap = null;
    }
    let mismatches = 0;
    for (let i = 0; i < saw.length; i++) {
      // Written as "not within" and not as "further than", because NaN is further than nothing.
      // Measured 2026-11-05: a deliberately broken build whose clamp produced NaN on every frame
      // wrote NaN 41 times and this counter said 0, because `Math.abs(NaN - NaN) > 1e-4` is false.
      // A probe that cannot go red on every frame being wrong is not a probe.
      if (!(Math.abs(saw[i] - gave[i]) <= 1e-4)) mismatches++;
    }
    const round = function (v) {
      return +v.toFixed(4);
    };
    return Object.assign(base, {
      frames: saw.length,
      // Only the first 24 of each are printed, which is more than one pass of the ramp.
      gave: gave.slice(0, 24).map(round),
      saw: saw.slice(0, 24).map(round),
      mismatches: mismatches,
      span: saw.length ? round(Math.max.apply(null, saw) - Math.min.apply(null, saw)) : 0,
      range: [mouthWiring.low, mouthWiring.high],
    });
  }

  // How many pixels the character actually put on the canvas.
  //
  // This reads the canvas the window composites. A count of zero while `state` is `ready` is the
  // failure this exists for, and it is the one a try/catch cannot see, because nothing throws when
  // a figure loads and draws nothing.
  function countOpaque() {
    const px = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    let opaque = 0;
    for (let i = 3; i < px.length; i += 4) {
      if (px[i] > 8) opaque++;
    }
    return { opaque: opaque, sampled: canvas.width * canvas.height, resolution: window.devicePixelRatio || 1 };
  }

  function drawn() {
    if (state !== State.Ready) return null;
    try {
      // The control. A plain filled square goes on the canvas and the pixels are counted with it
      // and without it. Without this, a zero for the figure means either `the figure drew nothing`
      // or `this whole read-back is broken`, and those two have completely different fixes.
      //
      // Drawn here rather than waiting for the next frame, so neither number can be about when the
      // read happened.
      draw();
      const alone = countOpaque();
      const side = Math.round(10 * alone.resolution);
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.fillStyle = "#ff00ff";
      ctx.fillRect(0, 0, side, side);
      const withFlag = countOpaque();
      draw();
      // 10 CSS pixels square at this resolution. Reported rather than asserted, because the point
      // is to read both numbers and see which one is wrong.
      return {
        opaquePixels: alone.opaque,
        sampled: alone.sampled,
        control: { opaquePixels: withFlag.opaque - alone.opaque, expected: side * side },
      };
    } catch (e) {
      return { error: String((e && e.message) || e) };
    }
  }

  // A PNG of the canvas, so a human can look at the result instead of trusting a pixel count. A
  // count cannot tell a character apart from a smear.
  //
  // The whole canvas and not a crop of the figure, because the canvas is exactly the rectangle the
  // hit test covers: a picture of it shows both whether the character drew and whether it sits
  // where the window thinks it does.
  function shot() {
    if (state !== State.Ready) return null;
    try {
      return canvas.toDataURL("image/png");
    } catch (e) {
      return null;
    }
  }

  function status() {
    return {
      state: state,
      error: error,
      renderer: "canvas2d",
      model: model,
      runtimes: { canvas2d: ctx ? "present" : null },
      inventory: inventory(),
      mouth: {
        param: mouthWiring.id,
        index: mouthWiring.index,
        wired: mouthWiring.why === null,
        failure: mouthWiring.why,
        sourceError: mouthSourceError,
      },
      gestures: {
        wired: gestureWiring.why === null,
        failure: gestureWiring.why,
        refused: Array.from(refusedLanes),
        sourceError: gestureSourceError,
      },
      look: {
        wired: Boolean(rig && rig.look),
        following: lookSource !== null,
        sourceError: lookSourceError,
      },
    };
  }

  return {
    State: State,
    mount: mount,
    status: status,
    drawn: drawn,
    sampleMotion: sampleMotion,
    mouth: mouth,
    sampleMouth: sampleMouth,
    gestures: gestures,
    look: look,
    shot: shot,
  };
})();
