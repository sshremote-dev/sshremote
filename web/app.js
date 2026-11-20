"use strict";
// SSH Remote in a browser: the open page (an ID or an SSH host, saved connections, this gateway's
// own hosts) and one connection at a time with its terminal, screen and files, as in the app.

const $ = id => document.getElementById(id);
const base = location.pathname.replace(/[^/]*$/, "");
const WS = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + base + "ws";
// A download request with this bit in its tag is told when its last file has been sent.
const DONE_TAG = 0x80000000;
const MAX_TRANSFER = 2 * 1024 ** 3, CHUNK = 64 * 1024, WINDOW = 1024 * 1024;

// ----- Small helpers -------------------------------------------------------------------------------

const ICONS = {
  sun: "M12 8a4 4 0 1 1 0 8 4 4 0 0 1 0-8z|M12 2v2|M12 20v2|M4.9 4.9l1.4 1.4|M17.7 17.7l1.4 1.4|M2 12h2|M20 12h2|M4.9 19.1l1.4-1.4|M17.7 6.3l1.4-1.4",
  moon: "M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z",
  auto: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z|M12 3v18|M12 7.5h7|M12 12h9|M12 16.5h7",
  terminal: "M4 5h16v14H4z|M7.5 9.5 10 12l-2.5 2.5|M12.5 15h4",
  monitor: "M3 4h18v12H3z|M8 20h8|M12 16v4",
  folder: "M3 6.5h6l2 2h10V19H3z",
  file: "M6 3h8l5 5v13H6z|M14 3v5h5",
  up: "M12 19V5|M6 11l6-6 6 6",
  home: "M3.5 11 12 3.5l8.5 7.5|M5.5 9.5V20h13V9.5",
  reload: "M20 12a8 8 0 1 1-2.4-5.7|M20 4v5h-5",
  upload: "M12 15V4|M7 9l5-5 5 5|M4 15v5h16v-5",
  download: "M12 4v11|M7 10l5 5 5-5|M4 15v5h16v-5",
  newfolder: "M3 6.5h6l2 2h10V19H3z|M12 11v6|M9 14h6",
  rename: "M4 20h4L19 9l-4-4L4 16z|M13.5 6.5l4 4",
  trash: "M4 7h16|M10 11v6|M14 11v6|M6 7l1 13h10l1-13|M9 7V4h6v3",
  keyboard: "M3 6h18v12H3z|M7 10h.01|M11 10h.01|M15 10h.01|M7.5 14h9",
  chat: "M4 5h16v11H10l-5 4v-4H4z",
  pause: "M8 5v14|M16 5v14",
  window: "M3 5h18v14H3z|M3 9h18",
  sound: "M4 9h4l5-4v14l-5-4H4z|M16 9a4 4 0 0 1 0 6|M18.5 6.5a7.5 7.5 0 0 1 0 11",
  phone: "M6.6 3.5 9.3 6.3 7.8 8.6a11 11 0 0 0 7.6 7.6l2.3-1.5 2.8 2.7-1 2.4a2 2 0 0 1-2.2 1.2A16 16 0 0 1 3.1 7.7a2 2 0 0 1 1.2-2.2z",
  record: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z|M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z",
  below: "M3 4h18v16H3z|M3 13h18|M6.5 16l2 1.5-2 1.5",
  play: "M7 5l12 7-12 7z",
  clipboard: "M9 3.5h6v3H9z|M7 5H5v16h14V5h-2",
  eye: "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z|M12 9.2a2.8 2.8 0 1 0 0 5.6 2.8 2.8 0 0 0 0-5.6z",
  camera: "M4 8h3.5l2-3h5l2 3H20v11H4z|M12 10a3.3 3.3 0 1 0 0 6.6 3.3 3.3 0 0 0 0-6.6z",
  sliders: "M4 7h9|M17 7h3|M15 5v4|M4 12h3|M11 12h9|M9 10v4|M4 17h11|M19 17h1|M17 15v4",
  screens: "M2 5h13v9H2z|M9 14v4|M5 18h8|M18 8h4v10h-6",
  full: "M4 9V4h5|M20 9V4h-5|M4 15v5h5|M20 15v5h-5",
  unfull: "M9 4v5H4|M15 4v5h5|M9 20v-5H4|M15 20v-5h5",
  caretup: "M6 15l6-6 6 6",
  caretdown: "M6 9l6 6 6-6",
  logout: "M10 4H5v16h5|M15 8l4 4-4 4|M19 12H9",
  x: "M6 6l12 12|M18 6 6 18",
  dots: "M5 12h.01|M12 12h.01|M19 12h.01",
  server: "M4 4h16v6H4z|M4 14h16v6H4z|M8 7h.01|M8 17h.01",
  lock: "M6 11h12v9H6z|M8 11V7.5a4 4 0 0 1 8 0V11",
  open: "M5 12h14|M13 6l6 6-6 6",
};
const SVG = "http://www.w3.org/2000/svg";
function icon(name) {
  const s = document.createElementNS(SVG, "svg");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("class", "i");
  s.setAttribute("aria-hidden", "true");
  for (const d of ICONS[name].split("|")) {
    const p = document.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    s.append(p);
  }
  return s;
}

// An element: h("div", { class: "x", onclick }, child...).
function h(tag, props, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e[k] = v;
    else e.setAttribute(k, v === true ? "" : v);
  }
  for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) e.append(k);
  return e;
}
// A button with an icon and, if given, a label.
function button(ic, label, props) {
  const b = h("button", { type: "button", class: "btn" + (label ? "" : " icon"), ...props });
  if (ic) b.append(icon(ic));
  if (label) b.append(h("span", { text: label }));
  return b;
}
function setButton(b, ic, label) { b.textContent = ""; if (ic) b.append(icon(ic)); if (label) b.append(h("span", { class: "tx", text: label })); }

function size(n) {
  if (n < 1024) return n + " B";
  const u = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return n.toFixed(n < 10 ? 1 : 0) + " " + u[i];
}
function ago(ms) {
  const s = (Date.now() - ms) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + " min ago";
  if (s < 86400) return Math.floor(s / 3600) + " h ago";
  if (s < 172800) return "yesterday";
  return Math.floor(s / 86400) + " days ago";
}
const spaced = id => id.replace(/(\d{3})(?=\d)/g, "$1 ");
const when = t => t ? new Date(t * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "";

function toast(text, opts = {}) {
  const t = h("div", { class: "toast" + (opts.bad ? " bad" : ""), role: "status" }, h("span", { text }));
  if (opts.action) t.append(button(null, opts.action[0], { class: "btn solid", onclick: () => { opts.action[1](); t.remove(); } }));
  $("toasts").append(t);
  setTimeout(() => t.remove(), opts.ms || (opts.action ? 12000 : 5000));
}

// When the page last lost the keyboard to another window or tab: something may have been copied
// there since, newer than what the host copied before.
let leftAt = 0;
addEventListener("blur", () => { leftAt = Date.now(); });
document.addEventListener("visibilitychange", () => { if (document.hidden) leftAt = Date.now(); });

// A menu of [label, action, {on, kbd, danger}] items (null for a rule, a string for a heading),
// under `anchor` or at a point.
let openMenu = null;
function menu(items, at) {
  closeMenu();
  const m = h("div", { class: "menu", role: "menu" });
  for (const it of items) {
    if (it === null) { m.append(h("hr")); continue; }
    if (typeof it === "string") { m.append(h("div", { class: "lab", text: it })); continue; }
    const [label, act, o = {}] = it;
    const b = h("button", { type: "button", role: "menuitem", class: o.on ? "on" : "", disabled: o.disabled },
      o.icon ? icon(o.icon) : null, h("span", { text: label }), o.kbd ? h("kbd", { text: o.kbd }) : null);
    b.onclick = () => { closeMenu(); act(); };
    m.append(b);
  }
  document.body.append(m);
  const r = at instanceof Element ? at.getBoundingClientRect() : { left: at[0], bottom: at[1], top: at[1] };
  const w = m.offsetWidth, hh = m.offsetHeight;
  m.style.left = Math.max(4, Math.min(r.left, innerWidth - w - 4)) + "px";
  m.style.top = (r.bottom + hh + 4 > innerHeight ? Math.max(4, r.top - hh - 4) : r.bottom + 4) + "px";
  openMenu = m;
  m.querySelector("button:not([disabled])")?.focus();
  setTimeout(() => addEventListener("pointerdown", outside, true), 0);
}
function outside(e) { if (openMenu && !openMenu.contains(e.target)) closeMenu(); }
function closeMenu() { if (openMenu) { openMenu.remove(); openMenu = null; removeEventListener("pointerdown", outside, true); } }
addEventListener("keydown", e => { if (e.key === "Escape" && openMenu) { e.preventDefault(); closeMenu(); } }, true);

// A dialog: a title, a line of text, an optional field, and its buttons; resolves with the
// field's text (or true), or null when cancelled.
function ask({ title, text, field, area, value, select, ok = "OK", danger }) {
  return new Promise(done => {
    const box = $("modal").firstElementChild;
    box.textContent = "";
    const input = field || area ? h(area ? "textarea" : "input", { placeholder: field || area, spellcheck: "false" }) : null;
    if (input) input.value = value || "";
    const finish = v => { $("modal").hidden = true; removeEventListener("keydown", keys, true); done(v); };
    const yes = h("button", { type: "button", class: "btn " + (danger ? "solid danger" : "primary"), text: ok });
    yes.onclick = () => finish(input ? input.value : true);
    const keys = e => {
      if (e.key === "Escape") { e.preventDefault(); finish(null); }
      if (e.key === "Enter" && !area) { e.preventDefault(); yes.click(); }
    };
    box.append(...[h("h2", { text: title }), text ? h("p", { text }) : null, input,
      h("div", { class: "acts" }, h("button", { type: "button", class: "btn solid", text: "Cancel", onclick: () => finish(null) }), yes)].filter(Boolean));
    $("modal").hidden = false;
    addEventListener("keydown", keys, true);
    if (input) { input.focus(); if (select) input.setSelectionRange(...select(input.value)); } else yes.focus();
  });
}

// ----- What this browser keeps ----------------------------------------------------------------------

const store = {
  get(k, d) { try { const v = localStorage.getItem("sshremote." + k); return v ? JSON.parse(v) : d; } catch { return d; } },
  set(k, v) { try { if (v === null) localStorage.removeItem("sshremote." + k); else localStorage.setItem("sshremote." + k, JSON.stringify(v)); } catch { /* private mode */ } },
};
// The theme chosen here (light or dark) wins over the system's, from the first paint on.
{ const t = store.get("theme", ""); if (t === "light" || t === "dark") document.documentElement.dataset.theme = t; }
const b64 = u => btoa(String.fromCharCode(...u));
const unb64 = s => Uint8Array.from(atob(s), c => c.charCodeAt(0));

// Passwords are kept sealed with a key this browser made and cannot export: it lives in
// IndexedDB, usable by this page only, so the saved text alone gives nothing away.
let keyPromise = null;
function secretKey() {
  if (keyPromise) return keyPromise;
  keyPromise = new Promise((ok, no) => {
    if (!window.indexedDB || !window.crypto?.subtle) { no(new Error("no safe store")); return; }
    const req = indexedDB.open("sshremote", 1);
    req.onupgradeneeded = () => req.result.createObjectStore("keys");
    req.onerror = () => no(req.error);
    req.onsuccess = () => {
      const db = req.result;
      const get = db.transaction("keys").objectStore("keys").get("passwords");
      get.onerror = () => no(get.error);
      get.onsuccess = async () => {
        if (get.result) { ok(get.result); return; }
        try {
          const k = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
          const put = db.transaction("keys", "readwrite").objectStore("keys").put(k, "passwords");
          put.onsuccess = () => ok(k);
          put.onerror = () => no(put.error);
        } catch (e) { no(e); }
      };
    };
  });
  keyPromise.catch(() => { keyPromise = null; });
  return keyPromise;
}
const canKeep = !!(window.isSecureContext && window.crypto?.subtle && window.indexedDB);
async function seal(text) {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const data = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, await secretKey(), new TextEncoder().encode(text));
  return { iv: b64(iv), data: b64(new Uint8Array(data)) };
}
async function unseal(s) {
  const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: unb64(s.iv) }, await secretKey(), unb64(s.data));
  return new TextDecoder().decode(plain);
}

// A gateway for computers by ID only (a public one, as web.sshremote.dev): no SSH, no sign-in,
// none of its own computers. It says so on the page it serves.
const idOnly = document.documentElement.hasAttribute("data-id-only");
const allowed = e => !idOnly || e.kind === "id";

// Saved connections: { key, kind: "id" | "ssh" | "gw", address, port, name, last, view, pw }.
const saved = () => store.get("saved", []);
const keyOf = t => t.kind + ":" + t.address + (t.port ? ":" + t.port : "");
function title(t) { return t.name || (t.kind === "id" ? "ID " + spaced(t.address) : t.address + (t.port ? ":" + t.port : "")); }
function save(entry) {
  const list = saved().filter(e => e.key !== entry.key);
  list.unshift(entry);
  store.set("saved", list.slice(0, 40));
}
function update(key, change) {
  const list = saved();
  const e = list.find(x => x.key === key);
  if (e) { Object.assign(e, change); store.set("saved", list); }
}

// The gateway sign-in this browser remembers.
const gateway = { token: null, until: 0, hosts: null, password: null };
{
  const g = store.get("gateway", null);
  if (g && g.until > Date.now()) Object.assign(gateway, g);
}
const signedIn = () => !!(gateway.token || gateway.password);
const gatewayAuth = () => gateway.token ? { token: gateway.token } : { password: gateway.password };

// ----- The open page --------------------------------------------------------------------------------

function parse(text) {
  const t = text.trim();
  const digits = t.replace(/[\s-]/g, "");
  if (/^\d{9}$/.test(digits)) return { kind: "id", address: digits };
  if (/^\d[\d\s-]*$/.test(t)) return { kind: "id", address: digits, partial: true };
  if (!t) return null;
  // user@host:port, [v6]:port, or a plain alias.
  let host = t, port = "";
  const v6 = /^(.*\[[^\]]+\]):(\d{1,5})$/.exec(t);
  const v4 = /^([^:]*):(\d{1,5})$/.exec(t);
  if (v6) [, host, port] = v6; else if (v4) [, host, port] = v4;
  return { kind: "ssh", address: host.replace(/[[\]]/g, ""), port };
}

function fail(text) { $("error").textContent = text || ""; }

