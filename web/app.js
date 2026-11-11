const $ = id => document.getElementById(id);
let ws, term, fit;
const base = location.pathname.replace(/[^/]*$/, "");
const url = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + base + "ws";

function fail(text) { $("error").textContent = text; }

$("id").oninput = () => {
  // Spaced in threes as the app shows it.
  const d = $("id").value.replace(/\D/g, "").slice(0, 9);
  $("id").value = d.replace(/(\d{3})(?=\d)/g, "$1 ");
};
$("gateway").onclick = e => {
  e.preventDefault();
  const gate = $("signin").style.display === "none";
  $("signin").style.display = gate ? "" : "none";
  $("byid").style.display = gate ? "none" : "";
  $("gateway").textContent = gate ? "Connect by ID" : "Terminals on this gateway";
  (gate ? $("password") : $("id")).focus();
  fail("");
};
$("byid").onsubmit = e => {
  e.preventDefault();
  fail("");
  const id = $("id").value.replace(/\D/g, "");
  if (id.length !== 9) { fail("An ID is 9 digits."); return; }
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onerror = () => fail("The gateway could not be reached.");
  openClient($("id").value, id, $("idpw").value);
};

$("signin").onsubmit = e => {
  e.preventDefault();
  fail("");
  ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  ws.onopen = () => ws.send(JSON.stringify({ password: $("password").value }));
  ws.onerror = () => fail("The gateway could not be reached.");
  ws.onmessage = m => {
    if (typeof m.data !== "string") return;
    const v = JSON.parse(m.data);
    if (v.error) { fail(v.error); return; }
    if (v.hosts) {
      $("signin").style.display = "none";
      $("toggle").style.display = "none";
      $("pick").style.display = "";
      const sel = $("host");
      sel.textContent = "";
      for (const h of v.hosts) { const o = document.createElement("option"); o.textContent = h; sel.append(o); }
      sel.focus();
    }
  };
};

$("pick").onsubmit = e => {
  e.preventDefault();
  const host = $("host").value;
  chrome(host, "terminal");
  $("term").style.display = "block";
  term = new Terminal({ cursorBlink: true, fontSize: 14, scrollback: 5000,
    fontFamily: "'Cascadia Mono', 'DejaVu Sans Mono', Menlo, Consolas, monospace",
    theme: { background: "#0b1220" } });
  fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($("term"));
  fit.fit();
  const enc = new TextEncoder();
  ws.onmessage = m => {
    if (typeof m.data === "string") {
      const v = JSON.parse(m.data);
      if (v.error) term.write("\r\n\x1b[31m" + v.error + "\x1b[0m\r\n");
      if (v.end) term.write("\r\n\x1b[2m[" + v.end + "]\x1b[0m\r\n");
    } else {
      term.write(new Uint8Array(m.data));
    }
  };
  ws.onclose = () => term.write("\r\n\x1b[2m[closed]\x1b[0m\r\n");
  ws.send(JSON.stringify({ open: host, cols: term.cols, rows: term.rows }));
  term.onData(d => ws.readyState === 1 && ws.send(enc.encode(d)));
  term.onResize(s => ws.readyState === 1 && ws.send(JSON.stringify({ resize: [s.cols, s.rows] })));
  addEventListener("resize", () => fit.fit());
  term.focus();
};

function chrome(host, what) {
  $("card").style.display = "none";
  $("bar").style.display = "flex";
  $("title").textContent = host + " · " + what;
  document.title = host + " - SSH Remote Web";
}

// Keys as X keysyms, which the agent takes on every system.
const NAMED = { Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, Escape: 0xff1b, Delete: 0xffff, Home: 0xff50,
  ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54, PageUp: 0xff55, PageDown: 0xff56,
  End: 0xff57, Insert: 0xff63, ContextMenu: 0xff67, Pause: 0xff13, PrintScreen: 0xff61, CapsLock: 0xffe5 };
const SIDED = { Shift: [0xffe1, 0xffe2], Control: [0xffe3, 0xffe4], Alt: [0xffe9, 0xffea], Meta: [0xffeb, 0xffec], AltGraph: [0xfe03, 0xfe03] };
function keysym(e) {
  if (NAMED[e.key]) return NAMED[e.key];
  if (SIDED[e.key]) return SIDED[e.key][e.location === 2 ? 1 : 0];
  const f = /^F(\d{1,2})$/.exec(e.key);
  if (f) return 0xffbe + Number(f[1]) - 1;
  if ([...e.key].length === 1) {
    // With Ctrl or Alt the key's own letter, not what the layout made of it.
    let c = e.key.codePointAt(0);
    if ((e.ctrlKey || e.altKey) && /^Key[A-Z]$/.test(e.code)) c = e.code.charCodeAt(3) + (e.shiftKey ? 0 : 32);
    return c < 0x100 ? c : 0x01000000 + c;
  }
  return 0;
}

