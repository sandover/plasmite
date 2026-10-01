// Purpose: Exact protocol sequence IDs, status notices, and shared top bar controls.
// Role: Loaded by each page before its own script.
// Invariants: User data keeps JSON number semantics; pool names and paths enter the DOM as text.
(() => {
  const maxSequence = 18446744073709551615n;
  const sequence = (text) => {
    const digits = String(text).replace(/^0+/, "");
    if (!digits.length || digits.length > 20 || !/^[0-9]+$/.test(digits) ||
      (digits.length === 20 && digits > String(maxSequence))) return null;
    return BigInt(digits);
  };

  // JSON numbers round u64 sequence IDs in browsers. Read their original tokens,
  // only at protocol paths; data and meta keep ordinary JSON number semantics.
  // This works without the newer JSON reviver context.source API.
  function parseJSON(source) {
    const parsed = JSON.parse(source); // Malformed input cannot stall the scanner.
    const matches = [...source.matchAll(/"(?:\\[\s\S]|[^"\\])*"|[{}\[\]:,]|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null/g)];
    const tokens = matches.map((match) => match[0]);
    let at = 0;
    const edits = [];
    const protocolPath = (path) => {
      if (path[0] === "pool" || path[0] === "message") return path.slice(1);
      if (path[0] === "pools" && typeof path[1] === "number") return path.slice(2);
      return path;
    };
    const isSequence = (path) => {
      const p = protocolPath(path);
      return (p.length === 1 && p[0] === "seq" && (path.length === 1 || path[0] === "message")) ||
        (p.length === 2 && ((p[0] === "bounds" && (p[1] === "oldest" || p[1] === "newest")) ||
          (p[0] === "ring" && p[1] === "first")) && path[0] !== "message");
    };
    const enters = (path) => {
      if (!path.length) return true;
      if (path.length === 1 && ["pool", "message", "pools"].includes(path[0])) return true;
      if (path.length === 2 && path[0] === "pools" && typeof path[1] === "number") return true;
      const p = protocolPath(path);
      return p.length === 1 && ["bounds", "ring"].includes(p[0]) && path[0] !== "message";
    };
    function visit(path) {
      const token = tokens[at++];
      if (token === "{" || token === "[") {
        if (!enters(path)) {
          let depth = 1;
          while (depth) {
            const t = tokens[at++];
            if (t === "{" || t === "[") depth++;
            if (t === "}" || t === "]") depth--;
          }
          return;
        }
        let index = 0;
        const end = token === "{" ? "}" : "]";
        while (tokens[at] !== end) {
          const key = token === "{" ? JSON.parse(tokens[at++]) : index++;
          if (token === "{") at++; // colon
          visit([...path, key]);
          if (tokens[at] === ",") at++;
        }
        at++;
      } else if (isSequence(path)) {
        if (/^[0-9]+$/.test(token)) edits.push(matches[at - 1]);
      }
    }
    visit([]);
    if (!edits.length) return parsed;
    for (const edit of edits.reverse()) {
      source = source.slice(0, edit.index) + '"' + edit[0] + '"' + source.slice(edit.index + edit[0].length);
    }
    const value = JSON.parse(source);
    const exact = (object, key) => {
      if (object && typeof object[key] === "string" && /^[0-9]+$/.test(object[key])) object[key] = BigInt(object[key]);
    };
    const pool = (object) => {
      exact(object?.bounds, "oldest"); exact(object?.bounds, "newest"); exact(object?.ring, "first");
    };
    exact(value, "seq"); exact(value.message, "seq");
    pool(value); pool(value.pool);
    if (Array.isArray(value.pools)) value.pools.forEach(pool);
    return value;
  }

  window.PlasmiteUI = {
    parseJSON, sequence,
    cursor: (next) => next > maxSequence ? null : String(next),
    tailStart: (bounds, count) => {
      if (typeof bounds.newest !== "bigint") return null;
      const start = bounds.newest - BigInt(count) + 1n;
      return start > (bounds.oldest || 1n) ? start : (bounds.oldest || 1n);
    },
    messageJSON: (message) => JSON.stringify({ ...message, seq: String(message.seq) }, null, 2)
      .replace(/^  "seq": "([0-9]+)"/m, '  "seq": $1'),
    loginURL: () => "/ui?next=" + encodeURIComponent(location.pathname + location.search +
      (sequence(location.hash.slice(1)) != null ? location.hash : "")),
  };

  let noticeTimer;
  window.showNotice = (text, persistent = false) => {
    let notice = document.getElementById("ui-notice");
    if (!notice) {
      notice = document.createElement("div");
      notice.id = "ui-notice";
      notice.setAttribute("role", "status");
      document.body.append(notice);
    }
    clearTimeout(noticeTimer);
    notice.textContent = text;
    notice.hidden = !text;
    if (!persistent) noticeTimer = setTimeout(() => { notice.hidden = true; }, 8000);
  };

  const css = `
    #ui-notice { position: fixed; bottom: 16px; left: 16px; max-width: min(560px, calc(100vw - 32px));
      z-index: 30; padding: 10px 14px; border: 1px solid var(--muted); border-radius: 6px;
      background: #162026; color: var(--text); font: 13px/1.5 var(--sans); overflow-wrap: anywhere; box-shadow: 0 4px 20px #0006; }
    #ui-notice[hidden] { display: none; }
    /* The served directory: named "Serving", parent folders muted, its own name bright,
       breaking only after a slash. A click copies it, as its copy mark says. */
    .topbar { flex-wrap: wrap; row-gap: 4px; }
    .topbar .dir {
      display: inline-flex; gap: 7px; align-items: baseline;
      margin-left: auto; min-width: 0; padding: 1px 6px; border: 0; border-radius: 5px; background: none;
      color: var(--muted); font: 12px var(--mono); text-align: right; overflow-wrap: anywhere; cursor: pointer;
    }
    .topbar .dir strong { color: var(--text); font-weight: 600; }
    .topbar .dir .what { color: var(--muted); font: 11px var(--sans); white-space: nowrap; }
    .topbar .dir .path { min-width: 0; }
    .topbar .dir .mark { align-self: center; display: inline-flex; color: var(--muted); opacity: .7; }
    .topbar .dir .mark svg, .jump-panel svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
    .topbar .dir:hover, .topbar .dir:focus-visible { background: #182228; outline: none; }
    .topbar .dir:hover .mark, .topbar .dir:focus-visible .mark { opacity: 1; }
    .topbar .dir.done .mark { color: var(--ok); opacity: 1; }
    .topbar .dir ~ .status, .topbar .dir ~ .actions { margin-left: 0; }

    /* The jump to a pool: g, a few letters, Enter. */
    .jump-shade { position: fixed; inset: 0; z-index: 20; display: grid; justify-items: center; align-items: start; padding-top: 12vh; background: #0008; }
    .jump-panel {
      width: min(440px, calc(100vw - 32px)); background: #162026; border: 1px solid #3f4950; border-radius: 10px;
      box-shadow: 0 16px 40px #000a; overflow: hidden; font: 14px var(--mono);
    }
    .jump-panel input {
      width: 100%; padding: 10px 14px; border: 0; border-bottom: 1px solid #2d373e; background: none;
      color: var(--text); font: 15px var(--mono); outline: none;
    }
    .jump-panel ol { list-style: none; margin: 0; padding: 4px; max-height: 50vh; overflow: auto; }
    .jump-panel li { padding: 5px 10px; border-radius: 6px; color: var(--accent); cursor: pointer; }
    .jump-panel li[aria-selected="true"] { background: #182228; box-shadow: inset 0 0 0 1px var(--ok); }
    .jump-panel li mark { background: none; color: var(--bright, var(--text)); text-decoration: underline; text-underline-offset: 3px; }
    .jump-panel .none { padding: 8px 12px; color: var(--muted); font: 13px var(--sans); }
    .jump-panel .keys { padding: 6px 12px 8px; border-top: 1px solid #2d373e; color: #72787d; font: 11px var(--sans); }
    .jump-panel kbd {
      display: inline-block; min-width: 1.6em; padding: 0 4px; border: 1px solid #2d373e; border-bottom-width: 2px;
      border-radius: 4px; color: var(--accent); font: 11px/1.4 var(--mono); text-align: center;
    }
    @media (prefers-reduced-motion: no-preference) {
      .jump-panel { animation: jump-in .12s ease-out; }
      @keyframes jump-in { from { opacity: 0; transform: translateY(-6px); } }
    }`;
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);

  const copyIcon = '<svg viewBox="0 0 16 16"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5"/></svg>';
  const doneIcon = '<svg viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7"/></svg>';

  // The served directory, shown once, in the top bar. Copying takes the path itself.
  window.showDirectory = (dir) => {
    const button = document.getElementById("dir");
    if (!button || !dir || button.dataset.dir === dir) return;
    button.dataset.dir = dir;
    const cut = Math.max(dir.lastIndexOf("/"), dir.lastIndexOf("\\")) + 1;
    button.querySelector(".parent").replaceChildren(...dir.slice(0, cut).split(/(?<=[\\/])/)
      .flatMap((part) => [part, document.createElement("wbr")]));
    button.querySelector("strong").textContent = dir.slice(cut);
    button.querySelector(".mark").innerHTML = copyIcon;
    button.title = "Copy " + dir;
    button.setAttribute("aria-label", "Serving " + dir + ". Copy the path");
    button.hidden = false;
  };
  document.addEventListener("click", async (event) => {
    const button = event.target.closest && event.target.closest("#dir");
    if (!button) return;
    try {
      await navigator.clipboard.writeText(button.dataset.dir);
      button.classList.add("done");
      button.querySelector(".mark").innerHTML = doneIcon;
      setTimeout(() => { button.classList.remove("done"); button.querySelector(".mark").innerHTML = copyIcon; }, 1200);
    } catch (_) { showNotice("Could not copy. Copy this path manually: " + button.dataset.dir); }
  });

  // The jump: g opens a list of the pools; typing narrows it, matching anywhere in a
  // name; arrows move; Enter opens the pool's page; Esc closes.
  let shade = null;
  async function openJump() {
    if (shade) return;
    shade = document.createElement("div");
    shade.className = "jump-shade";
    shade.innerHTML = '<div class="jump-panel" role="dialog" aria-label="Jump to a pool">' +
      '<input type="text" aria-label="Pool name" placeholder="Jump to a pool" autocomplete="off" spellcheck="false" />' +
      '<ol role="listbox"></ol><div class="keys"><kbd>↑</kbd><kbd>↓</kbd> to choose · <kbd>Enter</kbd> to open · <kbd>Esc</kbd> to close</div></div>';
    document.body.append(shade);
    const input = shade.querySelector("input");
    const list = shade.querySelector("ol");
    let names = [];
    let at = 0;
    const shown = () => names.filter((name) => name.toLowerCase().includes(input.value.trim().toLowerCase()));
    const render = () => {
      const matches = shown();
      at = Math.max(0, Math.min(at, matches.length - 1));
      const needle = input.value.trim().toLowerCase();
      list.replaceChildren(...matches.map((name, i) => {
        const item = document.createElement("li");
        item.setAttribute("role", "option");
        item.setAttribute("aria-selected", String(i === at));
        const from = needle ? name.toLowerCase().indexOf(needle) : -1;
        if (from < 0) item.textContent = name;
        else item.append(name.slice(0, from), Object.assign(document.createElement("mark"), { textContent: name.slice(from, from + needle.length) }), name.slice(from + needle.length));
        item.addEventListener("click", () => go(name));
        return item;
      }));
      if (!matches.length) list.replaceChildren(Object.assign(document.createElement("li"), { className: "none", textContent: names.length ? "No pool matches." : "No pools." }));
      list.children[at]?.scrollIntoView({ block: "nearest" });
    };
    const go = (name) => { location.assign("/ui/pools/" + encodeURIComponent(name)); };
    input.addEventListener("input", () => { at = 0; render(); });
    input.addEventListener("keydown", (event) => {
      if (event.key === "Escape") { event.preventDefault(); closeJump(); }
      else if (event.key === "ArrowDown") { event.preventDefault(); at++; render(); }
      else if (event.key === "ArrowUp") { event.preventDefault(); at--; render(); }
      else if (event.key === "Enter") { event.preventDefault(); const name = shown()[at]; if (name) go(name); }
    });
    shade.addEventListener("click", (event) => { if (event.target === shade) closeJump(); });
    input.focus();
    try {
      const response = await fetch("/v0/ui/pools", { cache: "no-store" });
      if (response.ok) names = ((await response.json()).pools || []).map((pool) => pool.name).sort((a, b) => a.localeCompare(b));
    } catch (_) { /* The list stays empty; the dialog says so. */ }
    render();
  }

  function closeJump() {
    shade?.remove();
    shade = null;
  }

  document.addEventListener("keydown", (event) => {
    if (event.key !== "g" || shade || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.target instanceof Element && event.target.closest("input, textarea, select, [contenteditable]")) return;
    event.preventDefault();
    openJump();
  });
})();