function refreshForm() {
  const p = parse($("addr").value);
  const ssh = p?.kind === "ssh" && !idOnly;
  // SSH needs the gateway's owner first: one password at a time, the gateway's, then the host's.
  const locked = ssh && !signedIn();
  $("port").hidden = !ssh;
  $("gatebox").hidden = !locked;
  $("pwlabel").hidden = $("pwrow").hidden = locked;
  $("go").textContent = locked ? "Sign in to this gateway" : "Connect";
  const match = p && saved().find(e => e.key === keyOf({ ...p, port: ssh ? $("port").value.trim() : "" }));
  $("pw").placeholder = match?.pw ? "Saved password (type to change)" : ssh ? "Password (none with SSH keys)" : "Password set on that computer";
  $("keeprow").hidden = !canKeep || locked;
  // What was typed, named; and what the password is for it.
  $("kind").textContent = !p || (idOnly && p.kind !== "id") ? "" : ssh ? "SSH host" : "ID";
  $("pwlabel").textContent = ssh ? "SSH password" : "Password";
  $("hint").textContent = idOnly && p && p.kind !== "id" ? "This page opens computers by their 9-digit ID. For SSH, use the app." : !p || locked ? "" : ssh
    ? "Signed in to this gateway: SSH leaves from its server."
    : "A one-time password works too. Without any, leave it empty: the person there is asked to let you in, where that computer allows it.";
}

// The page's theme: the system's, or light or dark as chosen here (kept in this browser).
function themeButton() {
  const now = store.get("theme", "");
  const [glyph, title] = { light: ["sun", "Light theme (click: dark)"], dark: ["moon", "Dark theme (click: the system's)"] }[now] || ["auto", "The system's theme (click: light)"];
  const b = $("theme");
  b.replaceChildren(icon(glyph));
  b.title = title;
  b.setAttribute("aria-label", title);
}
$("theme").onclick = () => {
  const next = { "": "light", light: "dark", dark: "" }[store.get("theme", "")] ?? "";
  store.set("theme", next);
  if (next) document.documentElement.dataset.theme = next;
  else delete document.documentElement.dataset.theme;
  themeButton();
};
themeButton();
$("sitelink").prepend(icon("home"));

if (idOnly) {
  $("addrlabel").textContent = "Remote access ID";
  $("addr").placeholder = "123 456 789";
  $("addr").setAttribute("inputmode", "numeric");
  $("addr").setAttribute("aria-label", "Remote access ID");
  $("gatelink").hidden = true;
}

$("addr").addEventListener("input", () => {
  // An ID is spaced in threes as the app shows it.
  const v = $("addr").value;
  if (/^[\d\s]+$/.test(v)) {
    const d = v.replace(/\D/g, "").slice(0, 9);
    $("addr").value = spaced(d);
  }
  const p = parse($("addr").value);
  if (p?.kind === "ssh" && p.port) $("port").value = p.port;
  fail("");
  refreshForm();
});
$("port").addEventListener("input", refreshForm);
$("keep").checked = store.get("keep", true) && canKeep;
$("keep").onchange = () => store.set("keep", $("keep").checked);

$("form").onsubmit = async e => {
  e.preventDefault();
  fail("");
  const p = parse($("addr").value);
  if (!p) { fail("Enter a remote access ID, or an SSH host such as user@192.168.1.20."); $("addr").focus(); return; }
  if (idOnly && p.kind !== "id") { fail("This page opens computers by their 9-digit ID. For SSH, use the app: sshremote.dev."); $("addr").focus(); return; }
  if (p.kind === "id" && p.address.length !== 9) { fail("An ID is 9 digits."); $("addr").focus(); return; }
  const port = p.kind === "ssh" ? ($("port").value.trim() || p.port || "") : "";
  if (port && !(+port >= 1 && +port <= 65535)) { fail("A port is a number from 1 to 65535."); $("port").focus(); return; }
  const t = { kind: p.kind, address: p.address, port, key: keyOf({ ...p, port }) };
  const typed = $("pw").value;
  const known = saved().find(x => x.key === t.key);
  let password = typed;
  if (!typed && known?.pw) {
    try { password = await unseal(known.pw); } catch { fail("The saved password could not be read on this browser; type it again."); $("pw").focus(); return; }
  }
  // An ID without a password asks the person there to let this page in (where that computer
  // takes such requests; else it says so and the password is asked for).
  // SSH: the gateway's sign-in is its own step; the host's password is asked for after it.
  if (p.kind === "ssh" && !signedIn()) {
    const gp = $("gatepw").value;
    if (!gp) { fail("The gateway's password, set where it runs."); $("gatepw").focus(); return; }
    $("go").disabled = true;
    try { await signIn(gp, $("gatekeep").checked); } catch (err) { $("go").disabled = false; fail(err.message); $("gatepw").focus(); return; }
    $("go").disabled = false;
    $("gatepw").value = "";
    toast("Signed in to this gateway. Now the SSH host's password, if it asks for one.");
    $("pw").focus();
    return;
  }
  // A password typed without "Remember" replaces the kept one with none.
  // The two-step code goes with this try only, never kept.
  const code = p.kind === "id" ? $("code").value.trim() : "";
  $("code").value = "";
  connect({ ...t, name: known?.name, view: known?.view, password, code, keep: $("keep").checked && !!typed, kept: typed ? null : known?.pw });
};

function renderSaved() {
  const list = saved().filter(allowed);
  const box = $("saved");
  box.textContent = "";
  $("savedn").textContent = list.length ? list.length : "";
  $("guide").hidden = list.length > 0;
  if (!list.length) { box.append(h("div", { class: "empty", text: "Computers you open are kept here, to open again with a click." })); return; }
  for (const e of list) {
    const sub = [e.kind === "id" ? "Remote access by ID" : e.kind === "gw" ? "On this gateway" : "SSH" + (e.port ? " · port " + e.port : ""),
      e.pw ? "password kept" : null, e.last ? ago(e.last) : null].filter(Boolean).join(" · ");
    const go = () => openSaved(e);
    const more = button("dots", null, { title: "More", class: "btn icon" });
    more.onclick = ev => {
      ev.stopPropagation();
      menu([
        ["Connect", go, { icon: "open" }],
        ["Rename…", async () => {
          const name = await ask({ title: "Rename", field: title(e), value: title(e), ok: "Rename" });
          if (name !== null) { update(e.key, { name: name.trim() || null }); renderSaved(); }
        }, { icon: "rename" }],
        ["Forget the password", () => { update(e.key, { pw: null }); renderSaved(); refreshForm(); }, { icon: "lock", disabled: !e.pw }],
        null,
        ["Remove", () => { store.set("saved", saved().filter(x => x.key !== e.key)); renderSaved(); }, { icon: "trash" }],
      ], more);
    };
    const row = h("div", { class: "item", tabindex: "0", title: "Click to fill in, double-click to connect" },
      h("div", { class: "ico" }, icon(e.kind === "id" || e.address === "This computer" ? "monitor" : e.kind === "gw" ? "server" : "terminal")),
      h("div", { class: "what" }, h("b", { text: title(e) }), h("span", { text: sub })),
      h("div", { class: "acts" }, button(null, "Connect", { class: "btn primary", onclick: ev => { ev.stopPropagation(); go(); } }), more));
    row.onclick = () => {
      for (const r of box.children) r.classList.remove("sel");
      row.classList.add("sel");
      if (e.kind === "gw") return;
      $("addr").value = e.kind === "id" ? spaced(e.address) : e.address;
      $("port").value = e.port || "";
      $("pw").value = "";
      fail("");
      refreshForm();
      (e.pw || e.kind === "ssh" ? $("go") : $("pw")).focus();
    };
    row.ondblclick = go;
    row.onkeydown = ev => { if (ev.key === "Enter") go(); };
    box.append(row);
  }
}

async function openSaved(e) {
  fail("");
  let password = "";
  if (e.pw) {
    try { password = await unseal(e.pw); } catch { password = ""; }
  }
  // The gateway's own computers: its sign-in first, in its dialog.
  if (e.kind === "gw" && !signedIn()) { $("gatelink").click(); return; }
  if (e.kind === "ssh" && !signedIn()) {
    $("addr").value = e.address;
    $("port").value = e.port || "";
    refreshForm();
    $("gatepw").focus();
    return;
  }
  connect({ ...e, password, keep: false, kept: e.pw });
}

function renderGateway() {
  $("gatesec").hidden = !signedIn() || !gateway.hosts;
  $("gatelink").textContent = signedIn() ? "Signed in to this gateway" : "Sign in to this gateway";
  const box = $("ghosts");
  box.textContent = "";
  for (const host of gateway.hosts || []) {
    const t = { kind: "gw", address: host, port: "", key: "gw:" + host };
    const go = () => connect({ ...t, ...(saved().find(x => x.key === t.key) || {}), password: "" });
    const row = h("div", { class: "item", tabindex: "0" },
      h("div", { class: "ico" }, icon(host === "This computer" ? "monitor" : "server")),
      h("div", { class: "what" }, h("b", { text: host }), h("span", { text: host === "This computer" ? "The gateway's own computer" : "From the gateway's ~/.ssh/config" })),
      h("div", { class: "acts" }, button(null, "Connect", { class: "btn primary", onclick: ev => { ev.stopPropagation(); go(); } })));
    row.ondblclick = go;
    row.onkeydown = ev => { if (ev.key === "Enter") go(); };
    box.append(row);
  }
}

// Signs in to the gateway; resolves with its hosts, keeping a sign-in when asked to.
function signIn(password, remember, token) {
  return new Promise((ok, no) => {
    const ws = new WebSocket(WS);
    let settled = false;
    const end = (f, v) => { if (!settled) { settled = true; f(v); try { ws.close(); } catch { /* gone */ } } };
    ws.onopen = () => ws.send(JSON.stringify({ gateway: token ? { token } : { password, remember } }));
    ws.onerror = () => end(no, new Error("The gateway could not be reached."));
    ws.onclose = () => end(no, new Error("The gateway closed the connection."));
    ws.onmessage = m => {
      const v = JSON.parse(m.data);
      if (v.error) { end(no, Object.assign(new Error(v.error), { signin: !!v.signin })); return; }
      if (!v.signedIn) return;
      gateway.hosts = v.signedIn.hosts;
      if (v.signedIn.token) {
        Object.assign(gateway, { token: v.signedIn.token, until: Date.now() + v.signedIn.days * 864e5, password: null });
        store.set("gateway", { token: gateway.token, until: gateway.until });
      } else if (!token) {
        gateway.password = password;
      }
      renderGateway();
      refreshForm();
      end(ok, v.signedIn.hosts);
    };
  });
}
function signOut() {
  Object.assign(gateway, { token: null, until: 0, hosts: null, password: null });
  store.set("gateway", null);
  renderGateway();
  refreshForm();
}
$("signout").onclick = signOut;
$("gatelink").onclick = async e => {
  e.preventDefault();
  if (signedIn()) { signOut(); return; }
  const pw = await ask({ title: "Sign in to this gateway", text: "Its password lets this browser reach SSH hosts and the gateway's own computers.", field: "Gateway password", ok: "Sign in" });
  if (pw === null) return;
  try { await signIn(pw, true); toast("Signed in to this gateway for 30 days."); } catch (err) { toast(err.message, { bad: true }); }
};

// ----- A connection ---------------------------------------------------------------------------------

let current = null;

// Keys as X keysyms, which the agent takes on every system.
const NAMED = { Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, Escape: 0xff1b, Delete: 0xffff, Home: 0xff50,
  ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54, PageUp: 0xff55, PageDown: 0xff56,
  End: 0xff57, Insert: 0xff63, ContextMenu: 0xff67, Pause: 0xff13, PrintScreen: 0xff61, CapsLock: 0xffe5, NumLock: 0xff7f, ScrollLock: 0xff14 };
const SIDED = { Shift: [0xffe1, 0xffe2], Control: [0xffe3, 0xffe4], Alt: [0xffe9, 0xffea], Meta: [0xffeb, 0xffec], OS: [0xffeb, 0xffec], AltGraph: [0xffea, 0xffea] };
const K = { ctrl: 0xffe3, alt: 0xffe9, shift: 0xffe1, win: 0xffeb, tab: 0xff09, esc: 0xff1b, del: 0xffff, f4: 0xffc1, prt: 0xff61, enter: 0xff0d };
function keysym(e) {
  if (NAMED[e.key]) return NAMED[e.key];
  if (SIDED[e.key]) return SIDED[e.key][e.location === 2 ? 1 : 0];
  const f = /^F(\d{1,2})$/.exec(e.key);
  if (f && +f[1] >= 1 && +f[1] <= 24) return 0xffbe + Number(f[1]) - 1;
  if ([...e.key].length === 1) {
    // With Ctrl or Alt the key's own letter, not what the layout made of it; AltGr (which
    // Windows reports as Ctrl+Alt) makes a character of its own, "@" or "€", sent as it is.
    let c = e.key.codePointAt(0);
    const altGr = e.getModifierState && e.getModifierState("AltGraph");
    if ((e.ctrlKey || e.altKey) && !altGr && /^Key[A-Z]$/.test(e.code)) c = e.code.charCodeAt(3) + (e.shiftKey ? 0 : 32);
    return c < 0x100 ? c : 0x01000000 + c;
  }
  return 0;
}

// Draws the gateway's picture updates on `canvas`, in order: the moves (an area shifted, as the
// host scrolled), then the changed areas, each a WebP or JPEG picture. `drawn` is told each
// update's number once it is on the canvas, and `first` the first time.
function painter(canvas, first, drawn, resized) {
  const ctx = canvas.getContext("2d");
  const TYPES = ["image/webp", "image/jpeg"];
  const queue = [];
  let busy = false;
  const apply = async buf => {
    const d = new DataView(buf);
    let o = 1;
    const seq = d.getUint32(o, true), w = d.getUint16(o + 4, true), hh = d.getUint16(o + 6, true);
    o += 8;
    if (canvas.width !== w || canvas.height !== hh) { canvas.width = w; canvas.height = hh; resized(); }
    const moves = d.getUint8(o++);
    for (let i = 0; i < moves; i++, o += 10) {
      const x = d.getUint16(o, true), y = d.getUint16(o + 2, true), mw = d.getUint16(o + 4, true), mh = d.getUint16(o + 6, true), dy = d.getInt16(o + 8, true);
      ctx.drawImage(canvas, x, y - dy, mw, mh, x, y, mw, mh);
    }
    const parts = [];
    const areas = d.getUint8(o++);
    for (let i = 0; i < areas; i++) {
      const x = d.getUint16(o, true), y = d.getUint16(o + 2, true), fmt = d.getUint8(o + 8), len = d.getUint32(o + 9, true);
      o += 13;
      parts.push([x, y, createImageBitmap(new Blob([new Uint8Array(buf, o, len)], { type: TYPES[fmt] || TYPES[1] }))]);
      o += len;
    }
    for (const [x, y, made] of parts) {
      try { const b = await made; ctx.drawImage(b, x, y); b.close(); } catch { /* a part this browser cannot decode */ }
    }
    drawn(seq);
    first();
  };
  const run = async () => {
    if (busy) return;
    busy = true;
    while (queue.length) await apply(queue.shift());
    busy = false;
  };
  return buf => { queue.push(buf); run(); };
}