$("openScreen").onclick = () => {
  const host = $("host").value;
  showScreen(host, size => ({ open: host, screen: true, w: size[0], h: size[1] }));
};


// Draws JPEG pictures on `canvas`, the newest only, each when the last is done.
function drawer(canvas) {
  const ctx = canvas.getContext("2d");
  let busy = false, next = null;
  const draw = () => {
    if (!next) { busy = false; return; }
    busy = true;
    const blob = new Blob([next], { type: "image/jpeg" });
    next = null;
    createImageBitmap(blob).then(b => {
      if (canvas.width !== b.width || canvas.height !== b.height) { canvas.width = b.width; canvas.height = b.height; }
      ctx.drawImage(b, 0, 0);
      b.close();
      draw();
    }, () => draw());
  };
  return data => { next = data; if (!busy) draw(); };
}

// Pointer, buttons, wheel and keys on `canvas` sent as the gateway's JSON messages.
function screenInput(canvas, send) {
  // Pointer positions in the picture's pixels, past the letterbox around it.
  const point = e => {
    const r = canvas.getBoundingClientRect();
    const k = Math.min(r.width / canvas.width, r.height / canvas.height);
    const ox = (r.width - canvas.width * k) / 2, oy = (r.height - canvas.height * k) / 2;
    const x = Math.floor((e.clientX - r.left - ox) / k), y = Math.floor((e.clientY - r.top - oy) / k);
    return x >= 0 && y >= 0 && x < canvas.width && y < canvas.height ? [x, y] : null;
  };
  const BUTTON = [1, 2, 3, 8, 9];
  canvas.onpointermove = e => { const p = point(e); if (p) send({ move: p }); };
  canvas.onpointerdown = e => {
    canvas.focus();
    const p = point(e); if (!p) return;
    canvas.setPointerCapture(e.pointerId);
    send({ move: p }); send({ button: [BUTTON[e.button] || 1, 1] });
    e.preventDefault();
  };
  canvas.onpointerup = e => send({ button: [BUTTON[e.button] || 1, 0] });
  canvas.oncontextmenu = e => e.preventDefault();
  let wheel = [0, 0];
  canvas.onwheel = e => {
    e.preventDefault();
    const unit = e.deltaMode === 0 ? 40 : e.deltaMode === 1 ? 1 : 0.1;
    wheel[0] += e.deltaX / unit; wheel[1] += e.deltaY / unit;
    const sx = Math.trunc(wheel[0]), sy = Math.trunc(wheel[1]);
    if (sx || sy) { send({ wheel: [sx, sy] }); wheel = [wheel[0] - sx, wheel[1] - sy]; }
  };
  const held = new Set();
  canvas.onkeydown = e => {
    const k = keysym(e); if (!k) return;
    e.preventDefault(); held.add(k); send({ key: [k, 1] });
  };
  canvas.onkeyup = e => {
    const k = keysym(e); if (!k) return;
    e.preventDefault(); held.delete(k); send({ key: [k, 0] });
  };
  // Keys still down when the page loses focus come up, so none sticks on the host.
  canvas.onblur = () => { for (const k of held) send({ key: [k, 0] }); held.clear(); };
}

