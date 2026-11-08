// The dialogue box, laid out like a visual novel (Docs/DESIGN_Pet.md 4.4.6). A paper box along the
// bottom of the window with her name on it, the choices stacked in the band above her head, the
// text field as a pill under the box, and the history as a sheet that opens over her and closes
// again from the box's own "›".
//
// It draws the turns it is handed and reports where it drew them. It does not know what anyone
// says, and it does not touch the character: it draws on no canvas, and `pet_page` fails if it
// ever does.
//
// A classic script and not a module, like character.js: there is no npm and no bundler, so the
// page reads `Chat` off `window`.
window.Chat = (function () {
  "use strict";

  // Three values and not two booleans, so "closed but the log is open" cannot be built.
  const Surface = { Hidden: "hidden", Bubble: "bubble", Log: "log" };
  const Who = { Character: "character", User: "user" };

  // What the "›" will do when pressed, said on the button for the cursor and a screen reader. The
  // glyph is the same in all three, as in the games this copies; its colour and its turn say which.
  const NEXT_PART = "下一段";
  const LOG_OPEN = "看前面的對話";
  const LOG_CLOSE = "收起紀錄";

  // One root per place the dialogue appears. The page owns where they sit, because where they
  // sit is a fact about the character's silhouette and this file has never seen the character.
  let roots = { bubble: null, choices: null, input: null, log: null };
  // Controls the page owns that sit in the text row, the microphone button today. The page wires
  // them; this file only gives them a place, so they share the row's hit rectangle.
  let tools = [];
  let names = { character: "桌寵", user: "你" };
  let surface = Surface.Bubble;

  // Every turn so far, oldest first. Page memory only: durable state has one owner and it lives
  // in Rust, which does not exist yet (Docs/DESIGN_Pet.md 4.4 and 7).
  const turns = [];
  // The latest turn the character spoke. Once the user answers it the box stays and the cards go,
  // so one question cannot be answered twice.
  //
  // `parts` is what the turn arrived as, one entry per piece, and `page` is the one the box shows.
  // The turn's `text` is the same words joined the way the core joins them, which is what the log
  // shows: the box pages through an answer, the history reads it whole.
  let current = null;
  const chosen = [];

  function node(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }

  // What the character says is outside text, and from the next unit on it is a model's. It is
  // always set as a text node, never parsed, and the shape is checked here where it enters.
  function parseTurn(raw) {
    if (!raw || (raw.who !== Who.Character && raw.who !== Who.User)) {
      throw new TypeError("turn.who must be \"" + Who.Character + "\" or \"" + Who.User + "\"");
    }
    if (typeof raw.text !== "string") throw new TypeError("turn.text must be a string");
    if (!Array.isArray(raw.choices)) throw new TypeError("turn.choices must be an array, empty for none");
    if (raw.who === Who.User && raw.choices.length) throw new TypeError("a user turn offers no choices");
    const choices = raw.choices.map(function (c) {
      if (!c || typeof c.label !== "string" || typeof c.send !== "string") {
        throw new TypeError("a choice needs a label string and a send string");
      }
      // The label is the button and `send` is what goes out. They are separate so a button can be
      // short without the sentence it stands for being cut to fit.
      return { label: c.label, send: c.send };
    });
    return { who: raw.who, text: raw.text, choices: choices };
  }

  function waiting() {
    return current !== null && current.page < current.parts.length - 1;
  }

  // The box: her icon and name, the page of the answer she is on, and the "›". The text scrolls
  // inside its own element so the name and the mark stay where they are however long the page is.
  //
  // A page starts at its first word. The reason a silent greeting is silent comes in as a piece of
  // its own that begins with the blank line which separates it in the log, and on a page of its own
  // that blank line would only push it down.
  function boxNode() {
    const b = node("div", "chat-box");
    const head = node("div", "chat-head");
    head.append(node("span", "chat-icon"), node("span", "chat-name", names.character));
    const next = node("button", "chat-next", "›");
    next.type = "button";
    const label = waiting() ? NEXT_PART : surface === Surface.Log ? LOG_CLOSE : LOG_OPEN;
    next.title = label;
    next.setAttribute("aria-label", label);
    if (waiting()) next.classList.add("waiting");
    if (surface === Surface.Log) next.classList.add("open");
    next.addEventListener("click", advance);
    b.append(head, node("div", "chat-say", current.parts[current.page].replace(/^\s+/, "")), next);
    return b;
  }

  function cardNode(choice) {
    const b = node("button", "chat-choice", choice.label);
    b.type = "button";
    b.addEventListener("click", function () {
      send(choice.send);
    });
    return b;
  }

  function logNode() {
    const l = node("div", "chat-log");
    turns.forEach(function (t) {
      const row = node("div", "chat-turn " + t.who);
      row.append(node("div", "chat-who", names[t.who]), node("div", "chat-say", t.text));
      l.append(row);
    });
    return l;
  }

  // The "›": the next page of her answer when one has come in and is waiting, and the history
  // otherwise. One control and not two, because in the box there is only ever one thing to go on to.
  function advance() {
    if (waiting()) {
      current.page += 1;
      render();
      return;
    }
    setSurface(surface === Surface.Log ? Surface.Bubble : Surface.Log);
  }

  // The typed reply and a pressed card are the same event with different wording, so they take
  // the same path out.
  //
  // The caret arrives here only when the user clicks the field, which the browser does by itself.
  // Nothing in this file asks for it. The dialogue redraws on every turn, so anything that claimed
  // the caret would be taking it out of whatever the user was typing in elsewhere, on a timer
  // (Docs/DESIGN_Pet.md 5.1). `pet_page` fails on the attribute and the method that would, and it
  // does not exempt comments, so this one cannot name them either.
  // Built once and then kept. Rebuilding it with everything else would throw away a half-typed
  // reply, and the caret with it, every time the character spoke.
  let inputEl = null;

  function inputRow() {
    if (inputEl) return inputEl;
    const row = node("div", "chat-input");
    const field = document.createElement("input");
    field.type = "text";
    field.className = "chat-field";
    field.placeholder = "跟我說點什麼";
    const submit = function () {
      const text = field.value.trim();
      if (!text) return;
      field.value = "";
      send(text);
    };
    // Enter is the only way a typed line goes out, so the Enter that picks a word in a Chinese
    // input method must not be it: that one arrives while the composition is still open.
    field.addEventListener("keydown", function (e) {
      if (e.key === "Enter" && !e.isComposing) submit();
    });
    row.append(field, ...tools);
    inputEl = row;
    return row;
  }

  // Rebuilds every root from the state above. Every state change ends here, so there is no
  // second path that could leave the screen disagreeing with `surface`.
  function render() {
    if (!roots.bubble) return;
    if (surface === Surface.Hidden) {
      roots.bubble.replaceChildren();
      roots.choices.replaceChildren();
      roots.input.replaceChildren();
      roots.log.replaceChildren();
      return;
    }

    roots.log.replaceChildren.apply(roots.log, surface === Surface.Log && turns.length ? [logNode()] : []);
    roots.bubble.replaceChildren.apply(roots.bubble, current ? [boxNode()] : []);

    // Not while the history is open. It covers the band the cards are in, and the paper is only as
    // opaque as the settings make it, so the cards would show through it as a second layer of
    // words. Closing the history brings them back, still unanswered.
    const cards = [];
    if (current && !current.answered && surface === Surface.Bubble) {
      current.turn.choices.forEach(function (c) {
        cards.push(cardNode(c));
      });
    }
    roots.choices.replaceChildren.apply(roots.choices, cards);

    // Attached, not replaced: putting the same node back would still take it out of the document
    // on the way, which is what loses the caret.
    if (roots.input.firstChild !== inputRow()) roots.input.replaceChildren(inputRow());

    // The newest line is the one that matters, and a node that is not attached has no height to
    // scroll to, so this waits until after the children are in.
    const log = roots.log.querySelector(".chat-log");
    if (log) log.scrollTop = log.scrollHeight;
  }

  function show(raw) {
    const turn = parseTurn(raw);
    turns.push(turn);
    if (turn.who === Who.Character) {
      current = { turn: turn, answered: false, parts: [turn.text], page: 0 };
    } else if (current) {
      current.answered = true;
    }
    render();
  }

  // The marks a piece of an answer can end on without a space after them. The core cuts an answer
  // at punctuation and joins the clauses it keeps with one space, so these are the Chinese marks of
  // `cue/align.rs::is_punctuation`: Chinese runs on with no space, and after anything else (a Latin
  // word, an ASCII mark, a cut that fell between clauses) one space is what the core would have put.
  const NO_SPACE_AFTER = "，。！？、；：…";

  function separator(before) {
    if (before === "") return "";
    return NO_SPACE_AFTER.indexOf(before.charAt(before.length - 1)) >= 0 ? "" : " ";
  }

  // Adds the next piece of an answer to what the character is saying. It becomes the next page
  // of the box, waiting behind the "›", and the end of her turn in the log. `current` only ever
  // holds a turn the character spoke, so "there is nobody to add to" is the null test, and the
  // user's reply marks the turn answered, so a piece that comes in after it is refused and nothing
  // is changed. The caller learns that from the return value.
  function append(text) {
    if (typeof text !== "string") throw new TypeError("append needs a string");
    if (!current || current.answered) return false;
    if (text === "") return true;
    const before = current.turn.text;
    current.turn.text = before + separator(before) + text;
    current.parts.push(text);
    render();
    return true;
  }

  // Her voice reaching a page turns to it, the way a visual novel's auto mode does. Only forward:
  // a user who paged ahead with the "›" is not pulled back, and a page that is not there yet, or a
  // turn the user has answered, is left alone.
  function turnTo(page) {
    if (!current || current.answered) return false;
    if (!Number.isInteger(page) || page <= current.page || page >= current.parts.length) return false;
    current.page = page;
    render();
    return true;
  }

  function send(text) {
    show({ who: Who.User, text: text, choices: [] });
    // A copy, so a callback that registers another does not change this round. One that throws
    // must not stop the others, and `console.error` is where the page collects what went wrong.
    chosen.slice().forEach(function (cb) {
      try {
        cb(text);
      } catch (e) {
        console.error(e);
      }
    });
  }

  function setSurface(s) {
    if (Object.values(Surface).indexOf(s) < 0) {
      throw new RangeError("surface must be one of " + Object.values(Surface).join(", ") + ", got " + String(s));
    }
    surface = s;
    render();
  }

  // What is drawn and can take the cursor, as fractions of the viewport. Fractions because the
  // hit test on the other side of the window has no DPI to convert with (Docs/DESIGN_Pet.md 0).
  //
  // One rectangle per element and not the box around all of them, so the gaps between cards stay
  // click-through. Nothing in any root while hidden, which is what returns the window to the
  // character alone.
  //
  // Each rectangle is cut to its root, because the root is what clips. A card laid out above a
  // scrolling container is invisible and still has a rectangle of its own, and reporting that
  // would hang a button nobody can see over whatever is behind it. Far enough out the rectangle
  // goes negative, and Rust rejects a whole report over one bad rectangle, which freezes every
  // region the window has until the next good one. Both were measured, with four cards and eight.
  function regions() {
    if (!roots.bubble) return [];
    const w = window.innerWidth;
    const h = window.innerHeight;
    const out = [];
    [roots.bubble, roots.choices, roots.input, roots.log].forEach(function (r) {
      const clip = r.getBoundingClientRect();
      Array.from(r.children).forEach(function (el) {
        const box = el.getBoundingClientRect();
        const x0 = Math.max(box.left, clip.left);
        const y0 = Math.max(box.top, clip.top);
        const x1 = Math.min(box.right, clip.right);
        const y1 = Math.min(box.bottom, clip.bottom);
        if (!(x1 - x0 > 0) || !(y1 - y0 > 0)) return;
        out.push({ x: x0 / w, y: y0 / h, w: (x1 - x0) / w, h: (y1 - y0) / h });
      });
    });
    return out;
  }

  function onChoose(cb) {
    if (typeof cb !== "function") throw new TypeError("onChoose needs a function");
    chosen.push(cb);
  }

  // `els` names the four places, and `labels` names the two sides in the box and the log. The page
  // knows where the character's silhouette leaves room and who the character is; this file is not
  // meant to know either. `extra` is the page's own controls for the text row, before the row is
  // built.
  function mount(els, labels, extra) {
    ["bubble", "choices", "input", "log"].forEach(function (k) {
      if (!els || !els[k]) throw new TypeError("mount needs an element for " + k);
    });
    roots = { bubble: els.bubble, choices: els.choices, input: els.input, log: els.log };
    names = Object.assign({}, names, labels);
    tools = Array.isArray(extra) ? extra.slice() : [];
    render();
  }

  return {
    Surface: Surface,
    Who: Who,
    mount: mount,
    // The names on the box and in the log, changed after mounting. The page mounts the box at once
    // and learns who the character is from Rust a moment later.
    rename: function (labels) {
      names = Object.assign({}, names, labels);
      render();
    },
    show: show,
    // The next piece of an answer that comes in parts, added as the next page of the box the
    // character is speaking. False, with nothing changed, when that box is not there to add to or
    // the user has answered it.
    append: append,
    // How many pages the box holds, so the page can name the one a piece it just added became.
    pages: function () {
      return current ? current.parts.length : 0;
    },
    turnTo: turnTo,
    surface: function () {
      return surface;
    },
    setSurface: setSurface,
    regions: regions,
    onChoose: onChoose,
    // A sentence that did not come from the field, a spoken one: the same path a typed one takes,
    // so it is shown as the user's and reaches every `onChoose` callback the same way.
    submit: send,
  };
})();