// The host's sound: 20 ms Opus packets, decoded by the browser (WebCodecs) and played about
// 80 ms behind, which rides out the network's unevenness. Null where the browser cannot.
function soundPlayer() {
  if (!window.AudioDecoder || !window.AudioContext) return null;
  const ac = new AudioContext({ sampleRate: 48000 });
  let at = 0, ts = 0;
  const dec = new AudioDecoder({
    output: d => {
      const n = d.numberOfFrames, ch = d.numberOfChannels;
      const buf = ac.createBuffer(ch, n, d.sampleRate);
      for (let c = 0; c < ch; c++) d.copyTo(buf.getChannelData(c), { planeIndex: c, format: "f32-planar" });
      d.close();
      const now = ac.currentTime;
      // Behind by more than a moment (the tab slept): start over at the delay.
      if (at < now || at > now + 0.5) at = now + 0.08;
      const src = ac.createBufferSource();
      src.buffer = buf;
      src.connect(ac.destination);
      src.start(at);
      at += buf.duration;
    },
    error: () => {},
  });
  dec.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 2 });
  return {
    packet(p) {
      if (dec.state !== "configured") return;
      dec.decode(new EncodedAudioChunk({ type: "key", timestamp: ts, data: p }));
      ts += 20000;
    },
    close() { try { dec.close(); } catch { /* closed */ } ac.close(); },
  };
}

// This side's voice in a call: the microphone as 20 ms Opus packets (WebCodecs), each handed to
// `send`. Null where the browser cannot; the call goes on with their voice only.
async function voiceSender(send) {
  if (!window.AudioEncoder || !navigator.mediaDevices?.getUserMedia) return null;
  let stream;
  try { stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } }); } catch { return null; }
  const ac = new AudioContext({ sampleRate: 48000 });
  const enc = new AudioEncoder({ output: chunk => { const b = new Uint8Array(chunk.byteLength); chunk.copyTo(b); send(b); }, error: () => {} });
  enc.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 1, bitrate: 24000 });
  const src = ac.createMediaStreamSource(stream);
  // A plain processor: every browser has it, and 20 ms at a time is all a voice needs.
  const node = ac.createScriptProcessor(1024, 1, 1);
  let ts = 0;
  node.onaudioprocess = e => {
    if (enc.state !== "configured") return;
    const data = new Float32Array(e.inputBuffer.getChannelData(0));
    enc.encode(new AudioData({ format: "f32", sampleRate: 48000, numberOfFrames: data.length, numberOfChannels: 1, timestamp: ts, data }));
    ts += Math.round(data.length / 48000 * 1e6);
  };
  src.connect(node);
  node.connect(ac.destination);
  return { close() { try { enc.close(); } catch { /* closed */ } stream.getTracks().forEach(t => t.stop()); ac.close(); } };
}