// The screen on the canvas; `first` is what to ask the gateway for, given the canvas's size.
function showScreen(host, first) {
  chrome(host, "screen");
  $("screen").style.display = "block";
  const canvas = $("canvas");
  const want = () => [Math.round(canvas.clientWidth * devicePixelRatio), Math.round(canvas.clientHeight * devicePixelRatio)];
  const send = v => ws.readyState === 1 && ws.send(JSON.stringify(v));
  const [w, h] = want();
  const show = drawer(canvas);
  let shown = false;
  // Refused before anything showed (a wrong ID or password): back to the form, saying why.
  const back = text => {
    $("screen").style.display = "none";
    $("bar").style.display = "none";
    $("card").style.display = "";
    document.title = "SSH Remote Web";
    fail(text);
  };
  ws.onmessage = m => {
    if (typeof m.data === "string") {
      const v = JSON.parse(m.data);
      if (v.status) $("title").textContent = v.status;
      if (v.screen) $("title").textContent = host + " · screen " + v.screen[0] + "×" + v.screen[1];
      if (v.error && !shown) { back(v.error); return; }
      if (v.error || v.end) $("title").textContent = host + " · " + (v.error || v.end);
      return;
    }
    shown = true;
    show(m.data);
  };
  ws.onclose = () => { if (shown) $("title").textContent = host + " · closed"; };
  const start = () => ws.send(JSON.stringify(first([w, h])));
  if (ws.readyState === 1) start(); else ws.onopen = start;
  screenInput(canvas, send);
  let t;
  addEventListener("resize", () => { clearTimeout(t); t = setTimeout(() => send({ view: want() }), 250); });
  canvas.focus();
}

$("leave").onclick = () => { if (ws) ws.close(); location.reload(); };

// ----- A computer by ID: terminal, screen and files on one connection --------------------------------

const MAX_TRANSFER = 2 * 1024 ** 3, CHUNK = 64 * 1024, WINDOW = 1024 * 1024;

function size(n) {
  if (n < 1024) return n + " B";
  const u = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return n.toFixed(n < 10 ? 1 : 0) + " " + u[i];
}

// A binary frame: type byte, then (for transfers) a little-endian u32 id, then the bytes.
function framed(type, id, bytes) {
  const head = id === null ? 1 : 5;
  const f = new Uint8Array(head + bytes.length);
  f[0] = type;
  if (id !== null) new DataView(f.buffer).setUint32(1, id, true);
  f.set(bytes, head);
  return f;
}

