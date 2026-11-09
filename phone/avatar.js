// Avatar state machine shared by the phone page (phone/app.js) and the desktop
// pet (src/pet/). Feed it every WebSocket message with handle(msg); it calls
// onChange(state) when the avatar should switch animation.
//
// Priority, highest first:
//   1. the reply's emotion, for REPLY_HOLD_MS after a reply
//   2. "talking" while a reply streams in
//   3. "searching" while a web lookup runs, else "thinking" while waiting for the first token
//   4. "sleeping" when the user is away
//   5. the current activity (e.g. "coding"), once screen awareness exists
//   6. "idle"
//
// gifFor() turns a state into a file name from gifs/manifest.json, falling
// back step by step (excited -> happy -> idle -> default) for missing art.

export const REPLY_HOLD_MS = 8000;

const FALLBACK = {
  excited: "happy",
  happy: "idle",
  sad: "idle",
  neutral: "idle",
  thinking: "idle",
  searching: "thinking",
  talking: "idle",
  sleeping: "idle",
};

export function gifFor(manifest, state) {
  const states = (manifest && manifest.states) || {};
  const seen = new Set();
  for (let s = state; s && !seen.has(s); s = FALLBACK[s] || "idle") {
    if (states[s]) return states[s];
    seen.add(s);
  }
  return states.idle || (manifest && manifest.default) || null;
}

export function createAvatar(onChange) {
  const st = {
    thinking: false,
    searching: false,
    talking: false,
    reply: null,
    replyUntil: 0,
    away: false,
    activity: null,
  };
  let current = null;
  let timer = null;

  function compute() {
    if (st.reply && Date.now() < st.replyUntil) return st.reply;
    if (st.talking) return "talking";
    if (st.searching) return "searching";
    if (st.thinking) return "thinking";
    if (st.away) return "sleeping";
    if (st.activity) return st.activity;
    return "idle";
  }

  function update() {
    const next = compute();
    if (next !== current) {
      current = next;
      onChange(next);
    }
  }

  const avatar = {
    get state() {
      return current ?? compute();
    },
    thinking() {
      Object.assign(st, { thinking: true, searching: false, talking: false, reply: null });
      update();
    },
    searching() {
      Object.assign(st, { thinking: true, searching: true, talking: false, reply: null });
      update();
    },
    token() {
      Object.assign(st, { thinking: false, searching: false, talking: true, reply: null });
      update();
    },
    reply(emotion) {
      Object.assign(st, {
        thinking: false,
        searching: false,
        talking: false,
        reply: emotion || "neutral",
        replyUntil: Date.now() + REPLY_HOLD_MS,
      });
      clearTimeout(timer);
      timer = setTimeout(update, REPLY_HOLD_MS + 20);
      update();
    },
    /** "away" puts it to sleep; "active" wakes it; anything else is an activity */
    setState(state) {
      if (state === "away") st.away = true;
      else if (state === "active") st.away = false;
      else st.activity = state || null;
      update();
    },
    /** Handles one message from the companion app's WebSocket */
    handle(msg) {
      switch (msg.type || "reply") {
        case "user":
        case "thinking":
          avatar.thinking();
          break;
        case "tool":
          avatar.searching();
          break;
        case "token":
          if (!st.talking) avatar.token();
          break;
        case "reply":
          avatar.reply(typeof msg.emotion === "string" ? msg.emotion.toLowerCase() : "neutral");
          break;
        case "state":
          avatar.setState(msg.state);
          break;
      }
    },
  };
  return avatar;
}