// A ZIP file (stored, not compressed: what is sent is already as small as it gets over the
// link) built from parts as they arrive, so a folder downloads as one file.
const CRC = new Uint32Array(256).map((_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
function crc32(crc, bytes) { crc = ~crc >>> 0; for (let i = 0; i < bytes.length; i++) crc = CRC[(crc ^ bytes[i]) & 255] ^ (crc >>> 8); return ~crc >>> 0; }
function zipBlob(entries) {
  const enc = new TextEncoder(), parts = [], central = [];
  const d = new Date(), time = (d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1);
  const date = ((d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate();
  let offset = 0;
  for (const e of entries) {
    const name = enc.encode(e.name);
    const head = new DataView(new ArrayBuffer(30));
    [[0, 0x04034b50, 4], [4, 20, 2], [6, 0x0800, 2], [8, 0, 2], [10, time, 2], [12, date, 2], [14, e.crc, 4], [18, e.size, 4], [22, e.size, 4], [26, name.length, 2], [28, 0, 2]]
      .forEach(([at, v, n]) => n === 4 ? head.setUint32(at, v, true) : head.setUint16(at, v, true));
    parts.push(head, name, ...e.parts);
    const c = new DataView(new ArrayBuffer(46));
    [[0, 0x02014b50, 4], [4, 20, 2], [6, 20, 2], [8, 0x0800, 2], [10, 0, 2], [12, time, 2], [14, date, 2], [16, e.crc, 4], [20, e.size, 4], [24, e.size, 4],
      [28, name.length, 2], [30, 0, 2], [32, 0, 2], [34, 0, 2], [36, 0, 2], [38, 0, 4], [42, offset, 4]]
      .forEach(([at, v, n]) => n === 4 ? c.setUint32(at, v, true) : c.setUint16(at, v, true));
    central.push(c, name);
    offset += 30 + name.length + e.size;
  }
  const cdSize = central.reduce((n, p) => n + p.byteLength, 0);
  const end = new DataView(new ArrayBuffer(22));
  [[0, 0x06054b50, 4], [4, 0, 2], [6, 0, 2], [8, entries.length, 2], [10, entries.length, 2], [12, cdSize, 4], [16, offset, 4], [20, 0, 2]]
    .forEach(([at, v, n]) => n === 4 ? end.setUint32(at, v, true) : end.setUint16(at, v, true));
  return new Blob([...parts, ...central, end], { type: "application/zip" });
}
function saveBlob(blob, name) {
  const a = h("a", { href: URL.createObjectURL(blob), download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 60000);
}

// Opens `t` ({ kind, address, port, password, ... }) and shows it.
function connect(t) {
  if (current) current.close();
  current = new Client(t);
}

class Client {
  constructor(t) {
    this.t = t;
    this.ready = false;
    this.ended = false;
    this.view = null;
    this.viewOnly = false;
    this.agent = t.kind === "id" ? "up" : "idle";
    this.files = { cwd: null, entries: [], sel: new Set(), anchor: null, sort: ["name", 1], wanted: false };
    this.jobs = new Map();       // download requests by tag
    this.byId = new Map();       // downloads by file id: { job, entry }
    this.nextTag = 1;
    this.uploads = [];           // waiting to be sent, one at a time
    this.sending = null;
    this.nextUpload = 1;
    this.monitors = null;
    this.hostClip = null;
    this.chatLines = [];         // { you, text, at }
    this.split = store.get("split", false);
    this.unread = 0;
    $("home").hidden = true;
    $("client").hidden = false;
    $("banner").hidden = true;
    $("name").textContent = $("tabname").textContent = title(t);
    document.title = title(t) + " - SSH Remote Web";
    this.state("Connecting…");
    this.chrome();
    this.terminal();
    this.screen();
    this.filesView();
    this.pick(t.view || (t.kind === "id" ? "screen" : "term"), true);
    this.open();
  }

  state(text, dot) {
    $("state").textContent = text;
    $("state").title = text;
    $("dot").className = $("tabdot").className = "dot" + (dot ? " " + dot : "");
  }
  // Before the session is up, what is asked waits: the gateway reads the sign-in first, and a
  // refused one must not have more queued behind it.
  send(v) {
    if (!this.ready) { (this.queued ||= []).push(v); return; }
    if (this.ws?.readyState === 1) this.ws.send(JSON.stringify(v));
  }

  open() {
    const t = this.t;
    const ws = this.ws = new WebSocket(WS);
    ws.binaryType = "arraybuffer";
    const [w, hh] = this.want();
    const size = { w, h: hh, cols: this.term.cols, rows: this.term.rows };
    ws.onopen = () => {
      if (t.kind === "id") ws.send(JSON.stringify({ id: t.address, password: t.password, code: t.code || undefined, ...size }));
      else ws.send(JSON.stringify({ gateway: gatewayAuth(), open: { host: t.address, port: t.port ? +t.port : undefined, password: t.password || undefined }, ...size }));
    };
    ws.onmessage = m => this.message(m);
    ws.onerror = () => {};
    ws.onclose = () => this.closed();
  }

  // The connection went: back to the page if it never came up, else say so and offer to try again.
  closed(why) {
    if (this.ended || this.lost) return;
    // A try at coming back that did not get through: the next one, a while later.
    if (!this.ready && this.t.retry) { this.lost = true; this.retry(why, this.t.retry); return; }
    if (!this.ready) { this.leave(why || "The connection could not be made."); return; }
    this.lost = true;
    this.ready = false;
    this.state("Connection lost", "down");
    this.releaseKeys?.();
    this.retry(why, 0);
  }
  // Comes back by itself, as the app does: after 2, 4, 8... seconds (30 at most), eight times;
  // the banner counts down, and Reconnect tries at once.
  retry(why, done) {
    const text = why || "The connection was lost.";
    $("banner").hidden = false;
    clearInterval(this.retryTimer);
    if (done >= 8) { $("bannertext").textContent = text; $("again").focus(); return; }
    let left = Math.min(30, 2 ** (done + 1));
    const tick = () => {
      if (current !== this) { clearInterval(this.retryTimer); return; }
      $("bannertext").textContent = text + " Trying again in " + left + " s…";
      if (left-- <= 0) { clearInterval(this.retryTimer); connect({ ...this.t, view: this.view, retry: done + 1 }); }
    };
    tick();
    this.retryTimer = setInterval(tick, 1000);
  }
  leave(error, password) {
    const t = this.t;
    this.close();
    current = null;
    $("client").hidden = true;
    $("home").hidden = false;
    document.title = "SSH Remote Web";
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    renderSaved();
    if (password) {
      $("addr").value = t.kind === "id" ? spaced(t.address) : t.address;
      $("port").value = t.port || "";
      refreshForm();
    }
    fail(error || "");
    // A computer with two-step sign-in took the password and wants the app's code.
    const code = /authenticator|wrong code/.test(error || "");
    $("code").hidden = !code;
    (code ? $("code") : error ? $("pw") : $("addr")).focus();
  }
  close() {
    this.ended = true;
    clearInterval(this.retryTimer);
    this.sound?.close();
    this.sound = null;
    this.voice?.close(); this.callPlayer?.close();
    this.voice = this.callPlayer = null;
    this.recorder?.stop();
    $("views").classList.remove("split");
    try { this.ws?.close(); } catch { /* gone */ }
    this.term?.dispose();
    this.unbind?.();
    $("stools").textContent = "";
    $("xfers").textContent = "";
    $("ftable").textContent = "";
    $("fsel").textContent = "";
  }

  message(m) {
    if (typeof m.data !== "string") {
      const b = new Uint8Array(m.data);
      if (b[0] === 1) {
        this.term.write(b.subarray(1));
        // The last of what was printed, to tell why a shell ended.
        this.tail = ((this.tail || "") + new TextDecoder().decode(b.subarray(1))).slice(-2000);
      }
      else if (b[0] === 4) this.show(m.data);
      else if (b[0] === 6) this.sound?.packet(b.subarray(1));
      else if (b[0] === 7) this.callPlayer?.packet(b.subarray(1));
      else if (b[0] === 5) {
        saveBlob(new Blob([b.subarray(1)], { type: "image/png" }), "screen-" + new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-") + ".png");
        toast("A lossless picture of the screen was saved.");
      }
      else if (b[0] === 3) this.chunk(new DataView(m.data).getUint32(1, true), b.slice(5));
      // A picture copied on the host; none (too large to come over) still says the host's clipboard is newer.
      else if (b[0] === 8) { this.hostTurn = true; this.hostAt = Date.now(); if (b.length > 1) this.clipImageIn(new Blob([b.subarray(1)], { type: "image/png" })); else this.hostImage = null; }
      else if (b[0] === 9) this.pointerShape(m.data);
      return;
    }
    const v = JSON.parse(m.data);
    if (v.status) this.state(v.status);
    if (v.error && !this.ready) {
      // Signed out: the gateway forgot this browser.
      if (v.signin) signOut();
      this.ended = true;
      this.leave(v.error);
      return;
    }
    if (v.ready) {
      this.ready = true;
      for (const q of this.queued?.splice(0) || []) this.send(q);
      this.state(v.ready.screen ? v.ready.screen[0] + "×" + v.ready.screen[1] : "Connected", "up");
      this.connected = true;
      if (v.ready.screen) this.agentUp(v.ready.screen);
      // An ID is in once ready; an SSH host once its shell stays up or its agent answers.
      if (this.t.kind === "id") this.remember();
      else this.keepTimer = setTimeout(() => this.remember(), 5000);
      if (this.view === "term") this.term.focus();
      if (this.t.monitor !== undefined) this.send({ monitor: this.t.monitor });
      this.screenTools();
    }
    if (v.agent) this.agentUp(v.agent.screen);
    if (v.agentError) this.agentDown(v.agentError);
    if (v.screen) { this.desk = v.screen; this.figures(); }
    if (v.hostPointer !== undefined) { this.hostPtrAt = v.hostPointer; this.pointerLook(); }
    if (v.exit !== undefined) {
      this.shellEnded = true;
      this.term.write("\r\n\x1b[2m[" + (v.exit || "the shell ended") + " — press a key for a new one]\x1b[0m\r\n");
      // ssh refused the sign-in: said plainly, with the way back to the password.
      if (this.t.kind !== "id" && !this.kept && /permission denied|authentication failed|too many authentication/i.test(this.tail || "")) {
        clearTimeout(this.keepTimer);
        this.lost = true;
        $("bannertext").textContent = "The SSH host refused the sign-in: check the user name and the password.";
        $("again").textContent = "Change the password";
        $("again").onclick = () => this.leave("The SSH host refused the sign-in: check the user name and the password.", true);
        $("banner").hidden = false;
      }
    }
    if (v.dir) this.dir(v.dir);
    if (v.opDone) this.opDone(v.opDone);
    if (v.fetch) this.fetchStart(v.fetch);
    if (v.fetched) this.fetchEnd(v.fetched);
    if (v.fetchDone !== undefined) this.fetchDone(v.fetchDone);
    if (v.progress) this.progress(v.progress[0], v.progress[1]);
    if (v.uploaded) this.uploaded(v.uploaded);
    if (v.clipboard !== undefined) this.clipboardIn(v.clipboard);
    if (v.chat !== undefined) this.chatIn(v.chat);
    if (v.windows) this.windowPick(v.windows);
    if (v.sharing) { this.sharing = v.sharing; toast("Sharing " + v.sharing.text + "."); this.screenTools(); }
    if (v.monitors) { this.monitors = v.monitors; this.screenTools(); }
    if (v.modes) { this.modes = v.modes; this.screenTools(); }
    if (v.qualities) {
      this.qualities = v.qualities;
      // The quality chosen last time, asked for again.
      const saved = store.get("picture", null);
      if (saved) this.send(saved);
    }
    if (v.quality) { this.quality = v.quality; this.figures(); }
    if (v.stats) { this.stats = v.stats; this.figures(); }
    if (v.notice) toast(v.notice, { bad: /fail|error|cannot|denied|does not let/i.test(v.notice) });
    if (v.host) {
      this.host = v.host;
      const at = this.hostWait;
      this.hostWait = null;
      if (at && this.computerMenu) this.computerMenu(at);
    }
    if (v.allowed) this.allowed = v.allowed;
    if (typeof v.call === "number") this.callState(v.call);
    if (v.end || v.error) {
      const why = v.end || v.error;
      this.term.write("\r\n\x1b[31m[" + why + "]\x1b[0m\r\n");
      this.closed(why);
    }
  }

  // Kept on the page's list once it worked, with the password if asked to.
  async remember() {
    if (this.kept) return;
    this.kept = true;
    clearTimeout(this.keepTimer);
    const t = this.t;
    // It worked: what was typed does not wait in the form for the next person at this browser.
    $("pw").value = "";
    $("gatepw").value = "";
    const entry = { key: t.key || keyOf(t), kind: t.kind, address: t.address, port: t.port || "", name: t.name || null, last: Date.now(), view: this.view, pw: t.kept || null };
    if (t.keep && t.password) {
      try { entry.pw = await seal(t.password); } catch { toast("This browser cannot keep the password safely; it was not saved.", { bad: true }); }
    }
    save(entry);
  }

  // ----- The toolbar, full screen and the folded tab
  chrome() {
    setButton($("full"), "full");
    setButton($("fold"), "caretup");
    setButton($("bye"), "logout", "Close");
    for (const b of document.querySelectorAll("#bar [data-view]")) {
      b.onclick = () => this.pick(b.dataset.view);
      if (!b.querySelector("svg")) b.prepend(icon({ term: "terminal", screen: "monitor", files: "folder" }[b.dataset.view]));
    }
    if (!$("tab").querySelector("svg")) $("tab").append(icon("caretdown"));
    const fold = on => {
      $("client").classList.toggle("folded", on);
      setTimeout(() => this.relayout(), 0);
    };
    $("full").onclick = () => document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen().catch(() => {});
    const fs = () => {
      const on = !!document.fullscreenElement;
      setButton($("full"), on ? "unfull" : "full");
      $("full").title = on ? "Leave full screen" : "Full screen";
      fold(on);
      // Esc, Win and Alt+Tab go to the host while its screen fills this one.
      if (on && this.view === "screen") navigator.keyboard?.lock?.().catch(() => {});
      else navigator.keyboard?.unlock?.();
    };
    document.addEventListener("fullscreenchange", fs);
    $("fold").onclick = () => fold(true);
    $("tab").onclick = () => fold(false);
    $("bye").onclick = () => this.leave();
    // The chat: opened from the screen's tools, closed here; a message typed goes on Enter.
    setButton($("chatx"), "x");
    $("chatx").onclick = () => this.chatShow(false);
    $("chatform").onsubmit = e => {
      e.preventDefault();
      const text = $("chatin").value.trim();
      if (!text) return;
      this.send({ chat: text });
      this.chatLines.push({ you: true, text, at: new Date() });
      $("chatin").value = "";
      this.chatRender();
    };
    $("chat").hidden = true;
    this.chatRender();
    $("again").textContent = "Reconnect";
    $("again").onclick = () => { clearInterval(this.retryTimer); connect({ ...this.t, view: this.view, retry: 0 }); };
    $("home2").onclick = () => this.leave();
    const resize = () => this.relayout();
    addEventListener("resize", resize);
    const old = this.unbind;
    this.unbind = () => { old?.(); document.removeEventListener("fullscreenchange", fs); removeEventListener("resize", resize); $("client").classList.remove("folded"); };
  }

  pick(view, first) {
    this.view = view;
    for (const b of document.querySelectorAll("#bar [data-view]")) b.classList.toggle("on", b.dataset.view === view);
    for (const [k, id] of [["term", "vterm"], ["screen", "vscreen"], ["files", "vfiles"]]) $(id).classList.toggle("on", k === view);
    $("stools").hidden = view !== "screen";
    if (!first || view !== "term") this.send({ show: view === "term" ? "terminal" : view });
    if (view === "term") { this.fit.fit(); this.term.focus(); }
    if (view === "screen") { this.send({ view: this.want() }); this.keyboard(); this.screenWait(); }
    if (view === "files") { if (!this.files.wanted) { this.files.wanted = true; this.list(""); } else $("ftable").focus(); this.filesWait(); }
    if (this.ready) update(this.t.key || keyOf(this.t), { view });
    if (this.desk) this.figures();
    this.splitView();
  }
  relayout() {
    if (this.view === "term" || $("views").classList.contains("split")) this.fit.fit();
    clearTimeout(this.rt);
    this.rt = setTimeout(() => { if (this.view === "screen") this.send({ view: this.want() }); }, 250);
  }

  // The host's pointer, drawn by this page: its picture is the page's own pointer over the screen
  // (it moves with the mouse, no round trip behind it), and shows where the host has it while
  // the mouse is elsewhere or the screen is only looked at.
  pointerShape(data) {
    const dv = new DataView(data), hx = dv.getUint16(1, true), hy = dv.getUint16(3, true);
    const r = new FileReader();
    r.onload = () => { this.ptr = { url: r.result, hx, hy }; this.pointerLook(); };
    r.readAsDataURL(new Blob([new Uint8Array(data, 5)], { type: "image/png" }));
  }
  pointerLook() {
    const canvas = $("canvas");
    if (!canvas || !this.ptr) return;
    canvas.style.cursor = this.viewOnly ? "" : `url(${this.ptr.url}) ${this.ptr.hx} ${this.ptr.hy}, default`;
    let img = $("hostptr");
    if (!img) {
      img = h("img", { id: "hostptr", alt: "" });
      img.style.cssText = "position:fixed;pointer-events:none;z-index:5;display:none";
      document.body.append(img);
    }
    if (img.getAttribute("src") !== this.ptr.url) img.src = this.ptr.url;
    const at = this.hostPtrAt;
    if (!at || this.view !== "screen" || (this.overCanvas && !this.viewOnly)) { img.style.display = "none"; return; }
    const r = canvas.getBoundingClientRect();
    let x, y;
    if (this.viewFit === "fit") {
      const k = Math.min(r.width / canvas.width, r.height / canvas.height);
      x = r.left + (r.width - canvas.width * k) / 2 + at[0] * k; y = r.top + (r.height - canvas.height * k) / 2 + at[1] * k;
    } else {
      x = r.left + at[0] * r.width / canvas.width; y = r.top + at[1] * r.height / canvas.height;
    }
    img.style.left = (x - this.ptr.hx) + "px";
    img.style.top = (y - this.ptr.hy) + "px";
    img.style.display = "";
  }
  agentUp(screen) {
    if (this.ready && this.t.kind !== "id") this.remember();
    this.agent = "up";
    this.agentWhy = null;
    if (screen) this.state(screen[0] + "×" + screen[1], "up");
    this.screenWait();
    this.filesWait();
  }
  agentDown(why) {
    this.agent = "failed";
    this.agentWhy = why;
    if (this.ready) this.state("Terminal only", "up");
    this.screenWait();
    this.filesWait();
  }

  // ----- Terminal
  terminal() {
    $("vterm").textContent = "";
    const term = this.term = new Terminal({ cursorBlink: true, fontSize: 14, scrollback: 10000, allowProposedApi: false,
      fontFamily: "'Cascadia Mono', 'JetBrains Mono', 'DejaVu Sans Mono', Menlo, Consolas, monospace",
      theme: { background: "#0b0d11", selectionBackground: "#2b59c380" } });
    this.fit = new FitAddon.FitAddon();
    term.loadAddon(this.fit);
    term.open($("vterm"));
    this.fit.fit();
    const enc = new TextEncoder();
    // Ctrl+Shift+C and V copy and paste, as in a terminal; Ctrl+C stays the shell's.
    term.attachCustomKeyEventHandler(e => {
      if (e.type !== "keydown" || !e.ctrlKey || !e.shiftKey) return true;
      if (e.code === "KeyC") { const s = term.getSelection(); if (s) navigator.clipboard?.writeText(s).catch(() => {}); return false; }
      if (e.code === "KeyV") { navigator.clipboard?.readText().then(t => t && term.paste(t), () => toast("This browser did not let the page read the clipboard: use Ctrl+V.", { bad: true })); return false; }
      return true;
    });
    term.onData(d => {
      if (!this.ready) return;
      // After the shell ended, a key starts another.
      if (this.shellEnded) { this.shellEnded = false; term.reset(); this.send({ shell: [term.cols, term.rows] }); return; }
      const bytes = enc.encode(d), f = new Uint8Array(bytes.length + 1);
      f[0] = 1; f.set(bytes, 1);
      this.ws.send(f);
    });
    term.onResize(s => this.send({ resize: [s.cols, s.rows] }));
  }

  // ----- Screen
  want() {
    // Zoomed, the host's own size (the agent never sends more); else the view's, in pixels.
    if (typeof this.viewFit === "number") return [8192, 8192];
    const c = $("vscreen");
    return [Math.max(64, Math.round(c.clientWidth * devicePixelRatio)) || 1280, Math.max(64, Math.round(c.clientHeight * devicePixelRatio)) || 720];
  }
  // How the picture fits the view, kept for next time.
  setFit(f) {
    this.viewFit = f;
    store.set("fit", f);
    this.layout();
    this.send({ view: this.want() });
    this.screenTools();
  }
  screenWait() {
    const w = $("swait");
    w.textContent = "";
    if (this.pictured) { w.hidden = true; return; }
    w.hidden = false;
    if (this.agent === "failed") {
      w.append(h("div", {}, h("div", { class: "bad", text: "The screen could not be reached." }), h("div", { text: this.agentWhy || "" })));
    } else {
      w.append(h("div", {}, h("div", { class: "spin" }), h("div", { text: this.agent === "up" ? "Waiting for the picture…" : "Connecting for the screen…" })));
    }
  }
  screen() {
    const canvas = $("canvas");
    this.pictured = false;
    const ctx = canvas.getContext("2d");
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    // How the picture sits in the view: fitted (with bars where the shapes differ), stretched to
    // fill it, or at a zoom (1: a pixel of the host's to a point here), panned by scrolling.
    this.viewFit = store.get("fit", "fit");
    this.layout = () => {
      const z = typeof this.viewFit === "number" ? this.viewFit : 0;
      $("vscreen").classList.toggle("zoomed", !!z);
      canvas.style.objectFit = this.viewFit === "stretch" ? "fill" : "contain";
      canvas.style.width = z ? canvas.width * z + "px" : "";
      canvas.style.height = z ? canvas.height * z + "px" : "";
      // A picture of another size sits elsewhere in the same box.
      this.pointerLook();
    };
    this.layout();
    // The host's pointer drawn where the mouse is away stays over its place as the picture
    // moves or changes size (a notice above it, the window resized, a zoomed picture panned).
    new ResizeObserver(() => this.pointerLook()).observe(canvas);
    $("vscreen").addEventListener("scroll", () => this.pointerLook(), { passive: true });
    window.addEventListener("resize", () => this.pointerLook());
    this.show = painter(canvas, () => { if (!this.pictured) { this.pictured = true; this.screenWait(); } }, seq => this.send({ drawn: seq }), () => this.layout());
    const send = v => { if (!this.viewOnly) this.send(v); };
    // Pointer positions in the picture's pixels: past the bars around a fitted picture, or
    // across the whole of a stretched or zoomed one.
    const point = e => {
      const r = canvas.getBoundingClientRect();
      let x, y;
      if (this.viewFit === "fit") {
        const k = Math.min(r.width / canvas.width, r.height / canvas.height);
        const ox = (r.width - canvas.width * k) / 2, oy = (r.height - canvas.height * k) / 2;
        x = Math.floor((e.clientX - r.left - ox) / k); y = Math.floor((e.clientY - r.top - oy) / k);
      } else {
        x = Math.floor((e.clientX - r.left) * canvas.width / r.width); y = Math.floor((e.clientY - r.top) * canvas.height / r.height);
      }
      return x >= 0 && y >= 0 && x < canvas.width && y < canvas.height ? [x, y] : null;
    };
    const BUTTON = [1, 2, 3, 8, 9];
    // The button pressed here is let go there however the press ends: released, or taken by the
    // browser (a finger's drag turned into scrolling or zooming).
    let down = null;
    canvas.style.touchAction = "none";
    // Fingers, by pointer: a tap clicks, a long press right-clicks, a drag drags with the left
    // button, two fingers scroll and pinch to zoom the picture here.
    const fingers = new Map();
    let finger = null;   // the one finger's gesture: { at, start, drag, long, timer }
    let pair = null;     // two fingers': { mid, dist, zoom, rows }
    const mid = () => { const [a, b] = [...fingers.values()]; return [(a.x + b.x) / 2, (a.y + b.y) / 2, Math.hypot(a.x - b.x, a.y - b.y)]; };
    const click = (p, b) => { send({ move: p }); send({ button: [b, 1] }); send({ button: [b, 0] }); };
    const touchDown = e => {
      fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      try { canvas.setPointerCapture(e.pointerId); } catch { /* a pointer the browser no longer has */ }
      if (fingers.size === 1) {
        const p = point(e);
        finger = { at: p, start: [e.clientX, e.clientY], drag: false, long: false, timer: null };
        if (p) finger.timer = setTimeout(() => { finger.long = true; click(p, 3); navigator.vibrate?.(15); }, 550);
      } else if (fingers.size === 2) {
        if (finger?.drag) send({ button: [1, 0] });
        if (finger) clearTimeout(finger.timer);
        finger = null;
        const r = canvas.getBoundingClientRect();
        const [x, y, dist] = mid();
        pair = { mid: [x, y], dist, zoom: typeof this.viewFit === "number" ? this.viewFit : r.width / canvas.width, rows: 0 };
      }
    };
    const touchMove = e => {
      if (!fingers.has(e.pointerId)) return;
      fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (finger && fingers.size === 1) {
        const p = point(e);
        if (!finger.drag && !finger.long && Math.hypot(e.clientX - finger.start[0], e.clientY - finger.start[1]) > 8) {
          clearTimeout(finger.timer);
          finger.drag = true;
          if (finger.at) { send({ move: finger.at }); send({ button: [1, 1] }); }
        }
        if (finger.drag && p) send({ move: p });
      } else if (pair && fingers.size === 2) {
        const [x, y, dist] = mid();
        // Fingers up scroll the page down, as on a phone; every 30 points a step.
        pair.rows += (pair.mid[1] - y) / 30;
        // A hair over, so 90 points made of thirds of a step are 3 steps, not 2.999.
        const steps = Math.trunc(pair.rows + Math.sign(pair.rows) * 1e-9);
        if (steps) { send({ wheel: [0, steps] }); pair.rows -= steps; }
        pair.mid = [x, y];
        const z = Math.min(4, Math.max(0.25, pair.zoom * dist / pair.dist));
        if (Math.abs(dist / pair.dist - 1) > 0.12) { this.viewFit = Math.round(z * 20) / 20; this.layout(); }
      }
    };
    const touchUp = e => {
      if (!fingers.delete(e.pointerId)) return;
      if (finger && fingers.size === 0) {
        clearTimeout(finger.timer);
        if (finger.drag) send({ button: [1, 0] });
        else if (!finger.long && finger.at) click(finger.at, 1);
        finger = null;
      }
      if (fingers.size < 2) pair = null;
    };
    canvas.addEventListener("pointerenter", () => { this.overCanvas = true; this.pointerLook(); });
    canvas.addEventListener("pointerleave", () => { this.overCanvas = false; this.pointerLook(); });
    canvas.onpointermove = e => {
      if (!this.overCanvas) { this.overCanvas = true; this.pointerLook(); }
      if (e.pointerType === "touch") { touchMove(e); return; }
      // The host does not echo a move back: its pointer is where it was sent.
      const p = point(e); if (p) { send({ move: p }); this.hostPtrAt = p; }
    };
    canvas.onpointerdown = e => {
      if (e.pointerType === "touch") { e.preventDefault(); touchDown(e); return; }
      this.keyboard();
      const p = point(e); if (!p) return;
      canvas.setPointerCapture(e.pointerId);
      down = BUTTON[e.button] || 1;
      this.hostPtrAt = p;
      send({ move: p }); send({ button: [down, 1] });
      e.preventDefault();
    };
    canvas.onpointerup = e => {
      if (e.pointerType === "touch") { touchUp(e); return; }
      const b = BUTTON[e.button] || 1;
      if (b === down) down = null;
      send({ button: [b, 0] });
    };
    canvas.onpointercancel = e => {
      if (e.pointerType === "touch") { touchUp(e); return; }
      if (down !== null) send({ button: [down, 0] });
      down = null;
    };
    canvas.oncontextmenu = e => e.preventDefault();
    let wheel = [0, 0];
    canvas.onwheel = e => {
      e.preventDefault();
      const unit = e.deltaMode === 0 ? 40 : e.deltaMode === 1 ? 1 : 0.1;
      wheel[0] += e.deltaX / unit; wheel[1] += e.deltaY / unit;
      const sx = Math.trunc(wheel[0]), sy = Math.trunc(wheel[1]);
      if (sx || sy) { send({ wheel: [sx, sy] }); wheel = [wheel[0] - sx, wheel[1] - sy]; }
    };
    // The keyboard goes through a hidden text field, which input methods and phones' on-screen
    // keyboards can type into. A key comes up as the key it went down as, by its place on the
    // keyboard: let go after Shift, an "A" must not stay down as an "a" comes up.
    const kbd = $("kbd");
    const held = new Map();
    // Ctrl+V there pastes what is copied here: the browser's paste brings it (a picture, files
    // or text), it goes onto the host's clipboard, then Ctrl+V goes to the host.
    // Keys typed while a paste goes over wait for it, so they come after what is pasted.
    const swallowed = new Set();
    let pasteWait = null, later = null;
    const keySend = m => later ? later.push(m) : send(m);
    const settle = () => { const q = later; later = null; clearTimeout(this.pasteGiveUp); for (const m of q || []) send(m); };
    const pasteThere = () => {
      const ctrl = [...held.values()].some(k => k === 0xffe3 || k === 0xffe4);
      if (!ctrl) send({ key: [K.ctrl, 1] });
      tap(0x76);
      if (!ctrl) send({ key: [K.ctrl, 0] });
      settle();
    };
    const pasting = () => { later ||= []; clearTimeout(this.pasteGiveUp); this.pasteGiveUp = setTimeout(settle, 10000); };
    // One handler for the field, whichever connection has it now.
    kbd.onpaste = e => {
      e.preventDefault();
      clearTimeout(pasteWait); pasteWait = null;
      if (this.viewOnly) { settle(); return; }
      pasting();
      this.pasteData(e.clipboardData, pasteThere, settle);
    };
    kbd.onkeydown = e => {
      // An input method is composing (Korean, Japanese, Chinese) or a phone keyboard sends no
      // key: its text comes when it is done.
      if (e.isComposing || e.key === "Process" || e.key === "Unidentified" || e.keyCode === 229) return;
      if (e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey && e.code === "KeyV" && !this.viewOnly) {
        // Left to the browser, which pastes into the field; a browser that does not still pastes there.
        swallowed.add(e.code);
        pasting();
        clearTimeout(pasteWait);
        pasteWait = setTimeout(() => { pasteWait = null; pasteThere(); }, 400);
        return;
      }
      const k = keysym(e); if (!k) return;
      e.preventDefault();
      // "Win + the next key" from the keys menu: this key goes with Win held.
      if (this.winNext && !SIDED[e.key]) {
        this.winNext = false;
        send({ key: [K.win, 1] }); send({ key: [k, 1] }); send({ key: [k, 0] }); send({ key: [K.win, 0] });
        return;
      }
      held.set(e.code || e.key, k); keySend({ key: [k, 1] });
    };
    kbd.onkeyup = e => {
      const id = e.code || e.key;
      if (swallowed.delete(id)) { e.preventDefault(); return; }
      const k = held.get(id) ?? keysym(e); if (!k) return;
      e.preventDefault(); held.delete(id); keySend({ key: [k, 0] });
    };
    const tap = k => { send({ key: [k, 1] }); send({ key: [k, 0] }); };
    // Text typed in as text, each character as the key that makes it.
    const typeText = t => { for (const ch of t) { const c = ch.codePointAt(0); tap(c < 0x100 ? c : 0x01000000 + c); } };
    // One space stays in the field so a phone's Backspace has something to delete, and says so.
    const reset = () => { kbd.value = " "; kbd.setSelectionRange(1, 1); };
    kbd.addEventListener("compositionend", e => { if (e.data) typeText(e.data); reset(); });
    kbd.addEventListener("input", e => {
      if (e.isComposing) return;
      if (e.inputType === "insertText" && e.data) typeText(e.data);
      else if (e.inputType === "insertLineBreak") tap(0xff0d);
      else if (e.inputType === "deleteContentBackward") tap(0xff08);
      reset();
    });
    reset();
    // Keys still down when the page loses focus come up, so none sticks on the host.
    this.releaseKeys = () => { for (const k of held.values()) this.send({ key: [k, 0] }); held.clear(); };
    // Keys still down when the field loses the keyboard come up; a V left to a paste is forgotten.
    kbd.onblur = () => { swallowed.clear(); later = null; this.releaseKeys(); };
    // Files dropped on the screen go to the host's Downloads folder, as in the app.
    const scr = $("vscreen");
    scr.ondragover = e => { if (!this.viewOnly) e.preventDefault(); };
    scr.ondrop = async e => {
      e.preventDefault();
      if (this.viewOnly) return;
      const list = await dropped(e.dataTransfer);
      if (list.length) { this.queueUploads(list, ""); toast("Sending to the host's Downloads folder; the Files view shows how far it is."); }
    };
    // The tools as buttons or as one menu, by the window's width.
    const narrow = matchMedia("(max-width: 760px)");
    const retool = () => this.screenTools();
    narrow.addEventListener("change", retool);
    const old = this.unbind;
    this.unbind = () => { old?.(); narrow.removeEventListener("change", retool); };
    // What was copied here goes to the host as the screen takes the keyboard, once the browser
    // lets the page read the clipboard (asked the first time "Paste my clipboard" is used).
    kbd.onfocus = async () => {
      if (this.viewOnly || !navigator.clipboard?.readText || !navigator.permissions) return;
      try {
        const p = await navigator.permissions.query({ name: "clipboard-read" });
        if (p.state !== "granted") return;
        const text = await navigator.clipboard.readText();
        if (text && text !== this.sentClip && text !== this.hostClip) { this.sentClip = text; this.send({ clipboard: text }); }
      } catch { /* not this browser */ }
    };
    this.screenTools();
  }
  combo(keys) {
    if (this.viewOnly) { toast("View only: nothing is sent to the host."); return; }
    for (const k of keys) this.send({ key: [k, 1] });
    for (const k of [...keys].reverse()) this.send({ key: [k, 0] });
    this.keyboard();
  }
  // The picture's quality, kept for the next connection from this browser.
  setQuality(v) {
    store.set("picture", v);
    this.send(v);
  }
  // Colours, picture size and frame rate of one's own: from black and white at a tenth of the
  // size and one picture a second, the least traffic, to full colour at full size and 60.
  customQuality() {
    const Q = this.quality || { mode: "true", scale: 100, fps: 30 };
    const MODES = [["mono", "Black and white"], ["gray4", "4 greys"], ["gray16", "16 greys"], ["16c", "16 colours"], ["256c", "256 colours"], ["true", "Full colour"]];
    const box = $("modal").firstElementChild;
    box.textContent = "";
    const mode = h("select", { "aria-label": "Colours" }, ...MODES.map(([v, t]) => h("option", { value: v, text: t, selected: v === Q.mode })));
    const slider = (label, min, max, step, value, unit) => {
      const out = h("output", { text: value + unit });
      const r = h("input", { type: "range", min, max, step, value, "aria-label": label });
      r.oninput = () => { out.textContent = r.value + unit; };
      return [h("label", { class: "range" }, h("span", { text: label }), r, out), r];
    };
    // The picture sent, not the screen: its resolution is set in the quality menu.
    const [scaleRow, scale] = slider("Picture size", 10, 100, 5, Q.scale || 100, "%");
    const [fpsRow, fps] = slider("Frame rate", 1, 120, 1, Q.fps || 30, " fps");
    const finish = ok => {
      $("modal").hidden = true;
      removeEventListener("keydown", keys, true);
      if (ok) this.setQuality({ quality: "custom", mode: mode.value, scale: +scale.value, fps: +fps.value });
    };
    const keys = e => { if (e.key === "Escape") { e.preventDefault(); finish(false); } };
    const yes = h("button", { type: "button", class: "btn primary", text: "Use it", onclick: () => finish(true) });
    box.append(h("h2", { text: "Picture quality of your own" }),
      h("p", { text: "Fewer colours, a smaller picture and fewer pictures a second send less: black and white at a tenth of the size and 1 fps is the least." }),
      h("label", { class: "range" }, h("span", { text: "Colours" }), mode), scaleRow, fpsRow,
      h("div", { class: "acts" }, h("button", { type: "button", class: "btn solid", text: "Cancel", onclick: () => finish(false) }), yes));
    $("modal").hidden = false;
    addEventListener("keydown", keys, true);
    mode.focus();
  }
  // The status line: the host's screen, the quality in effect, and the link's figures.
  figures() {
    const parts = [];
    if (this.desk) parts.push(this.desk[0] + "×" + this.desk[1]);
    if (this.quality && this.view === "screen") parts.push(this.quality.text);
    if (this.stats && this.view === "screen") parts.push(this.stats.kbps.toFixed(this.stats.kbps < 10 ? 1 : 0) + " KB/s", Math.round(this.stats.rtt) + " ms");
    this.state(parts.join(" · "), "up");
  }
  // Each of the host's other screens in a browser window of its own, reaching the host the same
  // way (what it needs is handed over between the windows, never put in the address). Where the
  // browser may place windows (Chrome, Edge), each opens on the monitor of the same number here.
  async spread() {
    // Its screens, not its cameras.
    const others = this.monitors.list.map((m, i) => m.name.startsWith("Camera: ") ? -1 : i).filter(i => i >= 0 && i !== this.monitors.current);
    let screens = null;
    try { screens = window.getScreenDetails ? (await window.getScreenDetails()).screens : null; } catch { /* not allowed */ }
    for (const [k, i] of others.entries()) {
      const s = screens?.[i];
      const features = s ? `left=${s.availLeft},top=${s.availTop},width=${s.availWidth},height=${s.availHeight}` : `left=${80 + 40 * k},top=${80 + 40 * k},width=1100,height=720`;
      const win = window.open(location.pathname + "#screen=" + i, "_blank", features);
      if (!win) { toast("The browser kept the new window from opening: allow pop-ups for this page.", { bad: true }); return; }
      spreads.set(win, { t: { ...this.t, view: "screen", monitor: i, retry: 0 }, gateway: gatewayAuth() });
    }
    if (screens && screens.length > 1) toast("Press the full-screen button in each window to fill its monitor.");
  }
  // The terminal below the screen, as the app shows it, while the screen is the view.
  splitView() {
    const on = !!this.split && this.view === "screen";
    $("views").classList.toggle("split", on);
    setTimeout(() => {
      if (on) this.fit.fit();
      if (this.view === "screen") this.send({ view: this.want() });
    }, 0);
  }
  // The host's windows to choose from: one, or every window of its program.
  windowPick(w) {
    const box = $("modal").firstElementChild;
    box.textContent = "";
    const finish = () => { $("modal").hidden = true; removeEventListener("keydown", keys, true); };
    const keys = e => { if (e.key === "Escape") { e.preventDefault(); finish(); } };
    const list = h("div", { class: "picklist" });
    if (w.error) list.append(h("p", { class: "bad", text: w.error }));
    for (const win of w.list || []) {
      const one = h("button", { type: "button", class: "btn", text: "This window", onclick: () => { finish(); this.send({ share: { kind: "window", id: win.id } }); } });
      const all = h("button", { type: "button", class: "btn", text: "All of " + (win.app || "its program"), onclick: () => { finish(); this.send({ share: { kind: "app", id: win.id } }); } });
      list.append(h("div", { class: "pick" }, h("div", { class: "grow" }, h("b", { text: win.title || "(no title)" }), h("small", { text: (win.app ? win.app + " · " : "") + win.w + "×" + win.h })), one, all));
    }
    if (!w.error && !(w.list || []).length) list.append(h("p", { text: "No window to share on its own." }));
    box.append(h("h2", { text: "Share one window or program" }),
      h("p", { text: "Its menus and dialogs show with it; clicks outside it stay here." }), list,
      h("div", { class: "acts" }, h("button", { type: "button", class: "btn solid", text: "Cancel", onclick: finish })));
    $("modal").hidden = false;
    addEventListener("keydown", keys, true);
  }
  // A message from the person at the host: in the chat, and said when the chat is closed.
  chatIn(text) {
    this.chatLines.push({ you: false, text, at: new Date() });
    if ($("chat").hidden) {
      this.unread++;
      toast("Message from the host: " + text);
      this.screenTools();
    }
    this.chatRender();
  }
  chatShow(on) {
    $("chat").hidden = !on;
    if (on) { this.unread = 0; this.chatRender(); $("chatin").focus(); } else if (this.view === "screen") this.keyboard();
    this.screenTools();
  }
  // Where the call is: answered there (voices both ways), not answered, or ended.
  async callState(state) {
    const was = this.call;
    this.call = state;
    if (state === 2) {
      this.callPlayer = soundPlayer();
      this.voice = await voiceSender(p => { if (this.ws?.readyState === 1) this.ws.send(new Uint8Array([7, ...p])); });
      toast(this.voice ? "In a call with the person at that computer." : "In a call: this browser cannot send your voice, but you hear them.");
    } else if (state === 0 || state === 3) {
      this.voice?.close(); this.callPlayer?.close();
      this.voice = this.callPlayer = null;
      if (state === 3) toast("They did not answer the call.");
      else if (was === 2) toast("The call ended.");
    }
    this.screenTools();
  }
  toggleCall() {
    if (this.call === 1 || this.call === 2) { this.send({ call: false }); this.callState(0); }
    else { this.call = 1; this.send({ call: true }); toast("Calling the person at that computer…"); this.screenTools(); }
  }
  // Records the screen as it shows here, into a video file the browser saves.
  toggleRecord() {
    if (this.recorder) { this.recorder.stop(); return; }
    const canvas = $("canvas");
    if (!canvas.captureStream || !window.MediaRecorder) { toast("This browser cannot record the screen.", { bad: true }); return; }
    const type = ["video/webm;codecs=vp9", "video/webm", "video/mp4"].find(t => MediaRecorder.isTypeSupported(t)) || "";
    const rec = new MediaRecorder(canvas.captureStream(15), type ? { mimeType: type } : {});
    const parts = [];
    rec.ondataavailable = e => { if (e.data.size) parts.push(e.data); };
    rec.onstop = () => {
      this.recorder = null;
      const blob = new Blob(parts, { type: rec.mimeType });
      const a = h("a", { href: URL.createObjectURL(blob), download: "sshremote-" + (this.t?.address || "screen") + "-" + new Date().toISOString().replace(/[:.]/g, "-") + (rec.mimeType.includes("mp4") ? ".mp4" : ".webm") });
      // Firefox saves only from a link in the page.
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(a.href), 60000);
      toast("The recording is saved.");
      this.screenTools();
    };
    rec.start(1000);
    this.recorder = rec;
    toast("Recording the screen; the button again stops it and saves the video.");
    this.screenTools();
  }
  chatRender() {
    const log = $("chatlog");
    log.textContent = "";
    if (!this.chatLines.length) log.append(h("div", { class: "empty", text: "Messages go to a chat window on the host's screen, and its answers come here." }));
    for (const m of this.chatLines) {
      const line = h("div", { class: "msg" + (m.you ? " you" : ""), text: m.text });
      line.append(h("small", { text: (m.you ? "You" : "Host") + ", " + m.at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) }));
      log.append(line);
    }
    log.scrollTop = log.scrollHeight;
  }
  // The screen takes the keyboard (on a phone, its on-screen keyboard opens).
  keyboard() {
    $("kbd").focus({ preventScroll: true });
  }
  screenTools() {
    const bar = $("stools");
    bar.textContent = "";
    bar.append(h("span", { class: "sep" }));
    // The menus, each opened under `at`.
    const keysMenu = at => menu([
      ["Ctrl+Alt+Del", () => { if (!this.viewOnly) this.send({ sas: 1 }); }],
      ["Lock the screen", () => this.combo([K.win, 0x6c]), { kbd: "Win+L" }],
      ["Start menu", () => this.combo([K.win]), { kbd: "Win" }],
      ["Show the desktop", () => this.combo([K.win, 0x64]), { kbd: "Win+D" }],
      ["Win + the next key", () => { this.winNext = true; toast("Press the key to go with Win."); this.keyboard(); }],
      ["Switch windows", () => this.combo([K.alt, K.tab]), { kbd: "Alt+Tab" }],
      ["Close the window", () => this.combo([K.alt, K.f4]), { kbd: "Alt+F4" }],
      ["Task Manager", () => this.combo([K.ctrl, K.shift, K.esc]), { kbd: "Ctrl+Shift+Esc" }],
      ["Run", () => this.combo([K.win, 0x72]), { kbd: "Win+R" }],
      ["File Explorer", () => this.combo([K.win, 0x65]), { kbd: "Win+E" }],
      ["Print Screen", () => this.combo([K.prt]), { kbd: "PrtSc" }],
    ], at);
    const clipMenu = at => menu([
      ["Paste my clipboard into the host", () => this.pasteHere()],
      ["Paste files there…", () => this.pasteFiles(), { disabled: this.viewOnly }],
      ["Type or paste text for the host…", async () => {
        const text = await ask({ title: "Send text to the host", text: "It goes on the host's clipboard; paste it there.", area: "Text", ok: "Send" });
        if (text) { this.send({ clipboard: text }); toast("On the host's clipboard."); }
      }],
      null,
      ["Copy the host's clipboard here", () => this.copyHostClip(), { disabled: this.hostClip === null }],
      ["Copy the host's picture here", () => this.copyHostImage(), { disabled: !this.hostImage }],
    ], at);
    // The screen's own resolution (its display mode), set back when this connection ends.
    const modesMenu = at => { const M = this.modes, same = (a, b) => a[0] === b[0] && a[1] === b[1]; menu(["Resolution of its screen",
      ...M.list.map(m => [m.join("×") + (same(m, M.own) ? " (its own)" : ""), () => this.send({ mode: m }), { on: same(m, M.current) }]),
      ...(same(M.current, M.own) ? [] : [null, ["Back to its own, " + M.own.join("×"), () => this.send({ mode: [0, 0] })]]),
    ], at); };
    // Read when opened: the quality changes after the bar is made (auto, the host's answer).
    const qualityMenu = at => { const Q = this.quality || {}; menu(["Picture quality",
      ["Auto (adapts to the connection)", () => this.setQuality({ quality: "auto" }), { on: !!Q.auto }],
      ...(this.qualities || []).map((p, i) => [p.text, () => this.setQuality({ quality: "preset", level: i }), { on: !Q.auto && Q.preset === p.name }]),
      ["Custom: colours, picture size, frame rate…", () => this.customQuality(), { on: Q.preset === "custom" }],
      ...(this.modes?.list.length ? [null, ["Resolution of its screen: " + this.modes.current.join("×") + "…", () => modesMenu(at)]] : []),
      null,
      "View",
      ["Fit to the window", () => this.setFit("fit"), { on: this.viewFit === "fit" }],
      ["Stretch to fill", () => this.setFit("stretch"), { on: this.viewFit === "stretch" }],
      ...[0.5, 0.75, 1, 1.5, 2].map(z => [z * 100 + "%" + (z === 1 ? " (the host's own size)" : ""), () => this.setFit(z), { on: this.viewFit === z }]),
      null,
      ["Repaint the whole screen", () => this.send({ refresh: 1 })],
    ], at); };
    const several = this.monitors && this.monitors.list.length > 1;
    // What of the host is shared: all of the screen, or one window or program, with its menus.
    const kind = this.sharing?.kind || 0;
    const shareMenu = at => menu(["Share",
      ["The whole screen", () => this.send({ share: { kind: "screen" } }), { on: kind === 0 }],
      ["The window in front", () => this.send({ share: { kind: "activeWindow" } }), { on: kind === 1 || kind === 3 }],
      ["The program in front, all its windows", () => this.send({ share: { kind: "activeApp" } }), { on: kind === 2 || kind === 4 }],
      ["One window or program…", () => this.send({ windows: 1 })],
    ], at);
    const camera = m => m.name.startsWith("Camera: ");
    const monitorMenu = at => menu([...this.monitors.list.map((m, i) =>
      [camera(m) ? m.name : (i + 1) + "  " + m.name + " (" + m.w + "×" + m.h + (m.primary ? ", primary" : "") + ")", () => this.send({ monitor: i }), { on: i === this.monitors.current }]),
      ...(this.monitors.list.filter(m => !camera(m)).length > 1 ? [null, ["All its screens, a window each", () => this.spread()]] : [])], at);
    // The computer itself: asked what it can do the first time, then opened where it was asked for.
    const computerMenu = this.computerMenu = at => {
      const H = this.host;
      if (!H) { this.hostWait = at; this.send({ host: { act: 0 } }); return; }
      if (!H.can) return menu(["The person at that computer does not let you do these."], at);
      const can = a => (H.can & (1 << a)) !== 0, on = a => (H.on & (1 << a)) !== 0;
      const only = a => !can(a) ? { disabled: true } : {};
      menu(["This computer",
        ["Lock it", () => this.send({ host: { act: 2, on: true } }), { icon: "lock", ...only(2) }],
        ["Restart it…", async () => {
          if (await ask({ title: "Restart the computer?", text: "Programs there close; unsaved work may be lost. The connection comes back when it is up, if remote access starts with it.", ok: "Restart now", danger: true }))
            this.send({ host: { act: 1, on: true } });
        }, { icon: "reload", ...only(1) }],
        null,
        ["Keep its keyboard and mouse out", () => this.send({ host: { act: 3, on: !on(3) } }), { on: on(3), ...only(3) }],
        ["Hide its screen", () => this.send({ host: { act: 4, on: !on(4) } }), { on: on(4), ...only(4) }],
        ["Lock it when I leave", () => this.send({ host: { act: 5, on: !on(5) } }), { on: on(5), ...only(5) }],
      ], at);
    };
    const viewOnly = () => {
      this.viewOnly = !this.viewOnly;
      if (this.viewOnly) this.releaseKeys();
      $("vscreen").classList.toggle("viewonly", this.viewOnly);
      this.pointerLook();
      toast(this.viewOnly ? "View only: keys and clicks stay here." : "Keys and clicks reach the host again.");
      this.screenTools();
    };
    // The host's own pixels, lossless, whatever the quality shown here.
    const shot = () => this.send({ snapshot: 1 });
    const pause = () => {
      this.paused = !this.paused;
      this.send({ pause: this.paused });
      toast(this.paused ? "Paused: no picture comes until you go on or fetch one." : "The picture goes on.");
      this.screenTools();
    };
    // A narrow screen (a phone): one button for all of it, so the bar keeps its right end.
    if (matchMedia("(max-width: 760px)").matches) {
      const more = button("dots", null, { title: "The screen's tools" });
      more.classList.toggle("on", this.viewOnly);
      more.onclick = () => {
        const items = [
          ["Keyboard", () => this.keyboard(), { icon: "keyboard" }],
          [this.unread ? "Chat (" + this.unread + " new)" : "Chat", () => this.chatShow($("chat").hidden), { icon: "chat", on: !$("chat").hidden }],
          ["Keys…", () => keysMenu(more), { icon: "keyboard" }],
          ["This computer…", () => computerMenu(more), { icon: "lock" }],
          ["Clipboard…", () => clipMenu(more), { icon: "clipboard" }],
          ["Picture quality…", () => qualityMenu(more), { icon: "sliders" }],
          ["Share…", () => shareMenu(more), { icon: "window", on: kind !== 0 }],
          ["Sound", sound, { icon: "sound", on: !!this.sound }],
          [this.call === 2 ? "Hang up" : "Call the person there", () => this.toggleCall(), { icon: "phone", on: this.call === 1 || this.call === 2 }],
          [this.recorder ? "Stop recording" : "Record the screen", () => this.toggleRecord(), { icon: "record", on: !!this.recorder }],
          ["The terminal below", below, { icon: "below", on: !!this.split }],
        ];
        if (several) items.push(["Which screen…", () => monitorMenu(more), { icon: "screens" }]);
        items.push(null, [this.paused ? "Go on" : "Pause the picture", pause, { icon: this.paused ? "play" : "pause", on: this.paused }]);
        if (this.paused) items.push(["Fetch one picture", () => this.send({ refresh: 1 }), { icon: "reload" }]);
        items.push(["View only", viewOnly, { icon: "eye", on: this.viewOnly }], ["Save a picture of the screen", shot, { icon: "camera" }]);
        menu(items, more);
      };
      bar.append(more);
      return;
    }
    const keys = button("keyboard", "Keys", { title: "Keys this browser keeps for itself" });
    keys.onclick = () => keysMenu(keys);
    const computer = button("lock", null, { title: "This computer: lock, restart, keep its keyboard out, hide its screen" });
    computer.onclick = () => computerMenu(computer);
    const clip = button("clipboard", "Clipboard", { title: "Copy and paste between this computer and the host" });
    clip.onclick = () => clipMenu(clip);
    // Its screen's own resolution is set from here too, when it can be.
    const quality = button("sliders", null, { title: this.modes?.list.length ? "Picture quality and the resolution of its screen" : "Picture quality" });
    quality.onclick = () => qualityMenu(quality);
    const chat = button("chat", null, { title: "Chat with the person at the host" });
    chat.classList.toggle("on", !$("chat").hidden);
    if (this.unread) chat.append(h("span", { class: "count", text: String(this.unread) }));
    chat.onclick = () => this.chatShow($("chat").hidden);
    const sound = () => {
      if (this.sound) { this.sound.close(); this.sound = null; this.send({ sound: false }); }
      else {
        this.sound = soundPlayer();
        if (!this.sound) { toast("This browser cannot play the host's sound (it needs WebCodecs: Chrome, Edge or Firefox).", { bad: true }); return; }
        this.send({ sound: true });
      }
      this.screenTools();
    };
    const below = () => { this.split = !this.split; store.set("split", this.split); this.splitView(); this.screenTools(); };
    const hear = button("sound", null, { title: this.sound ? "Stop the host's sound" : "Hear the host's sound" });
    hear.classList.toggle("on", !!this.sound);
    hear.onclick = sound;
    const term = button("below", null, { title: this.split ? "The screen alone" : "The terminal below the screen" });
    term.classList.toggle("on", !!this.split);
    term.onclick = below;
    const share = button("window", null, { title: "Share the whole screen, one window or one program" });
    share.classList.toggle("on", kind !== 0);
    share.onclick = () => shareMenu(share);
    const phone = button("phone", null, { title: this.call === 2 ? "Hang up" : this.call === 1 ? "Calling… (click to call off)" : "Call the person at that computer: talk through the microphones" });
    phone.classList.toggle("on", this.call === 1 || this.call === 2);
    phone.onclick = () => this.toggleCall();
    const rec = button("record", null, { title: this.recorder ? "Stop recording and save the video" : "Record the screen to a video file" });
    rec.classList.toggle("on", !!this.recorder);
    rec.onclick = () => this.toggleRecord();
    bar.append(keys, computer, clip, quality, share, chat, hear, phone, rec, term);
    if (several) {
      const mon = button("screens", null, { title: "Which of the host's screens" });
      mon.onclick = () => monitorMenu(mon);
      bar.append(mon);
    }
    const eye = button("eye", null, { title: "View only: watch without sending keys or clicks" });
    eye.classList.toggle("on", this.viewOnly);
    eye.onclick = viewOnly;
    const cam = button("camera", null, { title: "Save a lossless picture of the screen" });
    cam.onclick = shot;
    const hold = button(this.paused ? "play" : "pause", null, { title: this.paused ? "Go on with the picture" : "Pause the picture, to save traffic" });
    hold.classList.toggle("on", !!this.paused);
    hold.onclick = pause;
    bar.append(eye, hold);
    if (this.paused) {
      const one = button("reload", null, { title: "Fetch one picture" });
      one.onclick = () => this.send({ refresh: 1 });
      bar.append(one);
    }
    bar.append(cam);
  }
  async pasteHere() {
    // A picture copied here, when the browser lets the page read one.
    if (navigator.clipboard?.read) {
      try {
        for (const item of await navigator.clipboard.read()) {
          const type = item.types.find(t => t.startsWith("image/"));
          if (type) { this.sendImage(await item.getType(type), () => this.combo([K.ctrl, 0x76])); return; }
        }
      } catch { /* text, below */ }
    }
    if (!navigator.clipboard?.readText) { toast("This browser does not let the page read the clipboard; use “Type or paste text”.", { bad: true }); return; }
    try {
      const text = await navigator.clipboard.readText();
      if (!text) { toast("The clipboard has no text."); return; }
      this.sentClip = text;
      this.send({ clipboard: text });
      // Then Ctrl+V there, once the text is on its clipboard.
      setTimeout(() => this.combo([K.ctrl, 0x76]), 150);
    } catch { toast("The page was not allowed to read the clipboard.", { bad: true }); }
  }
  // What a paste in the screen brought: a picture onto the host's clipboard, files as files, or
  // text; then `paste` (Ctrl+V there).
  // `settle` lets held keys go when nothing is pasted by keys (files paste there by themselves).
  pasteData(dt, paste, settle = () => {}) {
    const files = [...(dt?.files || [])];
    // A screenshot comes as one unnamed picture ("image.png"); a picture file copied in a file
    // manager keeps its name and is pasted as the file it is.
    if (files.length === 1 && files[0].type.startsWith("image/") && /^(image|untitled)?(\.\w+)?$/i.test(files[0].name)) { this.sendImage(files[0], paste, settle); return; }
    if (files.length) { this.pasteFileList(files); settle(); return; }
    const text = dt?.getData("text/plain");
    // What was sent before is sent again only if this page was left since the host copied
    // something (it may have been copied anew there); the host's copy is newer otherwise.
    const sentStill = text === this.sentClip && !(leftAt > (this.hostAt || 0));
    if (text && !sentStill && text !== this.hostClip) {
      this.sentClip = text;
      this.hostTurn = false;
      this.send({ clipboard: text });
      setTimeout(paste, 150);
    } else paste();
  }
  // A picture onto the host's clipboard as PNG, in parts; larger than the host takes, it goes as
  // a file instead.
  async sendImage(blob, paste, settle = () => {}) {
    try {
      if (blob.type !== "image/png") {
        const bmp = await createImageBitmap(blob);
        const c = h("canvas", { width: bmp.width, height: bmp.height });
        c.getContext("2d").drawImage(bmp, 0, 0);
        blob = await new Promise(ok => c.toBlob(ok, "image/png"));
      }
      if (blob.size > 4 << 20) {
        this.pasteFileList([new File([blob], "picture-" + new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-") + ".png", { type: "image/png" })]);
        settle();
        return;
      }
      const bytes = new Uint8Array(await blob.arrayBuffer());
      if (this.ws?.readyState !== 1) { settle(); return; }
      // The picture sent last, while the host has copied something since: theirs is newer.
      let mark = 0x811c9dc5;
      for (let i = 0; i < bytes.length; i++) mark = Math.imul(mark ^ bytes[i], 16777619);
      mark = bytes.length + ":" + (mark >>> 0);
      if (this.hostTurn && mark === this.sentImage && !(leftAt > (this.hostAt || 0))) { paste(); return; }
      this.sentImage = mark;
      this.hostTurn = false;
      this.sentClip = null;
      for (let at = 0; at < bytes.length; at += 512 << 10) {
        const part = bytes.subarray(at, at + (512 << 10));
        const fr = new Uint8Array(part.length + 1);
        fr[0] = 8; fr.set(part, 1);
        this.ws.send(fr);
      }
      this.send({ clipImageEnd: true });
      setTimeout(paste, 250);
    } catch { settle(); toast("The picture could not be read.", { bad: true }); }
  }
  // A picture copied on the host: onto this computer's clipboard when the browser allows it, or
  // a button to do it (or to save it, where pictures cannot be copied).
  clipImageIn(blob) {
    this.hostImage = blob;
    const offer = () => toast("A picture was copied on the host.", window.ClipboardItem ? { action: ["Copy here", () => this.copyHostImage()] } : { action: ["Save", () => this.saveHostImage()] });
    if (!window.ClipboardItem || !navigator.clipboard?.write) { offer(); return; }
    navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]).then(() => {}, offer);
  }
  copyHostImage() {
    if (!this.hostImage) return;
    if (!window.ClipboardItem || !navigator.clipboard?.write) { this.saveHostImage(); return; }
    navigator.clipboard.write([new ClipboardItem({ "image/png": this.hostImage })]).then(() => toast("Copied here."), () => toast("This browser did not allow copying a picture.", { bad: true, action: ["Save", () => this.saveHostImage()] }));
  }
  saveHostImage() {
    if (this.hostImage) saveBlob(this.hostImage, "picture-" + new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-") + ".png");
  }
  clipboardIn(text) {
    this.hostTurn = true;
    this.hostAt = Date.now();
    this.hostClip = text;
    navigator.clipboard?.writeText(text).then(() => {}, () => toast("Copied on the host.", { action: ["Copy here", () => this.copyHostClip()] }));
  }
  copyHostClip() {
    if (this.hostClip === null) return;
    navigator.clipboard?.writeText(this.hostClip).then(() => toast("Copied here."), () => toast("This browser did not allow copying.", { bad: true }));
  }

  // ----- Files
  filesView() {
    setButton($("fup"), "up"); setButton($("fhome"), "home"); setButton($("freload"), "reload");
    setButton($("fnew"), "newfolder", "New folder"); setButton($("fsend"), "upload", "Upload");
    setButton($("fhidden"), "eye");
    const hiddenState = () => { const on = store.get("hidden", false); $("fhidden").classList.toggle("on", on); $("fhidden").title = on ? "Hide hidden files" : "Show hidden files (names starting with a dot)"; };
    hiddenState();
    $("fhidden").onclick = () => { store.set("hidden", !store.get("hidden", false)); hiddenState(); this.renderFiles(); };
    const f = this.files;
    $("fup").onclick = () => this.up();
    $("fhome").onclick = () => this.list("");
    $("freload").onclick = () => this.list(f.cwd ?? "");
    $("path").onkeydown = e => { if (e.key === "Enter") { this.list($("path").value.trim()); $("ftable").focus(); } if (e.key === "Escape") { $("path").value = f.cwd === "::" ? "" : f.cwd || ""; $("ftable").focus(); } };
    $("path").onfocus = () => $("path").select();
    $("fnew").onclick = () => this.newFolder();
    $("fsend").onclick = () => menu([["Files…", () => $("fpick").click(), { icon: "file" }], ["A folder…", () => $("fpickdir").click(), { icon: "folder" }]], $("fsend"));
    $("fpick").onchange = () => { this.queueUploads([...$("fpick").files].map(file => ({ file, name: file.name }))); $("fpick").value = ""; };
    $("fpickdir").onchange = () => { this.queueUploads([...$("fpickdir").files].map(file => ({ file, name: file.webkitRelativePath || file.name }))); $("fpickdir").value = ""; };
    const drop = $("vfiles");
    let depth = 0;
    drop.ondragenter = e => { e.preventDefault(); depth++; drop.classList.add("dragging"); };
    drop.ondragover = e => e.preventDefault();
    drop.ondragleave = () => { if (--depth <= 0) { depth = 0; drop.classList.remove("dragging"); } };
    drop.ondrop = async e => {
      e.preventDefault();
      depth = 0; drop.classList.remove("dragging");
      this.queueUploads(await dropped(e.dataTransfer));
    };
    $("ftable").onkeydown = e => this.fileKeys(e);
    $("ftable").textContent = "";
    $("fsel").textContent = "";
  }
  filesWait() {
    if (this.agent === "failed" && !this.files.entries.length) {
      const t = $("ftable");
      t.textContent = "";
      t.append(h("div", { class: "msg bad", text: "The files could not be reached: " + (this.agentWhy || "") }));
    }
  }
  sep(p) { return p.includes("\\") || /^[A-Za-z]:/.test(p) ? "\\" : "/"; }
  join(p, n) { return p === "::" ? n : p.endsWith(this.sep(p)) ? p + n : p + this.sep(p) + n; }
  list(p) {
    if (this.files.cwd === null && this.agent !== "up") {
      const t = $("ftable");
      t.textContent = "";
      t.append(h("div", { class: "msg" }, h("div", { class: "spin" }), "Connecting for the files…"));
    }
    this.send({ list: p });
  }
  up() {
    const c = this.files.cwd;
    // A root goes up to the drives (or stays at / elsewhere).
    if (!c || c === "/" || /^[A-Za-z]:\\?$/.test(c)) this.list("::"); else this.list(this.join(c, ".."));
  }
  dir(d) {
    const f = this.files;
    const same = f.cwd === d.path;
    f.cwd = d.path;
    f.error = d.error;
    f.entries = d.entries || [];
    $("path").value = d.path === "::" ? "" : d.path;
    // A reload keeps what was selected, where it still is.
    if (!same) { f.sel.clear(); f.anchor = null; }
    const names = new Set(f.entries.map(e => e.name));
    for (const n of [...f.sel]) if (!names.has(n)) f.sel.delete(n);
    this.renderFiles();
    if (this.view === "files" && document.activeElement !== $("path")) $("ftable").focus();
  }
  sorted() {
    const [key, dir] = this.files.sort;
    const hidden = store.get("hidden", false);
    return this.files.entries.filter(e => hidden || !e.name.startsWith(".")).sort((a, b) => (b.dir - a.dir) ||
      dir * (key === "size" ? a.size - b.size : key === "mtime" ? a.mtime - b.mtime : a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" })));
  }
  renderFiles() {
    const f = this.files, t = $("ftable");
    t.textContent = "";
    const [key, dir] = f.sort;
    const head = h("div", { class: "head" });
    for (const [k, label, cls] of [["name", "Name"], ["size", "Size", "r"], ["mtime", "Modified", "when"]]) {
      head.append(h("span", { class: cls, text: label + (key === k ? (dir > 0 ? " ▲" : " ▼") : ""), onclick: () => { f.sort = [k, key === k ? -dir : 1]; this.renderFiles(); } }));
    }
    t.append(head);
    if (f.error) { t.append(h("div", { class: "msg bad", text: f.error })); this.renderSel(); return; }
    const rows = this.sorted();
    if (!rows.length) t.append(h("div", { class: "msg", text: f.entries.length ? "Only hidden files here." : "This folder is empty." }));
    for (const e of rows) {
      const row = h("div", { class: "f" + (e.dir ? " dir" : "") + (f.sel.has(e.name) ? " sel" : ""), title: e.name },
        h("div", { class: "n" }, icon(e.dir ? "folder" : "file"), h("span", { text: e.name })),
        h("div", { class: "m r", text: e.dir ? "" : size(e.size) }),
        h("div", { class: "m when", text: when(e.mtime) }));
      row.onclick = ev => this.click(e, ev, rows);
      row.ondblclick = () => this.activate(e);
      row.oncontextmenu = ev => {
        ev.preventDefault();
        if (!f.sel.has(e.name)) { f.sel = new Set([e.name]); f.anchor = e.name; this.renderFiles(); }
        menu(this.actions(), [ev.clientX, ev.clientY]);
      };
      t.append(row);
    }
    this.renderSel();
  }
  click(e, ev, rows) {
    const f = this.files;
    if (ev.shiftKey && f.anchor) {
      const names = rows.map(r => r.name);
      const [a, b] = [names.indexOf(f.anchor), names.indexOf(e.name)].sort((x, y) => x - y);
      f.sel = new Set(names.slice(a, b + 1));
    } else if (ev.ctrlKey || ev.metaKey) {
      if (f.sel.has(e.name)) f.sel.delete(e.name); else f.sel.add(e.name);
      f.anchor = e.name;
    } else {
      f.sel = new Set([e.name]);
      f.anchor = e.name;
    }
    $("ftable").focus();
    this.renderFiles();
  }
  // Double-click or Enter: a folder opens, a file downloads.
  activate(e) {
    if (e.dir) this.list(this.join(this.files.cwd, e.name)); else this.download([e]);
  }
  selected() { return this.sorted().filter(e => this.files.sel.has(e.name)); }
  actions() {
    const s = this.selected(), one = s.length === 1 ? s[0] : null;
    const items = [];
    if (one?.dir) items.push(["Open", () => this.activate(one), { icon: "open", kbd: "Enter" }]);
    items.push(
      ["Download" + (s.length > 1 || one?.dir ? " as ZIP" : ""), () => this.download(s), { icon: "download", disabled: !s.length }],
      ["Rename…", () => this.rename(one), { icon: "rename", kbd: "F2", disabled: !one }],
      null,
      ["Delete…", () => this.remove(s), { icon: "trash", kbd: "Del", disabled: !s.length }]);
    return items;
  }
  renderSel() {
    const bar = $("fsel"), s = this.selected();
    bar.textContent = "";
    const what = h("div", { class: "what" });
    if (!s.length) {
      const n = this.sorted().length, hid = this.files.entries.length - n;
      what.textContent = this.files.cwd === null ? "" : n + (n === 1 ? " item" : " items") + (hid ? " (" + hid + " hidden)" : "") + " · click to select, double-click to open";
      bar.append(what);
      return;
    }
    if (s.length === 1) {
      const e = s[0];
      what.append(h("b", { text: e.name }), " · " + (e.dir ? "folder" : size(e.size)) + (e.mtime ? " · modified " + when(e.mtime) : ""));
    } else {
      const files = s.filter(e => !e.dir), dirs = s.length - files.length;
      what.append(h("b", { text: s.length + " selected" }),
        " · " + [files.length ? files.length + (files.length === 1 ? " file, " : " files, ") + size(files.reduce((n, e) => n + e.size, 0)) : null,
          dirs ? dirs + (dirs === 1 ? " folder" : " folders") : null].filter(Boolean).join(" and "));
    }
    const one = s.length === 1 ? s[0] : null;
    bar.append(what);
    if (one?.dir) bar.append(button("open", "Open", { class: "btn solid", onclick: () => this.activate(one) }));
    bar.append(
      button("download", s.length > 1 || one?.dir ? "Download ZIP" : "Download", { class: "btn primary", onclick: () => this.download(s) }),
      button("rename", "Rename", { class: "btn solid", disabled: !one, onclick: () => this.rename(one) }),
      button("trash", "Delete", { class: "btn solid danger", onclick: () => this.remove(s) }));
  }
  fileKeys(e) {
    const f = this.files, rows = this.sorted();
    if (e.target !== $("ftable")) return;
    const names = rows.map(r => r.name);
    const at = f.anchor ? names.indexOf(f.anchor) : -1;
    const move = i => { if (!rows.length) return; i = Math.max(0, Math.min(rows.length - 1, i)); f.sel = new Set([names[i]]); f.anchor = names[i]; this.renderFiles(); $("ftable").children[i + 1]?.scrollIntoView({ block: "nearest" }); };
    if (e.key === "ArrowDown") { e.preventDefault(); move(at + 1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); move(at < 0 ? 0 : at - 1); }
    else if (e.key === "Home") { e.preventDefault(); move(0); }
    else if (e.key === "End") { e.preventDefault(); move(rows.length - 1); }
    else if (e.key === "Enter") { const s = this.selected(); if (s.length === 1) this.activate(s[0]); else if (s.length) this.download(s); }
    else if (e.key === "Backspace" || (e.altKey && e.key === "ArrowUp")) { e.preventDefault(); this.up(); }
    else if (e.key === "Delete") { const s = this.selected(); if (s.length) this.remove(s); }
    else if (e.key === "F2") { e.preventDefault(); const s = this.selected(); if (s.length === 1) this.rename(s[0]); }
    else if (e.key === "F5") { e.preventDefault(); this.list(f.cwd ?? ""); }
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") { e.preventDefault(); f.sel = new Set(names); this.renderFiles(); }
    else if (e.key === "Escape") { f.sel.clear(); this.renderFiles(); }
  }
  async newFolder() {
    if (this.files.cwd === null || this.files.cwd === "::") { toast("Open a folder first."); return; }
    const name = await ask({ title: "New folder", field: "Name", value: "New folder", select: v => [0, v.length], ok: "Create" });
    if (!name?.trim()) return;
    this.ops = 1;
    this.send({ op: ["mkdir", this.join(this.files.cwd, name.trim()), ""] });
  }
  async rename(e) {
    if (!e) return;
    const stem = e.dir ? e.name.length : (e.name.lastIndexOf(".") > 0 ? e.name.lastIndexOf(".") : e.name.length);
    const name = await ask({ title: "Rename", field: "New name", value: e.name, select: () => [0, stem], ok: "Rename" });
    if (!name?.trim() || name.trim() === e.name) return;
    this.ops = 1;
    this.files.sel = new Set([name.trim()]);
    this.send({ op: ["rename", this.join(this.files.cwd, e.name), name.trim()] });
  }
  async remove(s) {
    if (!s.length) return;
    const what = s.length === 1 ? "“" + s[0].name + "”" : s.length + " items";
    const ok = await ask({ title: "Delete " + what + "?", text: "Folders go with everything in them. This cannot be undone.", ok: "Delete", danger: true });
    if (!ok) return;
    this.ops = s.length;
    this.opErrors = [];
    for (const e of s) this.send({ op: ["delete", this.join(this.files.cwd, e.name), ""] });
  }
  opDone(d) {
    if (!d.ok) (this.opErrors ||= []).push(d.text);
    if (--this.ops > 0) return;
    if (this.opErrors?.length) toast(this.opErrors.length === 1 ? this.opErrors[0] : this.opErrors.length + " could not be done: " + this.opErrors[0], { bad: true });
    this.opErrors = [];
    this.list(this.files.cwd);
  }

  // ----- Downloads: one file as itself, a folder or several items as one ZIP
  download(items) {
    if (!items.length) return;
    const single = items.length === 1 && !items[0].dir ? items[0] : null;
    if (single && single.size > MAX_TRANSFER) { toast("Larger than 2 GB: too large for a browser download.", { bad: true }); return; }
    const cwdName = (this.files.cwd || "").split(/[\\/]/).filter(Boolean).pop() || "files";
    const name = single ? single.name : (items.length === 1 ? items[0].name : cwdName) + ".zip";
    const job = { name, zip: !single, tags: new Set(), done: new Set(), entries: [], open: 0, failed: [], bytes: 0, last: Date.now(), cancelled: false };
    job.ui = this.xferRow("download", name, single ? single.size : 0, () => this.cancelJob(job));
    for (const e of items) {
      const tag = (DONE_TAG | this.nextTag++) >>> 0;
      job.tags.add(tag);
      this.jobs.set(tag, job);
      this.send({ download: this.join(this.files.cwd, e.name), tag });
    }
    // An older agent never says a folder is complete: quiet for a while after its last file is.
    job.timer = setInterval(() => { if (job.open === 0 && Date.now() - job.last > 6000) this.finishJob(job); }, 1000);
  }
  fetchStart(f) {
    const job = this.jobs.get(f.tag >>> 0);
    if (!job || job.cancelled) { this.send({ cancel: f.id }); return; }
    const entry = { name: f.name, size: f.size, parts: [], got: 0, crc: 0 };
    this.byId.set(f.id, { job, entry });
    job.open++;
    job.last = Date.now();
    if (job.bytes + f.size > MAX_TRANSFER) { this.cancelJob(job, "More than 2 GB in all: too large for a browser download."); return; }
    job.ui.label(job.zip ? job.name + " · " + (job.entries.length + 1) + " files" : job.name);
  }
  chunk(id, bytes) {
    const d = this.byId.get(id);
    if (!d || d.job.cancelled) return;
    d.entry.parts.push(bytes);
    d.entry.got += bytes.length;
    if (d.job.zip) d.entry.crc = crc32(d.entry.crc, bytes);
    d.job.bytes += bytes.length;
    d.job.last = Date.now();
    d.job.ui.set(d.job.zip ? d.job.bytes : d.entry.got, d.job.zip ? 0 : d.entry.size);
  }
  fetchEnd(f) {
    const d = this.byId.get(f.id);
    if (!d) {
      // Could not even be opened there.
      const job = this.jobs.get((f.tag ?? 0) >>> 0);
      if (job) { job.failed.push(f.text || f.name); job.last = Date.now(); }
      else if (!f.ok && f.text && f.text !== "cancelled") toast(f.text, { bad: true });
      return;
    }
    this.byId.delete(f.id);
    const { job, entry } = d;
    job.open--;
    job.last = Date.now();
    if (job.cancelled) return;
    if (f.ok) job.entries.push(entry); else job.failed.push(entry.name + ": " + (f.text || "failed"));
    if (!job.zip) this.finishJob(job);
  }
  fetchDone(tag) {
    const job = this.jobs.get(tag >>> 0);
    if (!job) return;
    job.done.add(tag >>> 0);
    if (job.done.size === job.tags.size && job.open === 0) this.finishJob(job);
  }
  finishJob(job) {
    if (job.finished) return;
    job.finished = true;
    clearInterval(job.timer);
    for (const t of job.tags) this.jobs.delete(t);
    if (job.cancelled) return;
    if (!job.entries.length) { job.ui.done(false, job.failed[0] || "Nothing to download"); return; }
    const blob = job.zip ? zipBlob(job.entries) : new Blob(job.entries[0].parts);
    saveBlob(blob, job.zip ? job.name : job.entries[0].name.split(/[\\/]/).pop());
    const failed = job.failed.length ? " · " + job.failed.length + " could not be read" : "";
    job.ui.done(true, (job.zip ? job.entries.length + " files, " : "") + size(blob.size) + failed);
    if (job.failed.length) toast(job.failed.length + " could not be downloaded: " + job.failed[0], { bad: true });
  }
  cancelJob(job, why) {
    if (job.finished) return;
    job.cancelled = true;
    for (const tag of job.tags) this.send({ cancel: 0, tag });
    for (const [id, d] of this.byId) if (d.job === job) { this.send({ cancel: id }); this.byId.delete(id); }
    job.ui.done(false, why || "Cancelled");
    this.finishJob(job);
  }

  // ----- Uploads, one file at a time
  // `dir` "" sends to the host's Downloads folder.
  // Files chosen here, put on the host's clipboard as files and pasted where its keyboard is.
  pasteFiles() {
    const pick = h("input", { type: "file", multiple: true, hidden: true });
    pick.onchange = () => {
      const files = [...pick.files];
      pick.remove();
      this.pasteFileList(files);
    };
    document.body.append(pick);
    pick.click();
  }
  pasteFileList(files) {
    if (!files.length || this.viewOnly) return;
    const batch = "web-" + Date.now();
    this.queueUploads(files.map(file => ({ file, name: file.name })), "\u0000clipboard/" + batch, () => this.send({ pasteFiles: { batch, names: files.map(f => f.name) } }));
    toast("Pasting " + files.length + (files.length === 1 ? " file" : " files") + " there…");
  }
  queueUploads(list, dir = this.files.cwd, done = null) {
    if (!list.length) return;
    if (dir === null || dir === "::") { toast("Open a folder first."); return; }
    const big = list.filter(u => u.file.size > MAX_TRANSFER);
    if (big.length) toast(big.length + " larger than 2 GB left out: too large to send from a browser.", { bad: true });
    const items = list.filter(u => u.file.size <= MAX_TRANSFER);
    if (!items.length) return;
    const total = items.reduce((n, u) => n + u.file.size, 0);
    const batch = { dir, count: items.length, sent: 0, bytes: 0, total, failed: [], cancelled: false, done };
    batch.ui = this.xferRow("upload", items.length === 1 ? items[0].name : items.length + " files", total, () => { batch.cancelled = true; });
    for (const u of items) this.uploads.push({ ...u, batch });
    this.pump();
  }
  async pump() {
    if (this.sending || !this.uploads.length) return;
    const u = this.uploads.shift();
    const b = u.batch;
    if (b.cancelled) { this.batchStep(b); this.pump(); return; }
    const id = this.nextUpload++;
    const s = this.sending = { id, u, acked: 0, wake: null };
    this.send({ upload: { id, size: u.file.size, dir: b.dir, name: u.name } });
    let off = 0;
    while (off < u.file.size && this.sending === s && !b.cancelled) {
      // A bounded amount unacknowledged, so neither the gateway nor the socket piles it up.
      if (off - s.acked >= WINDOW || this.ws.bufferedAmount > 2 * WINDOW) {
        await new Promise(r => { s.wake = r; setTimeout(r, 200); });
        continue;
      }
      const bytes = new Uint8Array(await u.file.slice(off, off + CHUNK).arrayBuffer());
      if (this.ws.readyState !== 1) return;
      const fr = new Uint8Array(bytes.length + 5);
      fr[0] = 4; new DataView(fr.buffer).setUint32(1, id, true); fr.set(bytes, 5);
      this.ws.send(fr);
      off += bytes.length;
      b.ui.set(b.bytes + off, b.total);
    }
    if (this.sending === s) this.send({ uploadEnd: id });
  }
  progress(id, received) { const s = this.sending; if (s?.id === id) { s.acked = received; s.wake?.(); } }
  uploaded(d) {
    const s = this.sending;
    if (!s || s.id !== d.id) return;
    this.sending = null;
    s.wake?.();
    const b = s.u.batch;
    if (!d.ok && !b.cancelled) b.failed.push(s.u.name + ": " + d.text);
    b.bytes += s.u.file.size;
    this.batchStep(b);
    this.pump();
  }
  batchStep(b) {
    b.sent++;
    if (b.sent < b.count) { b.ui.label(b.count + " files · " + b.sent + " of " + b.count); return; }
    if (b.cancelled) b.ui.done(false, "Cancelled");
    else if (b.failed.length) { b.ui.done(false, b.failed.length + " failed: " + b.failed[0]); }
    else { b.ui.done(true, size(b.total) + " sent"); b.done?.(); }
    if (this.files.cwd === b.dir) this.list(b.dir);
  }

  // A row in the transfers list.
  xferRow(kind, label, total, cancel) {
    const name = h("span", { class: "name", text: label });
    const state = h("span", { class: "state" });
    const bar = h("progress", { max: Math.max(total, 1), value: 0 });
    const x = button("x", null, { title: "Cancel", class: "btn icon" });
    const row = h("div", { class: "xfer" }, icon(kind), name, state, x, bar);
    x.onclick = () => cancel();
    $("xfers").prepend(row);
    const ui = {
      label(t) { name.textContent = t; },
      set(n, of) { if (of) { bar.max = of; bar.value = n; state.textContent = size(n) + " of " + size(of); } else { bar.removeAttribute("value"); state.textContent = size(n); } },
      done(ok, text) {
        bar.max = 1; bar.value = 1; row.classList.add(ok ? "ok" : "bad"); state.textContent = text;
        x.onclick = () => row.remove(); x.title = "Dismiss";
        setTimeout(() => row.remove(), ok ? 6000 : 20000);
      },
    };
    ui.set(0, total);
    return ui;
  }
}

// Files and folders dropped on the page, with names relative to what was dropped.
async function dropped(dt) {
  const out = [];
  const entries = [...dt.items].map(i => i.webkitGetAsEntry?.()).filter(Boolean);
  if (!entries.length) return [...dt.files].map(file => ({ file, name: file.name }));
  const walk = async (entry, prefix) => {
    if (entry.isFile) {
      const file = await new Promise((ok, no) => entry.file(ok, no)).catch(() => null);
      if (file) out.push({ file, name: prefix + entry.name });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      for (;;) {
        const batch = await new Promise((ok, no) => reader.readEntries(ok, no)).catch(() => []);
        if (!batch.length) break;
        for (const e of batch) await walk(e, prefix + entry.name + "/");
      }
    }
  };
  for (const e of entries) await walk(e, "");
  // Entries the browser cannot read (WebKit's, for files it was handed by a page): the files.
  if (!out.length && dt.files.length) return [...dt.files].map(file => ({ file, name: file.name }));
  return out;
}

// ----- Start ------------------------------------------------------------------------------------------

// Windows opened for the host's other screens, and what each is to reach; a window asks for its
// part once it is ready to hear it.
const spreads = new Map();
addEventListener("message", e => {
  if (e.origin !== location.origin) return;
  if (e.data === "spread-ready" && spreads.has(e.source)) {
    e.source.postMessage({ spread: spreads.get(e.source) }, location.origin);
    spreads.delete(e.source);
  }
  // In the opened window: the host to reach, as the window that opened it reaches it.
  if (e.data?.spread && e.source === window.opener) {
    const { t, gateway: g } = e.data.spread;
    if (g?.token) gateway.token ||= g.token;
    if (g?.password) gateway.password ||= g.password;
    connect(t);
  }
});
if ((/^#screen=\d+$/.test(location.hash) || location.hash === "#program") && window.opener) window.opener.postMessage("spread-ready", location.origin);

// The page moved here from another address (sshremote.dev/web to web.sshremote.dev): the
// computers that browser kept there come along once the person says so; their passwords stay
// behind, sealed to that address. The link names what came: #import=BASE64URL(JSON).
let linked = location.hash;
{
  const m = /^#import=([A-Za-z0-9_-]{1,20000})$/.exec(location.hash);
  if (m) {
    history.replaceState(null, "", location.pathname + location.search);
    let got = null;
    try { got = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(m[1].replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0)))); } catch { got = null; }
    linked = typeof got?.then === "string" ? got.then : "";
    const text = (v, n) => (typeof v === "string" ? v : "").slice(0, n);
    const list = (Array.isArray(got?.saved) ? got.saved : [])
      .filter(e => e && ["id", "ssh", "gw"].includes(e.kind) && allowed(e) && typeof e.address === "string" && e.address && e.address.length < 300)
      .slice(0, 40)
      .map(e => {
        const t = { kind: e.kind, address: e.address, port: /^\d{1,5}$/.test(String(e.port ?? "")) ? String(e.port) : "", name: text(e.name, 80), last: Number(e.last) || 0 };
        return { ...t, key: keyOf(t) };
      });
    const fresh = list.filter(e => !saved().some(x => x.key === e.key));
    if (fresh.length) {
      const from = text(got.from, 80) || "its old address";
      ask({ title: "Bring your computers along?", text: `This page moved here from ${from}. This browser kept ${fresh.length === 1 ? "one computer" : fresh.length + " computers"} there: ${fresh.map(title).join(", ")}. Their passwords stay behind: type each once more.`, ok: "Add them" })
        .then(yes => { if (yes) { store.set("saved", [...saved(), ...fresh].slice(0, 40)); renderSaved(); } });
    }
    if ((got?.theme === "light" || got?.theme === "dark") && !store.get("theme", "")) {
      store.set("theme", got.theme);
      document.documentElement.dataset.theme = got.theme;
      themeButton();
    }
  }
}
// A link can name what to open: #id=123456789 or #ssh=user@host.
{
  const m = /^#(id|ssh)=(.+)$/.exec(linked);
  if (m) $("addr").value = m[1] === "id" ? spaced(decodeURIComponent(m[2]).replace(/\D/g, "").slice(0, 9)) : decodeURIComponent(m[2]);
}
renderSaved();
refreshForm();
renderGateway();
// A remembered sign-in: checked, and the gateway's hosts listed.
if (gateway.token && !idOnly) signIn(null, false, gateway.token).catch(e => { if (e.signin) signOut(); });