function openClient(label, id, password) {
  $("card").style.display = "none";
  $("client").style.display = "block";
  $("name").textContent = $("pillname").textContent = label;
  document.title = label + " - SSH Remote Web";
  const send = v => ws.readyState === 1 && ws.send(JSON.stringify(v));
  const state = (text, dot) => { $("state").textContent = text; $("dot").className = dot || ""; };
  let ready = false, view = "term";

  // The terminal, first.
  const term = new Terminal({ cursorBlink: true, fontSize: 14, scrollback: 5000,
    fontFamily: "'Cascadia Mono', 'DejaVu Sans Mono', Menlo, Consolas, monospace",
    theme: { background: "#0b1220" } });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($("cterm"));
  fit.fit();
  const enc = new TextEncoder();
  let ended = false;
  term.onData(d => {
    if (!ready) return;
    // After the shell ended, a key starts another.
    if (ended) { ended = false; term.reset(); send({ shell: [term.cols, term.rows] }); return; }
    ws.send(framed(1, null, enc.encode(d)));
  });
  term.onResize(s => send({ resize: [s.cols, s.rows] }));

  // The screen, drawn only while it shows.
  const canvas = $("ccanvas");
  const want = () => [Math.max(64, Math.round(canvas.clientWidth * devicePixelRatio)) || 1280,
    Math.max(64, Math.round(canvas.clientHeight * devicePixelRatio)) || 720];
  const show = drawer(canvas);
  screenInput(canvas, send);

  // Views: the gateway makes pictures only for the screen.
  const views = { term: "cterm", screen: "cscreen", files: "cfiles" };
  const pick = v => {
    view = v;
    for (const b of document.querySelectorAll("#tools [data-view]")) b.classList.toggle("on", b.dataset.view === v);
    for (const [k, el] of Object.entries(views)) $(el).classList.toggle("on", k === v);
    send({ show: v === "term" ? "terminal" : v });
    if (v === "term") { fit.fit(); term.focus(); }
    if (v === "screen") { send({ view: want() }); canvas.focus(); }
    if (v === "files" && !cwd) list("");
  };
  for (const b of document.querySelectorAll("#tools [data-view]")) b.onclick = () => pick(b.dataset.view);


  // Full screen folds the toolbar into a pill that unfolds it again.
  const fold = on => { $("client").classList.toggle("folded", on); setTimeout(relayout, 0); };
  $("full").onclick = () => document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen().catch(() => {});
  document.onfullscreenchange = () => {
    $("full").classList.toggle("on", !!document.fullscreenElement);
    fold(!!document.fullscreenElement);
  };
  $("fold").onclick = () => fold(true);
  $("pill").onclick = () => fold(false);
  $("bye").onclick = () => { if (ws) ws.close(); location.reload(); };
  let rt;
  const relayout = () => {
    if (view === "term") fit.fit();
    clearTimeout(rt);
    rt = setTimeout(() => { if (view === "screen") send({ view: want() }); }, 250);
  };
  addEventListener("resize", relayout);

  // Files.
  let cwd = "";
  const toast = text => { const t = $("toast"); t.textContent = text; t.style.display = "block"; clearTimeout(t.timer); t.timer = setTimeout(() => t.style.display = "none", 4000); };
  const sep = p => p.includes("\\") || /^[A-Za-z]:/.test(p) ? "\\" : "/";
  const join = (p, n) => p === "::" ? n : p.endsWith(sep(p)) ? p + n : p + sep(p) + n;
  const list = p => { send({ list: p }); };
  const FILE_ICON = '<svg viewBox="0 0 256 256" aria-hidden="true"><path d="m213.7 82.3-56-56A8 8 0 0 0 152 24H56a16 16 0 0 0-16 16v176a16 16 0 0 0 16 16h144a16 16 0 0 0 16-16V88a8 8 0 0 0-2.3-5.7ZM160 51.3 188.7 80H160ZM200 216H56V40h88v48a8 8 0 0 0 8 8h48v120Z"/></svg>';
  const showDir = d => {
    cwd = d.path;
    $("path").value = d.path === "::" ? "" : d.path;
    const box = $("list");
    box.textContent = "";
    if (d.error) { const n = document.createElement("div"); n.className = "note bad"; n.textContent = d.error; box.append(n); return; }
    if (!d.entries.length) { const n = document.createElement("div"); n.className = "note"; n.textContent = "This folder is empty."; box.append(n); }
    for (const e of d.entries) {
      const row = document.createElement("div");
      row.className = "row" + (e.dir ? " dir" : "");
      if (e.dir) row.append($("tools").querySelector("[data-view=files] svg").cloneNode(true));
      else row.insertAdjacentHTML("beforeend", FILE_ICON);
      const name = document.createElement("span"); name.textContent = e.name;
      const when = document.createElement("span"); when.className = "meta";
      when.textContent = e.mtime ? new Date(e.mtime * 1000).toLocaleString([], { dateStyle: "short", timeStyle: "short" }) : "";
      const big = document.createElement("span"); big.className = "meta"; big.textContent = e.dir ? "" : size(e.size);
      row.append(name, when, big);
      row.title = e.dir ? "Open" : "Download";
      row.onclick = () => e.dir ? list(join(d.path, e.name)) : download(join(d.path, e.name), e.size);
      box.append(row);
    }
  };
  $("fup").onclick = () => {
    // A root goes up to the drives (or stays at / elsewhere).
    if (!cwd || cwd === "/" || /^[A-Za-z]:\\?$/.test(cwd)) list("::"); else list(join(cwd, ".."));
  };
  $("fhome").onclick = () => list("");
  $("freload").onclick = () => list(cwd);
  $("path").onkeydown = e => { if (e.key === "Enter") list($("path").value.trim()); };

  // Transfers, with progress.
  const transfer = (label, total) => {
    const el = document.createElement("div"); el.className = "xfer";
    const name = document.createElement("span"); name.textContent = label;
    const st = document.createElement("span"); st.className = "state";
    const bar = document.createElement("progress"); bar.max = Math.max(total, 1); bar.value = 0;
    el.append(name, st, bar);
    $("transfers").prepend(el);
    const t = {
      set(n) { bar.value = n; st.textContent = size(n) + " of " + size(total); },
      done(ok, text) {
        bar.value = bar.max; el.classList.toggle("bad", !ok); st.textContent = ok ? "Done" : text || "Failed";
        setTimeout(() => el.remove(), ok ? 5000 : 15000);
      },
    };
    t.set(0);
    return t;
  };
  const downloads = {};
  const download = (path, n) => {
    if (n > MAX_TRANSFER) { toast("Larger than 2 GB: too large for a browser download."); return; }
    send({ download: path });
  };
  const uploads = {};
  let nextUpload = 1;
  const upload = async file => {
    const t = transfer("↑ " + file.name, file.size);
    if (file.size > MAX_TRANSFER) { t.done(false, "Larger than 2 GB"); return; }
    const id = nextUpload++, u = uploads[id] = { acked: 0, wake: null, t };
    send({ upload: { id, size: file.size, dir: cwd === "::" ? "" : cwd, name: file.name } });
    let off = 0;
    while (off < file.size && uploads[id]) {
      // A bounded amount unacknowledged, so neither the gateway nor the socket piles it up.
      if (off - u.acked >= WINDOW || ws.bufferedAmount > 2 * WINDOW) {
        await new Promise(r => { u.wake = r; setTimeout(r, 200); });
        continue;
      }
      const bytes = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer());
      if (ws.readyState !== 1) return;
      ws.send(framed(4, id, bytes));
      off += bytes.length;
      t.set(off);
    }
    if (uploads[id]) send({ uploadEnd: id });
  };
  const sendFiles = files => {
    if (!cwd || cwd === "::") { toast("Open a folder first."); return; }
    for (const f of files) upload(f);
  };
  $("fsend").onclick = () => $("fpick").click();
  $("fpick").onchange = () => { sendFiles([...$("fpick").files]); $("fpick").value = ""; };
  const drop = $("cfiles");
  let depth = 0;
  drop.ondragenter = e => { e.preventDefault(); depth++; drop.classList.add("dragging"); };
  drop.ondragover = e => e.preventDefault();
  drop.ondragleave = () => { if (--depth <= 0) { depth = 0; drop.classList.remove("dragging"); } };
  drop.ondrop = e => {
    e.preventDefault();
    depth = 0; drop.classList.remove("dragging");
    sendFiles([...e.dataTransfer.files]);
  };

  // The gateway's messages.
  ws.onopen = () => {
    const [w, h] = want();
    ws.send(JSON.stringify({ id, password, w, h, cols: term.cols, rows: term.rows }));
  };
  ws.onmessage = m => {
    if (typeof m.data !== "string") {
      const b = new Uint8Array(m.data);
      if (b[0] === 1) term.write(b.subarray(1));
      else if (b[0] === 2) show(m.data.slice(1));
      else if (b[0] === 3) {
        const d = downloads[new DataView(m.data).getUint32(1, true)];
        if (d) { d.parts.push(b.slice(5)); d.got += b.length - 5; d.t.set(d.got); }
      }
      return;
    }
    const v = JSON.parse(m.data);
    if (v.status) state(v.status);
    if (v.error && !ready) {
      // Refused (a wrong ID or password): back to the form, saying why.
      $("client").style.display = "none";
      $("card").style.display = "";
      document.title = "SSH Remote Web";
      term.dispose();
      fail(v.error);
      return;
    }
    if (v.ready) {
      ready = true;
      state(v.ready.screen[0] + "×" + v.ready.screen[1], "up");
      term.focus();
    }
    if (v.exit !== undefined) {
      ended = true;
      term.write("\r\n\x1b[2m[" + (v.exit || "the shell ended") + " — press a key for a new one]\x1b[0m\r\n");
    }
    if (v.dir) showDir(v.dir);
    if (v.fetch) downloads[v.fetch.id] = { name: v.fetch.name, parts: [], got: 0, t: transfer("↓ " + v.fetch.name, v.fetch.size) };
    if (v.fetched) {
      const d = downloads[v.fetched.id];
      delete downloads[v.fetched.id];
      if (!d) { if (!v.fetched.ok) toast((v.fetched.name ? v.fetched.name + ": " : "") + v.fetched.text); return; }
      d.t.done(v.fetched.ok, v.fetched.text);
      if (v.fetched.ok) {
        const a = document.createElement("a");
        a.href = URL.createObjectURL(new Blob(d.parts));
        a.download = d.name.split(/[\\/]/).pop();
        document.body.append(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(a.href), 60000);
      }
    }
    if (v.progress) {
      const u = uploads[v.progress[0]];
      if (u) { u.acked = v.progress[1]; if (u.wake) u.wake(); }
    }
    if (v.uploaded) {
      const u = uploads[v.uploaded.id];
      delete uploads[v.uploaded.id];
      if (u) { u.t.done(v.uploaded.ok, v.uploaded.text); if (u.wake) u.wake(); }
      if (v.uploaded.ok && view === "files") list(cwd);
    }
    if (v.notice) toast(v.notice);
    if (v.end || v.error) { state(v.end || v.error, "down"); term.write("\r\n\x1b[31m[" + (v.end || v.error) + "]\x1b[0m\r\n"); }
  };
  ws.onclose = () => { if (ready) state("Disconnected", "down"); ready = false; };
}
