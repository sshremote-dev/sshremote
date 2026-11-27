"use strict";
// The team server's console (docs/team.md): signing in, for the console or for the app that opened
// the page, and for administrators the team's devices, users, groups, device groups and their
// policies, access rules, enrollment tokens, audit log and settings.

const $ = id => document.getElementById(id);
const base = location.pathname.replace(/[^/]*$/, "");

// ----- Small helpers -------------------------------------------------------------------------------

const ICONS = {
  sun: "M12 8a4 4 0 1 1 0 8 4 4 0 0 1 0-8z|M12 2v2|M12 20v2|M4.9 4.9l1.4 1.4|M17.7 17.7l1.4 1.4|M2 12h2|M20 12h2|M4.9 19.1l1.4-1.4|M17.7 6.3l1.4-1.4",
  moon: "M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z",
  auto: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z|M12 3v18|M12 7.5h7|M12 12h9|M12 16.5h7",
  globe: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z|M3 12h18|M12 3c2.4 2.5 3.6 5.5 3.6 9s-1.2 6.5-3.6 9|M12 3c-2.4 2.5-3.6 5.5-3.6 9s1.2 6.5 3.6 9",
  monitor: "M3 4h18v12H3z|M8 20h8|M12 16v4",
  user: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8z|M4.5 20c1.4-3.4 4.2-5 7.5-5s6.1 1.6 7.5 5",
  users: "M9 11a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z|M2.5 19.5c1-3 3.5-4.5 6.5-4.5s5.5 1.5 6.5 4.5|M16 4.6a3.5 3.5 0 0 1 0 6.3|M18 15.2c1.8.6 2.9 2 3.5 4.3",
  layers: "M12 3 3 8l9 5 9-5z|M3 12.5l9 5 9-5|M3 17l9 5 9-5",
  shield: "M12 3l7.5 3v5.5c0 4.6-3.2 8-7.5 9.5-4.3-1.5-7.5-4.9-7.5-9.5V6z|M9 12l2 2 4-4",
  key: "M15.5 3.5a5 5 0 1 1 0 10 5 5 0 0 1 0-10z|M12 12 3.5 20.5|M6.5 17.5l2.5 2.5|M9 15l2 2",
  list: "M9 6h11|M9 12h11|M9 18h11|M4.5 6h.01|M4.5 12h.01|M4.5 18h.01",
  sliders: "M4 7h9|M17 7h3|M15 5v4|M4 12h3|M11 12h9|M9 10v4|M4 17h11|M19 17h1|M17 15v4",
  logout: "M10 4H5v16h5|M15 8l4 4-4 4|M19 12H9",
  menu: "M4 6h16|M4 12h16|M4 18h16",
  x: "M6 6l12 12|M18 6 6 18",
  search: "M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13z|M20 20l-4.8-4.8",
  plus: "M12 5v14|M5 12h14",
  rename: "M4 20h4L19 9l-4-4L4 16z|M13.5 6.5l4 4",
  trash: "M4 7h16|M10 11v6|M14 11v6|M6 7l1 13h10l1-13|M9 7V4h6v3",
  power: "M12 3v8|M6.3 6.8a7.5 7.5 0 1 0 11.4 0",
  up: "M12 19V5|M6 11l6-6 6 6",
  down: "M12 5v14|M6 13l6 6 6-6",
  copy: "M9 9h11v11H9z|M5 15H4V4h11v1",
  check: "M5 12.5l4.5 4.5L19 7.5",
  download: "M12 4v11|M7 10l5 5 5-5|M4 15v5h16v-5",
  image: "M4 5h16v14H4z|M4 16l5-5 4 4 3-3 4 4|M15.5 8.5h.01",
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
// The program's own mark, drawn where a team has no logo.
function mark() {
  const s = document.createElementNS(SVG, "svg");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("aria-hidden", "true");
  for (const d of ["M5 8.5 8.5 12 5 15.5", "M12 5h4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-4"]) {
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
  for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false && k !== "") e.append(k);
  return e;
}
// A button with an icon only: its words are its name and tooltip.
function iconButton(ic, label, props = {}) {
  const { class: cls, ...rest } = props;
  return h("button", { type: "button", ...rest, class: "btn icon" + (cls ? " " + cls : ""), title: label, "aria-label": label }, icon(ic));
}
function textButton(ic, label, props = {}) {
  return h("button", { type: "button", ...props, class: "btn" + (props.class ? " " + props.class : "") }, ic ? icon(ic) : null, h("span", { text: label }));
}

const spaced = id => String(id || "").replace(/(\d{3})(?=\d)/g, "$1 ");
const norm = s => String(s ?? "").toLowerCase().replace(/\s+/g, "");
const enc = encodeURIComponent;
const dash = () => h("span", { class: "faint", text: "\u2014" });
const nowSecs = () => Math.floor(Date.now() / 1000);
function ago(t) {
  if (!t) return T("Never");
  const s = Date.now() / 1000 - t;
  if (s < 60) return T("just now");
  if (s < 3600) return T("{} min ago", Math.floor(s / 60));
  if (s < 86400) return T("{} h ago", Math.floor(s / 3600));
  if (s < 172800) return T("yesterday");
  return T("{} days ago", Math.floor(s / 86400));
}
const when = t => t ? new Date(t * 1000).toLocaleString(dateLang, { dateStyle: "medium", timeStyle: "short" }) : "";
function duration(secs) {
  const s = Math.round(+secs || 0);
  if (s < 60) return T("{} s", s);
  if (s < 3600) return T("{} min", Math.round(s / 60));
  return T("{} h {} min", Math.floor(s / 3600), Math.round((s % 3600) / 60));
}
function cmp(a, b) {
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a ?? "").localeCompare(String(b ?? ""), dateLang, { numeric: true, sensitivity: "base" });
}

function toast(text, bad) {
  const t = h("div", { class: "toast" + (bad ? " bad" : ""), role: bad ? "alert" : "status" }, h("span", { text, dir: "auto" }));
  t.append(iconButton("x", T("Dismiss"), { onclick: () => t.remove() }));
  $("toasts").append(t);
  // The newest three are enough.
  while ($("toasts").children.length > 3) $("toasts").firstElementChild.remove();
  setTimeout(() => t.remove(), bad ? 10000 : 5000);
}
const oops = e => { if (e?.status !== 401) toast(e?.message || String(e), true); };

// Puts `text` on the clipboard; the button says so for a moment.
async function copy(text, btn) {
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; } catch {
    // Not a secure page: the old way, from a field inside the open dialog (the rest is inert then).
    const t = h("textarea", { class: "sr-only", readonly: true });
    t.value = text;
    (document.querySelector("dialog[open]") || document.body).append(t);
    t.select();
    try { ok = document.execCommand("copy"); } catch { ok = false; }
    t.remove();
  }
  if (!btn) { toast(ok ? T("Copied") : T("It could not be copied: select it and copy it yourself."), !ok); return; }
  const label = btn.querySelector("span");
  if (!label) return;
  const was = label.textContent;
  label.textContent = ok ? T("Copied") : T("Select it and copy it");
  setTimeout(() => { label.textContent = was; }, 1600);
}
function copyLine(text, label) {
  const btn = textButton("copy", T("Copy"));
  btn.onclick = () => copy(text, btn);
  const code = h("code", { text });
  if (label) code.setAttribute("aria-label", label);
  return h("div", { class: "cmd" }, code, btn);
}

// A menu of [label, action, {on, lang}] items (null for a rule) under `anchor`; arrows move in it.
let openMenu = null;
function menu(items, anchor) {
  closeMenu();
  const m = h("div", { class: "menu", role: "menu" });
  for (const it of items) {
    if (it === null) { m.append(h("hr")); continue; }
    const [label, act, o = {}] = it;
    const b = h("button", { type: "button", role: "menuitemradio", "aria-checked": o.on ? "true" : "false", class: o.on ? "on" : "", lang: o.lang }, h("span", { text: label }));
    if (!("on" in o)) b.setAttribute("role", "menuitem");
    b.onclick = () => { closeMenu(); anchor.focus(); act(); };
    m.append(b);
  }
  document.body.append(m);
  const r = anchor.getBoundingClientRect(), w = m.offsetWidth, hh = m.offsetHeight;
  const rtl = document.documentElement.dir === "rtl";
  m.style.left = Math.max(4, Math.min(rtl ? r.right - w : r.left, innerWidth - w - 4)) + "px";
  m.style.top = (r.bottom + hh + 4 > innerHeight ? Math.max(4, r.top - hh - 4) : r.bottom + 4) + "px";
  openMenu = { m, anchor };
  (m.querySelector("button.on") || m.querySelector("button"))?.focus();
  m.addEventListener("keydown", e => {
    const all = [...m.querySelectorAll("button")], i = all.indexOf(document.activeElement);
    const go = j => { e.preventDefault(); all[(j + all.length) % all.length].focus(); };
    if (e.key === "ArrowDown") go(i + 1);
    else if (e.key === "ArrowUp") go(i - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(all.length - 1);
    else if (e.key === "Escape") { e.preventDefault(); closeMenu(); anchor.focus(); }
    else if (e.key === "Tab") closeMenu();
  });
  setTimeout(() => addEventListener("pointerdown", outside, true), 0);
}
function outside(e) { if (openMenu && !openMenu.m.contains(e.target)) closeMenu(); }
function closeMenu() { if (openMenu) { openMenu.m.remove(); openMenu = null; removeEventListener("pointerdown", outside, true); } }

// A dialog with its fields; `submit` runs on its main button and keeps it open with its error
// shown when it throws. Resolves with what `submit` gave (true without it), or null when cancelled.
let dialogs = 0;
function dialog({ title, text, fields = [], ok = T("Save"), danger = false, wide = false, submit, cancel = T("Cancel") }) {
  return new Promise(done => {
    const back = document.activeElement, backKey = back?.dataset?.k;
    const id = "dlg" + ++dialogs;
    const err = h("div", { class: "error", role: "alert" });
    const yes = h("button", { type: "submit", class: "btn " + (danger ? "bad" : "primary"), text: ok });
    const no = cancel ? h("button", { type: "button", class: "btn solid", text: cancel }) : null;
    const form = h("form", { "aria-labelledby": id }, h("h2", { id, text: title }), text ? h("p", { class: "sub", text }) : null, ...fields, err, h("div", { class: "acts" }, no, yes));
    const d = h("dialog", { class: "dlg" + (wide ? " wide" : ""), "aria-labelledby": id }, form);
    let result = null;
    const close = v => { result = v; d.close(); };
    d.addEventListener("close", () => {
      d.remove();
      // Back to where the keyboard was, or to the same button drawn again since; a moment later,
      // as WebKit puts the focus back itself after this.
      const restore = () => {
        const to = back?.isConnected ? back : backKey && document.querySelector('[data-k="' + CSS.escape(backKey) + '"]');
        if (to && !to.disabled && !document.querySelector("dialog[open]")) to.focus();
      };
      restore();
      setTimeout(() => { if (document.activeElement === document.body) restore(); }, 0);
      done(result);
    });
    if (no) no.onclick = () => close(null);
    // WebKit lets a text field keep Escape from the dialog.
    d.addEventListener("keydown", e => { if (e.key === "Escape" && !e.defaultPrevented) { e.preventDefault(); close(null); } });
    form.onsubmit = async e => {
      e.preventDefault();
      err.textContent = "";
      if (!submit) { close(true); return; }
      yes.disabled = true;
      try { const v = await submit(); close(v === undefined ? true : v); } catch (x) { err.textContent = x?.message || String(x); yes.disabled = false; }
    };
    document.body.append(d);
    d.showModal();
    (form.querySelector("input:not([type=hidden]):not([disabled]), select, textarea") || yes).focus();
  });
}
const closeDialogs = () => { for (const d of document.querySelectorAll("dialog[open]")) d.close(); };

// A field: its label over it, its hint under it.
let uid = 0;
function field(label, input, hint, cls) {
  if (!input.id) input.id = "f" + ++uid;
  const hn = hint ? h("p", { class: "hint", id: input.id + "-h", text: hint }) : null;
  if (hn) input.setAttribute("aria-describedby", hn.id);
  return h("div", { class: "fld" + (cls ? " " + cls : "") }, h("label", { for: input.id, text: label }), input, hn);
}
function checkbox(label, checked, hint, props = {}) {
  const input = h("input", { type: "checkbox", ...props });
  input.checked = !!checked;
  const node = h("label", { class: "check" }, input, h("span", null, label, hint ? h("span", { class: "hint", text: hint }) : null));
  return { node, input };
}
function select(options, value, props = {}) {
  const s = h("select", props);
  for (const o of options) {
    if (o.group) s.append(h("optgroup", { label: o.group }, o.items.map(i => h("option", { value: i[0], text: i[1] }))));
    else s.append(h("option", { value: o[0], text: o[1] }));
  }
  s.value = value;
  return s;
}

// ----- What this browser keeps ----------------------------------------------------------------------

const store = {
  get(k, d) { try { const v = localStorage.getItem("sshremote." + k); return v ? JSON.parse(v) : d; } catch { return d; } },
  set(k, v) { try { if (v === null) localStorage.removeItem("sshremote." + k); else localStorage.setItem("sshremote." + k, JSON.stringify(v)); } catch { /* private mode */ } },
};
// The theme chosen here (light or dark) wins over the system's, from the first paint on.
{ const t = store.get("theme", ""); if (t === "light" || t === "dark") document.documentElement.dataset.theme = t; }

// The session, for this tab only.
let token = "";
try { token = sessionStorage.getItem("sshremote.team.session") || ""; } catch { token = ""; }
function setToken(t) {
  token = t || "";
  try { if (token) sessionStorage.setItem("sshremote.team.session", token); else sessionStorage.removeItem("sshremote.team.session"); } catch { /* kept in memory only */ }
}

// ----- The visitor's language ------------------------------------------------------------------------

// The languages the page speaks, each by its own name, as src/lang/mod.rs lists them (the names
// written as escapes, so this file stays ASCII).
const LANGS = [
  ["en", "English"], ["ar", "\u0627\u0644\u0639\u0631\u0628\u064a\u0629"], ["az", "Az\u0259rbaycan dili"],
  ["be", "\u0411\u0435\u043b\u0430\u0440\u0443\u0441\u043a\u0430\u044f"],
  ["bg", "\u0411\u044a\u043b\u0433\u0430\u0440\u0441\u043a\u0438"], ["ca", "Catal\u00e0"],
  ["cs", "\u010ce\u0161tina"], ["da", "Dansk"], ["de", "Deutsch"],
  ["el", "\u0395\u03bb\u03bb\u03b7\u03bd\u03b9\u03ba\u03ac"], ["eo", "Esperanto"], ["es", "Espa\u00f1ol"],
  ["et", "Eesti"], ["eu", "Euskara"], ["fa", "\u0641\u0627\u0631\u0633\u06cc"], ["fi", "Suomi"],
  ["fr", "Fran\u00e7ais"], ["gl", "Galego"], ["gu", "\u0a97\u0ac1\u0a9c\u0ab0\u0abe\u0aa4\u0ac0"],
  ["he", "\u05e2\u05d1\u05e8\u05d9\u05ea"], ["hi", "\u0939\u093f\u0928\u094d\u0926\u0940"], ["hr", "Hrvatski"],
  ["hu", "Magyar"], ["id", "Bahasa Indonesia"], ["it", "Italiano"], ["ja", "\u65e5\u672c\u8a9e"],
  ["ka", "\u10e5\u10d0\u10e0\u10d7\u10e3\u10da\u10d8"],
  ["kk", "\u049a\u0430\u0437\u0430\u049b \u0442\u0456\u043b\u0456"], ["ko", "\ud55c\uad6d\uc5b4"],
  ["lt", "Lietuvi\u0173"], ["lv", "Latvie\u0161u"], ["ml", "\u0d2e\u0d32\u0d2f\u0d3e\u0d33\u0d02"],
  ["nb", "Norsk bokm\u00e5l"], ["nl", "Nederlands"], ["pl", "Polski"], ["pt-BR", "Portugu\u00eas (Brasil)"],
  ["pt-PT", "Portugu\u00eas (Portugal)"], ["ro", "Rom\u00e2n\u0103"],
  ["ru", "\u0420\u0443\u0441\u0441\u043a\u0438\u0439"], ["sc", "Sardu"], ["sk", "Sloven\u010dina"],
  ["sl", "Sloven\u0161\u010dina"], ["sq", "Shqip"], ["sr", "Srpski"], ["sv", "Svenska"],
  ["ta", "\u0ba4\u0bae\u0bbf\u0bb4\u0bcd"], ["th", "\u0e44\u0e17\u0e22"], ["tr", "T\u00fcrk\u00e7e"],
  ["uk", "\u0423\u043a\u0440\u0430\u0457\u043d\u0441\u044c\u043a\u0430"], ["ur", "\u0627\u0631\u062f\u0648"],
  ["vi", "Ti\u1ebfng Vi\u1ec7t"], ["zh-CN", "\u7b80\u4f53\u4e2d\u6587"], ["zh-TW", "\u7e41\u9ad4\u4e2d\u6587"]
];
const RTL = ["ar", "fa", "he", "ur"];
// The language's words (English to its own, from lang/<code>.json); those with "{}" also match
// what the server says (`said`).
let words = new Map(), templates = [], langNow = "en", langAsked = 0;

// A template with its values: "{}" takes them in order, "{0}", "{1}" by place (a language that
// says them in another order), "{{" and "}}" are braces.
function fill(s, values) {
  let next = 0;
  return s.replace(/\{\{|\}\}|\{(\d*)\}/g, (m, at) => {
    if (m === "{{") return "{";
    if (m === "}}") return "}";
    const i = at === "" ? next++ : +at;
    return i < values.length ? String(values[i]) : m;
  });
}
// Words shown to the visitor, in the language now: T("Save"), T("Edit {}", name). Only a literal
// is collected for translation (scripts/lang.py), so a sentence is one, its values in slots.
function T(text, ...values) {
  const s = words.get(text) || text;
  return values.length ? fill(s, values) : s;
}
// What the server said (its errors are English), in the language now when its words are known.
function said(text) {
  if (!text || typeof text !== "string") return text;
  const w = words.get(text);
  if (w) return w;
  for (const [re, to] of templates) {
    const m = re.exec(text);
    if (m) return fill(to, m.slice(1));
  }
  return text;
}

// A tag such as "pt-BR", "zh-Hant-TW", "nn" or "ko_KR.UTF-8" as one of LANGS, or null; the same
// rules as find() in src/lang/mod.rs.
const ALIASES = new Map([["no", "nb"], ["nn", "nb"], ["iw", "he"], ["in", "id"], ["ge", "ka"], ["kz", "kk"]]);
function langOf(tag) {
  const parts = String(tag || "").split(/[.@]/)[0].replace(/_/g, "-").toLowerCase().split("-");
  const b = parts.shift();
  const code = b === "zh" ? (parts.some(p => ["hant", "tw", "hk", "mo"].includes(p)) ? "zh-TW" : "zh-CN")
    : b === "pt" ? (parts.includes("br") ? "pt-BR" : "pt-PT") : ALIASES.get(b) || b;
  return LANGS.find(l => l[0].toLowerCase() === code.toLowerCase())?.[0] || null;
}
// The browser's language, the first it prefers that is one of LANGS; English when none is.
const browserLang = () => (navigator.languages?.length ? navigator.languages : [navigator.language]).map(langOf).find(Boolean) || "en";
// The language chosen on this page ("" follows the browser).
function chosenLang() { const c = store.get("lang", ""); return LANGS.some(l => l[0] === c) ? c : ""; }
const langName = code => (LANGS.find(l => l[0] === code) || LANGS[0])[1];
// Dates and times as the language chosen here writes them, else as the browser does.
let dateLang = [];

// Speaks `code` from now on: its words fetched (English needs none), the page's language and
// direction set. Resolves once they are in use; a table that does not come leaves English.
function useLang(code) {
  const asked = ++langAsked;
  const use = (c, table) => {
    if (asked !== langAsked) return;
    words = new Map(Object.entries(table).filter(([, v]) => typeof v === "string" && v));
    const esc = s => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    // The longest first, so the most specific matches.
    templates = [...words].filter(([en]) => en.includes("{}")).sort((a, b) => b[0].length - a[0].length)
      .map(([en, to]) => [new RegExp("^" + en.split("{}").map(esc).join("(.+?)") + "$", "s"), to]);
    langNow = c;
    dateLang = chosenLang() ? [c] : [];
    document.documentElement.lang = c;
    if (RTL.includes(c)) document.documentElement.dir = "rtl"; else document.documentElement.removeAttribute("dir");
  };
  if (code === "en") { use("en", {}); return Promise.resolve(); }
  return fetch(base + "lang/" + code + ".json").then(r => r.ok ? r.json() : null).catch(() => null)
    .then(t => t && typeof t === "object" ? use(code, t) : use("en", {}));
}
const langReady = useLang(chosenLang() || browserLang());
const langSoon = Promise.race([langReady, new Promise(ok => setTimeout(ok, 3000))]);

// The page's own texts in the language now: data-t="English" is an element's text, data-t-title="..."
// and the like are its attributes.
const ownKids = new WeakMap();
function localize() {
  for (const e of document.querySelectorAll("*")) {
    for (const a of [...e.attributes]) {
      if (a.name === "data-t") putText(e, T(a.value));
      else if (a.name.startsWith("data-t-")) e.setAttribute(a.name.slice(7), T(a.value));
    }
  }
}
function putText(e, text) {
  if (!ownKids.has(e)) ownKids.set(e, [...e.children]);
  const kids = ownKids.get(e);
  if (!kids.length) { e.textContent = text; return; }
  // Each <b>...</b> of the sentence is the element's own next <b>, and so on.
  const out = [], used = new Map(), re = /<([a-z]+)>(.*?)<\/\1>/gs;
  let at = 0;
  for (const m of text.matchAll(re)) {
    out.push(text.slice(at, m.index));
    const n = used.get(m[1]) || 0;
    used.set(m[1], n + 1);
    const k = kids.filter(x => x.localName === m[1])[n];
    if (k) { k.textContent = m[2]; out.push(k); } else out.push(m[2]);
    at = m.index + m[0].length;
  }
  out.push(text.slice(at));
  e.replaceChildren(...out.filter(x => x !== ""));
}

// The page's theme: the system's, or light or dark as chosen here (kept in this browser).
function themeButtons() {
  const now = store.get("theme", "");
  const [glyph, title] = { light: ["sun", T("Light theme (click: dark)")], dark: ["moon", T("Dark theme (click: the system's)")] }[now] || ["auto", T("The system's theme (click: light)")];
  for (const b of document.querySelectorAll(".themebtn")) {
    b.replaceChildren(icon(glyph));
    b.title = title;
    b.setAttribute("aria-label", title);
  }
}
for (const b of document.querySelectorAll(".themebtn")) {
  b.onclick = () => {
    const next = { "": "light", light: "dark", dark: "" }[store.get("theme", "")] ?? "";
    store.set("theme", next);
    if (next) document.documentElement.dataset.theme = next;
    else delete document.documentElement.dataset.theme;
    themeButtons();
  };
}
// The page's language: the browser's, or one chosen here (kept in this browser).
for (const b of document.querySelectorAll(".langbtn")) {
  b.append(icon("globe"));
  b.onclick = () => {
    const chosen = chosenLang();
    const pick = code => { store.set("lang", code || null); useLang(code || browserLang()).then(relabel); };
    const now = chosen || browserLang();
    // Machine translated and reviewed twice: those who speak the language put it right.
    const improve = now && now !== "en" ? [null, [T("Improve this translation"), () => window.open("https://github.com/sshremote-dev/sshremote/issues/new?template=translation.yml&language=" + enc(now), "_blank", "noopener")]] : [];
    menu([[T("The system's ({})", langName(browserLang())), () => pick(""), { on: !chosen }], null,
      ...LANGS.map(([code, name]) => [name, () => pick(code), { on: chosen === code, lang: code }]), ...improve], b);
  };
}

// ----- The team server ------------------------------------------------------------------------------

class ApiError extends Error {
  constructor(message, status, data) { super(message); this.status = status; this.data = data; }
}
// A call to the server's API: its JSON answer, or an ApiError with the server's words.
async function api(method, path, body) {
  const headers = { Accept: "application/json" };
  if (token) headers.Authorization = "Bearer " + token;
  const init = { method, headers, cache: "no-store" };
  if (body !== undefined) { headers["Content-Type"] = "application/json"; init.body = JSON.stringify(body); }
  let r;
  try { r = await fetch(base + path, init); } catch { throw new ApiError(T("The team server could not be reached. Check the network, then try again."), 0, null); }
  const text = await r.text().catch(() => "");
  let data = null;
  try { data = text ? JSON.parse(text) : {}; } catch { data = null; }
  if (r.ok) return data ?? {};
  const message = typeof data?.error === "string" && data.error ? said(data.error) : T("The team server answered {}.", r.status);
  // A session that ended (expired, or signed out elsewhere) sends the page back to its sign-in.
  if (r.status === 401 && token && !path.startsWith("api/login")) ended();
  throw new ApiError(message, r.status, data);
}
// A list from an answer: the answer itself, or its first field that is one.
const listOf = (v, ...keys) => Array.isArray(v) ? v : (keys.map(k => v?.[k]).find(Array.isArray) || []);

const S = {
  user: null, team: null, options: { oidc: false, ldap: false },
  devices: [], users: [], groups: [], dgroups: [], rules: [], rulesSaved: "[]", tokens: [], events: [], settings: null, mine: [],
  view: "", loaded: new Set(), sort: { devices: ["name", 1], users: ["name", 1], mine: ["name", 1], groups: ["name", 1], dgroups: ["name", 1], tokens: ["expires", 1] },
  logo: "", focusAfter: "",
};
const teamUrl = () => String(S.team?.url || location.origin + base).replace(/\/+$/, "");
const online = d => typeof d.online === "boolean" ? d.online : nowSecs() - seen(d) < 180;
const seen = d => +(d.seen ?? d.last_seen ?? 0) || 0;

const load = {
  devices: async () => { S.devices = listOf(await api("GET", "api/admin/devices"), "devices"); },
  users: async () => { S.users = listOf(await api("GET", "api/admin/users"), "users"); },
  groups: async () => { S.groups = listOf(await api("GET", "api/admin/groups"), "groups").map(g => typeof g === "string" ? { name: g } : g); },
  dgroups: async () => { S.dgroups = listOf(await api("GET", "api/admin/device-groups"), "groups", "device_groups").map(g => typeof g === "string" ? { name: g, policy: {} } : g); },
  rules: async () => {
    // Edits not saved yet stay.
    if (rulesChanged()) return;
    S.rules = listOf(await api("GET", "api/admin/rules"), "rules").map(r => ({ who: r.who || "all", devices: r.devices || "all", rights: (r.rights ?? 0x7f) & 0x7f, ask: !!r.ask }));
    S.rulesSaved = JSON.stringify(S.rules);
  },
  tokens: async () => { S.tokens = listOf(await api("GET", "api/admin/tokens"), "tokens"); },
  settings: async () => { S.settings = await api("GET", "api/admin/settings"); },
  mine: async () => { S.mine = listOf(await api("GET", "api/devices"), "devices"); },
  me: async () => { const me = await api("GET", "api/me"); S.user = me.user; if (me.team) S.team = me.team; },
};
// What each view shows, fetched each time it opens.
const NEEDS = {
  mine: ["mine"], devices: ["devices", "dgroups"], users: ["users", "groups"], groups: ["groups", "users"], dgroups: ["dgroups", "devices"],
  rules: ["rules", "users", "groups", "dgroups", "devices"], enroll: ["tokens", "dgroups"], audit: ["devices", "users"], settings: ["settings"], account: ["me"],
};

// ----- Rights and policies ---------------------------------------------------------------------------

// The rights (src/grant.rs): each bit with its name in a list and in a sentence.
const RIGHTS = [
  [1, () => T("Use the keyboard and mouse"), () => T("keyboard and mouse")],
  [2, () => T("Copy and paste"), () => T("clipboard")],
  [4, () => T("Send and fetch files"), () => T("files")],
  [8, () => T("Hear this computer's sound and call it"), () => T("sound and calls")],
  [16, () => T("Open terminals"), () => T("terminals")],
  [32, () => T("Restart or lock this computer, block its keyboard and hide its screen"), () => T("restart and lock")],
  [64, () => T("Reach ports through this computer (tunnels)"), () => T("ports")],
];
function rightsWords(bits) {
  bits &= 0x7f;
  if (bits === 0x7f) return T("everything");
  const names = RIGHTS.filter(([b]) => bits & b).map(([, , w]) => w());
  return names.length ? names.join(", ") : T("nothing but watching");
}
const POLICY = { password: true, ask: null, rights: 0x7f, record: null, idle: null, direct: null, lan: null };
const tri = v => v === true ? "on" : v === false ? "off" : "keep";
const untri = v => v === "on" ? true : v === "off" ? false : null;

// The settings a device group keeps, as chips: what differs from the computers' own.
function policyChips(p) {
  p = { ...POLICY, ...(p || {}) };
  const c = [];
  const chip = (text, cls) => c.push(h("span", { class: "badge" + (cls ? " " + cls : ""), text }));
  if (!p.password) chip(T("Team members only"), "blue");
  if (p.ask === true) chip(T("Asks first"));
  if (p.ask === false) chip(T("Does not ask"), "warn");
  if ((p.rights & 0x7f) !== 0x7f) chip(T("At most: {}", rightsWords(p.rights)));
  if (p.record === true) chip(T("Recorded"));
  if (p.record === false) chip(T("Not recorded"));
  if (p.idle === 0) chip(T("Idle sessions go on"));
  else if (p.idle != null) chip(T("Idle end: {} min", p.idle));
  if (p.direct === true) chip(T("Straight connections on"));
  if (p.direct === false) chip(T("Straight connections off"));
  if (p.lan === true) chip(T("Local network on"));
  if (p.lan === false) chip(T("Local network off"));
  if (!c.length) chip(T("The computers' own settings"));
  return h("div", { class: "chips" }, c);
}

// The policy's fields; `value()` reads them back.
function policyEditor(p) {
  p = { ...POLICY, ...(p || {}) };
  const keepOnOff = (label, v, hint) => {
    const s = select([["keep", T("The computer's own setting")], ["on", T("On")], ["off", T("Off")]], tri(v));
    return { node: field(label, s, hint), s };
  };
  const pw = checkbox(T("Passwords let people in too"), p.password,
    T("Off: only team members with a ticket come in. The computer's passwords, one-time passwords and requests to come in are refused."));
  const ask = keepOnOff(T("Ask the person at the screen first"), p.ask);
  const record = keepOnOff(T("Record each session by ID"), p.record, T("Into the computer's Videos folder; the viewer is told."));
  const direct = keepOnOff(T("Let viewers by ID go straight through the routers"), p.direct, T("Off: through the relay only."));
  const lan = keepOnOff(T("Found and reached on the local network"), p.lan);
  const minutes = [0, 5, 15, 30, 60, 120];
  if (p.idle != null && !minutes.includes(p.idle)) minutes.push(p.idle);
  const idle = select([["keep", T("The computer's own setting")], ...minutes.sort((a, b) => a - b).map(m => [String(m), m === 0 ? T("Never") : T("After {} min without input", m)])], p.idle == null ? "keep" : String(p.idle));
  const boxes = RIGHTS.map(([bit, long]) => ({ bit, ...checkbox(long(), p.rights & bit) }));
  const rights = h("fieldset", null, h("legend", { text: T("What anyone may do there, at most") }), boxes.map(b => b.node));
  const node = h("div", null,
    pw.node,
    h("div", { class: "grid2" }, ask.node, field(T("End a session left idle"), idle), record.node, direct.node, lan.node),
    rights);
  const value = () => ({
    password: pw.input.checked, ask: untri(ask.s.value), rights: boxes.reduce((r, b) => r | (b.input.checked ? b.bit : 0), 0),
    record: untri(record.s.value), idle: idle.value === "keep" ? null : +idle.value, direct: untri(direct.s.value), lan: untri(lan.s.value),
  });
  return { node, value };
}

// ----- Tables ------------------------------------------------------------------------------------------

// A table of `rows` with `cols` [{ key, label, cell(row), sort?(row), cls }], sorted as S.sort[id]
// says; on a phone each row is a small card, its column names beside its values.
function table(id, cols, rows, empty) {
  if (!rows.length) return h("div", { class: "empty" }, empty);
  const sort = S.sort[id];
  const by = sort && cols.find(c => c.key === sort[0] && c.sort);
  if (by) rows = [...rows].sort((a, b) => sort[1] * cmp(by.sort(a), by.sort(b)) || cmp(cols[0].sort?.(a), cols[0].sort?.(b)));
  const head = h("tr", null, cols.map(c => {
    const th = h("th", { scope: "col", class: c.cls });
    if (c.cls === "acts") th.append(h("span", { class: "sr-only", text: c.label }));
    else if (c.sort) {
      const on = sort?.[0] === c.key;
      th.setAttribute("aria-sort", on ? (sort[1] > 0 ? "ascending" : "descending") : "none");
      th.append(h("button", { type: "button", class: "sortbtn", "data-k": "sort-" + id + "-" + c.key, onclick: () => { S.sort[id] = [c.key, on ? -sort[1] : 1]; render(); } },
        c.label, h("span", { class: "arrow", "aria-hidden": "true", text: on ? (sort[1] > 0 ? "\u25b2" : "\u25bc") : "" })));
    } else th.textContent = c.label;
    return th;
  }));
  const body = h("tbody", null, rows.map(r => h("tr", null, cols.map(c => h("td", { class: c.cls, "data-label": c.label }, c.cell(r))))));
  return h("div", { class: "tablewrap" }, h("table", { class: "tbl" }, h("thead", null, head), body));
}
const loading = () => h("div", { class: "empty", role: "status" }, h("div", { class: "spin" }), h("p", { text: T("Loading\u2026") }));
const dot = up => h("span", { class: "dot" + (up ? " up" : ""), role: "img", "aria-label": up ? T("Online") : T("Offline"), title: up ? T("Online") : T("Offline") });
const count = (id, shown, all) => { $(id).textContent = all ? (shown === all ? String(all) : T("{} of {}", shown, all)) : ""; };

// ----- Views ----------------------------------------------------------------------------------------------

const RENDER = {};

// A person's own computers.
RENDER.mine = () => {
  if (!S.loaded.has("mine")) { $("mine").replaceChildren(loading()); return; }
  const q = norm($("msearch").value);
  const rows = S.mine.filter(d => !q || [d.name, d.id, d.group, d.os].some(x => norm(x).includes(q)));
  count("minecount", rows.length, S.mine.length);
  $("mine").replaceChildren(table("mine", [
    { key: "name", label: T("Name"), cls: "main", sort: d => d.name || d.id, cell: d => h("span", { class: "namecell" }, dot(d.online), h("b", { text: d.name || spaced(d.id) })) },
    { key: "id", label: T("ID"), sort: d => d.id, cell: d => h("span", { class: "mono", text: spaced(d.id) }) },
    { key: "group", label: T("Device group"), sort: d => d.group || "", cell: d => d.group || dash() },
    { key: "os", label: T("System"), sort: d => d.os || "", cell: d => d.os || dash() },
    { key: "rights", label: T("You may"), cls: "wrap", cell: d => rightsWords(d.rights ?? 0) },
    { key: "ask", label: T("Asked first"), sort: d => d.ask ? 1 : 0, cell: d => d.ask ? h("span", { class: "badge", text: T("Asked first") }) : dash() },
  ], rows, S.mine.length ? h("p", { text: T("Nothing matches.") }) : h("p", { text: T("No computer of the team is open to you yet. An administrator gives access with an access rule.") })));
};

RENDER.devices = () => {
  const f = $("dfilter"), was = f.options.length ? f.value : "*";
  f.replaceChildren(...[["*", T("Every device group")], ["", T("No group")], ...S.dgroups.map(g => [g.name, g.name])].map(([v, t]) => h("option", { value: v, text: t })));
  f.value = [...f.options].some(o => o.value === was) ? was : "*";
  if (!S.loaded.has("devices")) { $("devices").replaceChildren(loading()); return; }
  const q = norm($("dsearch").value), g = f.value;
  const rows = S.devices.filter(d => (g === "*" || (d.group || "") === g) && (!q || [d.name, d.id, d.group, d.os, d.version].some(x => norm(x).includes(q))));
  count("dcount", rows.length, S.devices.length);
  $("devices").replaceChildren(table("devices", [
    { key: "name", label: T("Name"), cls: "main", sort: d => d.name || d.id, cell: d => h("span", { class: "namecell" }, dot(online(d)), h("b", { text: d.name || spaced(d.id) })) },
    { key: "id", label: T("ID"), sort: d => d.id, cell: d => h("span", { class: "mono", text: spaced(d.id) }) },
    { key: "group", label: T("Device group"), sort: d => d.group || "", cell: d => d.group || dash() },
    { key: "os", label: T("System"), sort: d => d.os || "", cell: d => d.os || dash() },
    { key: "version", label: T("Version"), sort: d => d.version || "", cell: d => d.version ? h("span", { class: "mono", text: d.version }) : dash() },
    { key: "seen", label: T("Last seen"), cls: "nowrap", sort: d => seen(d), cell: d => h("span", { title: when(seen(d)), text: online(d) ? T("Online") : ago(seen(d)) }) },
    { label: T("Actions"), cls: "acts", cell: d => [
      iconButton("rename", T("Edit {}", d.name || spaced(d.id)), { "data-k": "edit-" + d.id, onclick: () => editDevice(d) }),
      iconButton("power", T("End the sessions on {}", d.name || spaced(d.id)), { "data-k": "end-" + d.id, onclick: () => endSessions(d) }),
      iconButton("trash", T("Remove {}", d.name || spaced(d.id)), { class: "danger", "data-k": "rm-" + d.id, onclick: () => removeDevice(d) }),
    ] },
  ], rows, S.devices.length ? h("p", { text: T("Nothing matches.") }) : [
    h("p", { text: T("No computer has joined the team yet. Make a token, then run its installer line on each computer.") }),
    h("a", { class: "btn primary", href: "#enroll", text: T("Make a token") }),
  ]));
};
function groupSelect(value) {
  const names = S.dgroups.map(g => g.name);
  if (value && !names.includes(value)) names.push(value);
  return select([["", T("No group")], ...names.map(n => [n, n])], value || "");
}
function editDevice(d) {
  const name = h("input", { type: "text", maxlength: 64, autocomplete: "off", required: true });
  name.value = d.name || "";
  const group = groupSelect(d.group);
  dialog({
    title: T("Edit {}", d.name || spaced(d.id)), text: T("ID {}", spaced(d.id)),
    fields: [field(T("Name"), name), field(T("Device group"), group, T("Its group's policy applies to it, and access rules can name the group."))],
    submit: async () => {
      await api("PUT", "api/admin/devices/" + enc(d.id), { name: name.value.trim(), group: group.value });
      await load.devices();
      render();
      toast(T("Saved."));
    },
  });
}
function endSessions(d) {
  dialog({
    title: T("End the sessions on {}?", d.name || spaced(d.id)), danger: true, ok: T("End the sessions"),
    text: T("Everyone connected to it is let go when it next checks in with the team server, within a minute."),
    submit: async () => { await api("PUT", "api/admin/devices/" + enc(d.id), { end: true }); toast(T("Its sessions end within a minute.")); },
  });
}
function removeDevice(d) {
  dialog({
    title: T("Remove {} from the team?", d.name || spaced(d.id)), danger: true, ok: T("Remove"),
    text: T("It no longer takes the team's policy, and the team cannot reach it through the console's rules. It can join again with a new token."),
    submit: async () => { await api("DELETE", "api/admin/devices/" + enc(d.id)); await load.devices(); render(); toast(T("Removed.")); },
  });
}

RENDER.users = () => {
  if (!S.loaded.has("users")) { $("users").replaceChildren(loading()); return; }
  const q = norm($("usearch").value);
  const rows = S.users.filter(u => !q || [u.name, u.display, u.email, ...(u.groups || [])].some(x => norm(x).includes(q)));
  count("ucount", rows.length, S.users.length);
  $("users").replaceChildren(table("users", [
    { key: "name", label: T("Name"), cls: "main", sort: u => u.display || u.name, cell: u => h("div", { class: "who" }, h("b", { text: u.display || u.name }), u.display && u.display !== u.name ? h("small", { text: u.name }) : null) },
    { key: "email", label: T("Email"), sort: u => u.email || "", cell: u => u.email || dash() },
    { key: "groups", label: T("Groups"), cls: "wrap", cell: u => (u.groups || []).length ? h("div", { class: "chips" }, u.groups.map(g => h("span", { class: "badge", text: g }))) : dash() },
    { key: "state", label: T("Account"), sort: u => (u.admin ? 0 : 2) + (u.disabled ? 1 : 0), cell: u => h("div", { class: "chips" },
      u.name === S.user.name ? h("span", { class: "badge blue", text: T("You") }) : null,
      u.admin ? h("span", { class: "badge blue", text: T("Administrator") }) : null,
      u.disabled ? h("span", { class: "badge bad", text: T("Disabled") }) : null,
      u.totp ? h("span", { class: "badge good", text: T("Two-step sign-in") }) : null,
      !u.admin && !u.disabled && !u.totp && u.name !== S.user.name ? dash() : null) },
    { label: T("Actions"), cls: "acts", cell: u => [
      iconButton("rename", T("Edit {}", u.display || u.name), { "data-k": "edit-" + u.name, onclick: () => editUser(u) }),
      iconButton("trash", T("Remove {}", u.display || u.name), { class: "danger", "data-k": "rm-" + u.name, disabled: u.name === S.user.name, onclick: () => removeUser(u) }),
    ] },
  ], rows, S.users.length ? h("p", { text: T("Nothing matches.") }) : h("p", { text: T("No users yet.") })));
};
// A password to give someone: 16 letters and digits, none that look alike.
function makePassword() {
  const abc = "abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  return [...crypto.getRandomValues(new Uint32Array(16))].map(n => abc[n % abc.length]).join("");
}
function editUser(u) {
  const adding = !u;
  u = u || { name: "", display: "", email: "", admin: false, groups: [], disabled: false, totp: false };
  const self = !adding && u.name === S.user.name;
  const name = h("input", { type: "text", maxlength: 64, autocomplete: "off", autocapitalize: "off", spellcheck: "false", required: true });
  name.value = u.name;
  const display = h("input", { type: "text", maxlength: 80, autocomplete: "off" });
  display.value = u.display || "";
  const email = h("input", { type: "email", maxlength: 200, autocomplete: "off", spellcheck: "false" });
  email.value = u.email || "";
  const pw = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false", minlength: 8 });
  const make = textButton(null, T("Make one"), { class: "solid" });
  make.onclick = () => { pw.type = "text"; pw.value = makePassword(); pw.focus(); pw.select(); };
  const pwField = field(adding ? T("Password") : T("New password"), pw, adding
    ? T("At least 8 characters. Empty for someone who signs in with single sign-on or the directory.") : T("At least 8 characters. Empty keeps the password."));
  const pwRow = h("div", { class: "row nowrap" });
  pw.replaceWith(pwRow);
  pw.className = "grow";
  pwRow.append(pw, make);
  const names = S.groups.map(g => g.name);
  for (const g of u.groups || []) if (!names.includes(g)) names.push(g);
  const groupBoxes = names.map(n => ({ n, ...checkbox(n, (u.groups || []).includes(n)) }));
  const groups = h("fieldset", null, h("legend", { text: T("Groups") }),
    groupBoxes.length ? groupBoxes.map(b => b.node) : h("p", { class: "hint", text: T("No user groups yet: make them under Groups.") }));
  const admin = checkbox(T("Administrator"), u.admin, self ? T("You cannot take this from yourself.") : T("Manages the team in this console."), { disabled: self });
  const disabled = checkbox(T("Disabled"), u.disabled, T("Cannot sign in until this is turned off."), { disabled: self });
  const reset = !adding && u.totp ? checkbox(T("Reset the two-step sign-in"), false, T("For a lost or new phone: the password alone signs in again.")) : null;
  const clear = !adding && !self && u.password ? checkbox(T("No password"), false, T("Signs in with single sign-on or the directory only. Single sign-on takes over an account with its email only when it has no password, no two-step sign-in and is no administrator.")) : null;
  const give = u.totp ? null : checkbox(T("Two-step sign-in"), false, T("A code from an authenticator app with the password. Its key is shown once, after saving, to set up in the app."));
  dialog({
    title: adding ? T("Add a user") : T("Edit {}", u.display || u.name), wide: true, ok: adding ? T("Add") : T("Save"),
    fields: [
      h("div", { class: "grid2" }, field(T("Name"), name, adding ? T("What they type to sign in.") : ""), field(T("Display name"), display), field(T("Email"), email, "", "full")),
      pwField, groups, admin.node, disabled.node, reset?.node, give?.node, clear?.node,
    ],
    submit: async () => {
      const body = { name: name.value.trim(), display: display.value.trim(), email: email.value.trim(), admin: admin.input.checked,
        groups: groupBoxes.filter(b => b.input.checked).map(b => b.n), disabled: disabled.input.checked };
      if (pw.value) body.password = pw.value;
      else if (clear?.input.checked) body.password_clear = true;
      if (reset?.input.checked) body.totp_reset = true;
      if (give?.input.checked) body.totp = true;
      const got = adding ? await api("POST", "api/admin/users", body) : await api("PUT", "api/admin/users/" + enc(u.name), body);
      // One's own name or account changed: the frame says it too.
      if (self) { await load.me(); renderMe(); }
      await load.users();
      render();
      toast(adding ? T("{} is added.", body.name) : T("Saved."));
      return got;
    },
  }).then(got => { if (got?.totp_secret) showTotp(got.user?.display || got.user?.name || name.value, got.totp_secret, got.totp_uri, got.totp_qr); });
}
// An authenticator's key, shown once, for its person to set up in their app.
function showTotp(who, secret, uri, qr) {
  const line = (label, text) => h("div", { class: "fld" }, h("span", { class: "lab", text: label }), copyLine(text, label));
  // Only a picture the server drew (an SVG data: URL) is shown.
  const code = typeof qr === "string" && qr.startsWith("data:image/svg+xml;base64,") ? h("img", { class: "qr", src: qr, alt: T("QR code of the setup address"), width: 192, height: 192 }) : null;
  dialog({
    title: T("Two-step sign-in for {}", who), wide: true, ok: T("Done"), cancel: null,
    text: code ? T("It is shown only now. In their authenticator app, they scan this code, or add an account with the key.") : T("It is shown only now. In their authenticator app, they add an account with this key, or open the setup address on their phone."),
    fields: [code, h("div", { class: "cmds" }, line(T("Key"), secret.replace(/(.{4})(?=.)/g, "$1 ")), uri ? line(T("Setup address"), uri) : null)],
  });
}
function removeUser(u) {
  dialog({
    title: T("Remove {}?", u.display || u.name), danger: true, ok: T("Remove"),
    text: T("They can no longer sign in to the team. Access rules that name them reach nobody."),
    submit: async () => { await api("DELETE", "api/admin/users/" + enc(u.name)); await load.users(); render(); toast(T("Removed.")); },
  });
}

RENDER.groups = () => {
  if (!S.loaded.has("groups")) { $("groups").replaceChildren(loading()); return; }
  const members = g => Array.isArray(g.users) ? g.users.map(n => S.users.find(u => u.name === n) || { name: n }) : S.users.filter(u => (u.groups || []).includes(g.name));
  count("gcount", S.groups.length, S.groups.length);
  $("groups").replaceChildren(table("groups", [
    { key: "name", label: T("Name"), cls: "main", sort: g => g.name, cell: g => h("b", { text: g.name }) },
    { key: "n", label: T("People"), sort: g => members(g).length, cell: g => String(members(g).length) },
    { key: "who", label: T("Members"), cls: "wrap", cell: g => members(g).length ? h("div", { class: "chips" }, members(g).slice(0, 12).map(u => h("span", { class: "badge", text: u.display || u.name })), members(g).length > 12 ? h("span", { class: "faint", text: T("and {} more", members(g).length - 12) }) : null) : dash() },
    { label: T("Actions"), cls: "acts", cell: g => iconButton("trash", T("Remove {}", g.name), { class: "danger", "data-k": "rm-" + g.name, onclick: () => removeGroup(g, "groups") }) },
  ], S.groups, h("p", { text: T("No user groups yet.") })));
};
function addGroup(kind) {
  const name = h("input", { type: "text", maxlength: 64, autocomplete: "off", required: true });
  const dev = kind === "dgroups";
  const editor = dev ? policyEditor(POLICY) : null;
  dialog({
    title: dev ? T("Add a device group") : T("Add a group"), wide: dev, ok: T("Add"),
    fields: [field(T("Name"), name), editor?.node],
    submit: async () => {
      const n = name.value.trim();
      if ((dev ? S.dgroups : S.groups).some(g => g.name === n)) throw new Error(T("There is a group of that name already."));
      if (dev) { await api("POST", "api/admin/device-groups", { name: n, policy: editor.value() }); await load.dgroups(); }
      else { await api("POST", "api/admin/groups", { name: n }); await load.groups(); }
      render();
      toast(T("{} is added.", n));
    },
  });
}
function removeGroup(g, kind) {
  const dev = kind === "dgroups";
  dialog({
    title: T("Remove the group {}?", g.name), danger: true, ok: T("Remove"),
    text: dev ? T("Its computers stay in the team, in no group, with their own settings. Access rules for the group reach nothing.")
      : T("Its people stay in the team. Access rules for the group reach nobody."),
    submit: async () => {
      await api("DELETE", (dev ? "api/admin/device-groups/" : "api/admin/groups/") + enc(g.name));
      await (dev ? Promise.all([load.dgroups(), load.devices()]) : Promise.all([load.groups(), load.users()]));
      render();
      toast(T("Removed."));
    },
  });
}

RENDER.dgroups = () => {
  if (!S.loaded.has("dgroups")) { $("dgroups").replaceChildren(loading()); return; }
  const n = g => typeof g.devices === "number" ? g.devices : S.devices.filter(d => d.group === g.name).length;
  count("dgcount", S.dgroups.length, S.dgroups.length);
  $("dgroups").replaceChildren(table("dgroups", [
    { key: "name", label: T("Name"), cls: "main", sort: g => g.name, cell: g => h("b", { text: g.name }) },
    { key: "n", label: T("Devices"), sort: n, cell: g => String(n(g)) },
    { key: "policy", label: T("Policy"), cls: "wrap", cell: g => policyChips(g.policy) },
    { label: T("Actions"), cls: "acts", cell: g => [
      iconButton("sliders", T("Edit the policy of {}", g.name), { "data-k": "edit-" + g.name, onclick: () => editPolicy(g) }),
      iconButton("trash", T("Remove {}", g.name), { class: "danger", "data-k": "rm-" + g.name, onclick: () => removeGroup(g, "dgroups") }),
    ] },
  ], S.dgroups, h("p", { text: T("No device groups yet. Without one, every computer keeps its own settings.") })));
};
function editPolicy(g) {
  const editor = policyEditor(g.policy);
  dialog({
    title: T("The policy of {}", g.name), wide: true,
    text: T("What its computers take from the team, over their own settings. They take it within a minute."),
    fields: [editor.node],
    submit: async () => {
      await api("PUT", "api/admin/device-groups/" + enc(g.name), { policy: editor.value() });
      await load.dgroups();
      render();
      toast(T("Saved."));
    },
  });
}

// Access rules: edited here, in order, and saved together.
const rulesChanged = () => JSON.stringify(S.rules) !== S.rulesSaved;
function rulesState() {
  const dirty = rulesChanged();
  $("rsave").disabled = !dirty;
  $("rundo").disabled = !dirty;
  $("rstate").textContent = dirty ? T("Not saved yet") : "";
}
RENDER.rules = () => {
  const ol = $("rules");
  rulesState();
  if (!S.loaded.has("rules")) { ol.replaceChildren(h("li", null, loading())); return; }
  count("rcount", S.rules.length, S.rules.length);
  if (!S.rules.length) { ol.replaceChildren(h("li", { class: "empty" }, h("p", { text: T("No rules yet: add one to let people reach the team's computers.") }))); return; }
  ol.replaceChildren(...S.rules.map(ruleRow));
};
function ruleRow(r, i) {
  const users = S.users.map(u => ["user:" + u.name, u.display && u.display !== u.name ? u.display + " (" + u.name + ")" : u.name]);
  const groups = S.groups.map(g => ["group:" + g.name, g.name]);
  const whoOpts = [["all", T("Everyone in the team")], { group: T("Groups"), items: groups }, { group: T("Users"), items: users }];
  if (r.who !== "all" && ![...users, ...groups].some(o => o[0] === r.who)) whoOpts.push([r.who, T("{} (not found)", r.who.replace(/^\w+:/, ""))]);
  const dgs = S.dgroups.map(g => ["group:" + g.name, g.name]);
  const devs = S.devices.map(d => ["device:" + d.id, (d.name || spaced(d.id)) + " \u00b7 " + spaced(d.id)]);
  const devOpts = [["all", T("All devices")], { group: T("Device groups"), items: dgs }, { group: T("Devices"), items: devs }];
  if (r.devices !== "all" && ![...dgs, ...devs].some(o => o[0] === r.devices)) devOpts.push([r.devices, T("{} (not found)", r.devices.replace(/^\w+:/, ""))]);
  const who = select(whoOpts.filter(o => !o.group || o.items.length), r.who, { "data-k": "who-" + i });
  who.onchange = () => { r.who = who.value; rulesState(); };
  const where = select(devOpts.filter(o => !o.group || o.items.length), r.devices, { "data-k": "where-" + i });
  where.onchange = () => { r.devices = where.value; rulesState(); };
  const chips = RIGHTS.map(([bit, long, short]) => {
    const input = h("input", { type: "checkbox", "data-k": "right-" + i + "-" + bit });
    input.checked = !!(r.rights & bit);
    input.onchange = () => { r.rights ^= bit; rulesState(); };
    return h("label", { class: "chip", title: long() }, input, h("span", { text: short() }));
  });
  const ask = checkbox(T("Ask the person at the screen first"), r.ask, "", { "data-k": "ask-" + i });
  ask.input.onchange = () => { r.ask = ask.input.checked; rulesState(); };
  const move = (by, k) => {
    const [x] = S.rules.splice(i, 1);
    S.rules.splice(i + by, 0, x);
    S.focusAfter = k + "-" + (i + by);
    render();
  };
  const last = i === S.rules.length - 1;
  return h("li", { class: "rule", "aria-label": T("Rule {}", i + 1) },
    h("span", { class: "num", "aria-hidden": "true", text: String(i + 1) }),
    h("div", null,
      h("div", { class: "what" }, field(T("Who"), who), field(T("Which computers"), where)),
      h("div", { class: "rights", role: "group", "aria-label": T("What they may do") }, chips),
      ask.node),
    h("div", { class: "racts" },
      iconButton("up", T("Move up"), { "data-k": "up-" + i, disabled: i === 0, onclick: () => move(-1, i === 1 ? "down" : "up") }),
      iconButton("down", T("Move down"), { "data-k": "down-" + i, disabled: last, onclick: () => move(1, i + 1 === S.rules.length - 1 ? "up" : "down") }),
      iconButton("trash", T("Remove the rule"), { class: "danger", onclick: () => { S.rules.splice(i, 1); S.focusAfter = S.rules.length ? "who-" + Math.min(i, S.rules.length - 1) : "addrule"; render(); } })));
}
$("addrule").onclick = () => {
  S.rules.push({ who: "all", devices: "all", rights: 0x7f, ask: true });
  S.focusAfter = "who-" + (S.rules.length - 1);
  render();
};
$("addrule").dataset.k = "addrule";
$("rundo").onclick = () => { S.rules = JSON.parse(S.rulesSaved); $("rerror").textContent = ""; render(); };
$("rsave").onclick = async () => {
  $("rerror").textContent = "";
  $("rsave").disabled = true;
  try {
    await api("PUT", "api/admin/rules", S.rules);
    S.rulesSaved = JSON.stringify(S.rules);
    toast(T("The rules are saved."));
  } catch (e) { $("rerror").textContent = e.message; }
  render();
};
addEventListener("beforeunload", e => { if (S.user?.admin && rulesChanged()) e.preventDefault(); });

// Enrollment tokens.
RENDER.enroll = () => {
  if (!S.loaded.has("enroll")) { $("tokens").replaceChildren(loading()); return; }
  count("tcount", S.tokens.length, S.tokens.length);
  const expired = t => t.expires && t.expires < nowSecs();
  const usedUp = t => t.uses > 0 && t.used >= t.uses;
  $("tokens").replaceChildren(table("tokens", [
    { key: "group", label: T("Device group"), cls: "main", sort: t => t.group || "", cell: t => h("b", { text: t.group || T("No group") }) },
    { key: "expires", label: T("Expires"), cls: "nowrap", sort: t => t.expires || Infinity, cell: t => h("span", { class: "row" }, h("span", { text: t.expires ? when(t.expires) : T("Never") }), expired(t) ? h("span", { class: "badge bad", text: T("Expired") }) : null) },
    { key: "uses", label: T("Used"), sort: t => t.used ?? 0, cell: t => h("span", { class: "row" }, h("span", { text: t.uses > 0 ? T("{} of {}", t.used ?? 0, t.uses) : T("{}, no limit", t.used ?? 0) }), usedUp(t) ? h("span", { class: "badge", text: T("Used up") }) : null) },
    { key: "made", label: T("Made"), cls: "nowrap", sort: t => t.created || t.made || 0, cell: t => t.created || t.made ? when(t.created || t.made) : dash() },
    { label: T("Actions"), cls: "acts", cell: t => iconButton("trash", T("Revoke"), { class: "danger", "data-k": "rm-" + t.id, onclick: () => revokeToken(t) }) },
  ], S.tokens, h("p", { text: T("No tokens. Make one to add computers to the team.") })));
};
function newToken() {
  const group = groupSelect("");
  const expires = select([[3600, T("In an hour")], [86400, T("In a day")], [604800, T("In a week")], [2592000, T("In 30 days")], [31536000, T("In a year")], [0, T("Never")]], "604800");
  const uses = h("input", { type: "number", min: 1, max: 1000000, step: 1, value: 1, placeholder: T("Any number") });
  dialog({
    title: T("Make a token"), ok: T("Make the token"),
    fields: [
      field(T("Device group"), group, T("The computers that join with it go into this group.")),
      h("div", { class: "grid2" }, field(T("Expires"), expires), field(T("Computers"), uses, T("How many may join with it. Empty: any number."))),
    ],
    submit: async () => {
      const secs = +expires.value;
      return api("POST", "api/admin/tokens", { group: group.value, expires: secs ? nowSecs() + secs : 0, uses: uses.value ? Math.max(1, Math.floor(+uses.value)) : 0 });
    },
  }).then(got => {
    if (!got || got === true) return;
    load.tokens().then(render, oops);
    showToken(got.token, got.group ?? group.value, got.install);
  });
}
// A new token, shown once, with the lines that join a computer with it.
function showToken(t, group, install) {
  const url = teamUrl();
  // The server's own lines when it gives them: it knows its address best.
  const lines = {
    sh: install?.sh || "curl -fsSL \"" + url + "/install.sh?token=" + enc(t) + "\" | sh",
    ps1: install?.ps1 || "irm \"" + url + "/install.ps1?token=" + enc(t) + "\" | iex",
    join: install?.join || "sshremote team join " + url + " " + t,
  };
  const line = (label, text) => h("div", { class: "fld" }, h("span", { class: "lab", text: label }), copyLine(text, label));
  dialog({
    title: T("The new token"), wide: true, ok: T("Done"), cancel: null,
    text: group ? T("It is shown only now. A computer that runs one of these lines joins the team, in the group {}.", group)
      : T("It is shown only now. A computer that runs one of these lines joins the team."),
    fields: [h("div", { class: "cmds" },
      line(T("Linux and macOS, in a terminal"), lines.sh),
      line(T("Windows, in PowerShell"), lines.ps1),
      line(T("Where SSH Remote is installed already"), lines.join),
      line(T("The token alone"), t))],
  });
}
function revokeToken(t) {
  dialog({
    title: T("Revoke this token?"), danger: true, ok: T("Revoke"),
    text: T("No more computers can join with it. Those that joined stay in the team."),
    submit: async () => { await api("DELETE", "api/admin/tokens/" + enc(t.id)); await load.tokens(); render(); toast(T("Revoked.")); },
  });
}
$("addtoken").onclick = newToken;

// The audit log.
const KINDS = {
  in: () => T("Came in"), out: () => T("Left"), refused: () => T("Refused"), file: () => T("Files"), login: () => T("Signed in"),
  ticket: () => T("Ticket"), join: () => T("Joined the team"), leave: () => T("Left the team"), removed: () => T("Removed from the team"),
};
const HOWS = {
  team: () => T("team ticket"), password: () => T("password"), "one-time": () => T("one-time password"), request: () => T("asked at the screen"),
  oidc: () => T("single sign-on"), ldap: () => T("directory"),
};
function auditQuery(limit) {
  const q = new URLSearchParams();
  const range = $("arange").value;
  if (range === "custom") {
    const secs = v => v ? String(Math.floor(new Date(v).getTime() / 1000)) : "";
    if ($("afrom").value) q.set("from", secs($("afrom").value));
    if ($("ato").value) q.set("to", secs($("ato").value));
  } else if (range !== "all") q.set("from", String(nowSecs() - +range));
  if ($("adevice").value) q.set("device", $("adevice").value);
  if ($("auser").value.trim()) q.set("user", $("auser").value.trim());
  if ($("akind").value) q.set("kind", $("akind").value);
  if (limit) q.set("limit", String(limit));
  return q.toString();
}
const AUDIT_LIMIT = 500;
async function loadAudit() {
  try { S.events = listOf(await api("GET", "api/admin/audit?" + auditQuery(AUDIT_LIMIT)), "events"); } catch (e) { S.events = []; throw e; } finally { S.auditDone = true; }
}
function auditFilters() {
  const d = $("adevice"), was = d.value;
  d.replaceChildren(h("option", { value: "", text: T("Any device") }), ...[...S.devices].sort((a, b) => cmp(a.name || a.id, b.name || b.id)).map(x => h("option", { value: x.id, text: (x.name || spaced(x.id)) + " \u00b7 " + spaced(x.id) })));
  d.value = [...d.options].some(o => o.value === was) ? was : "";
  $("auserlist").replaceChildren(...S.users.map(u => h("option", { value: u.name, label: u.display || u.name })));
  const custom = $("arange").value === "custom";
  $("afromrow").hidden = $("atorow").hidden = !custom;
}
RENDER.audit = () => {
  auditFilters();
  if (!S.auditDone) { $("audit").replaceChildren(loading()); $("amore").textContent = ""; return; }
  const name = e => S.devices.find(x => x.id === e.device)?.name || e.device_name || "";
  const rows = [...S.events].sort((a, b) => (b.time || 0) - (a.time || 0));
  count("acount", rows.length, rows.length);
  $("audit").replaceChildren(table("audit", [
    { key: "time", label: T("Time"), cls: "nowrap", cell: e => h("span", { title: ago(e.time), text: when(e.time) }) },
    { key: "kind", label: T("Event"), cls: "main", cell: e => h("span", { class: "row" },
      h("span", { class: "badge" + (e.kind === "refused" ? " bad" : e.kind === "in" ? " good" : e.kind === "file" || e.kind === "ticket" ? " blue" : ""), text: KINDS[e.kind]?.() || e.kind || "" }),
      e.how ? h("span", { class: "dim", text: HOWS[e.how]?.() || e.how }) : null) },
    { key: "device", label: T("Device"), cell: e => e.device ? h("div", { class: "who" }, name(e) ? h("b", { text: name(e) }) : null, h("small", { class: "mono", text: spaced(e.device) })) : dash() },
    { key: "user", label: T("User"), cell: e => e.user || dash() },
    { key: "from", label: T("Came from"), cell: e => e.from ? h("span", { class: "mono", text: /^\d{9}$/.test(e.from) ? spaced(e.from) : e.from }) : dash() },
    { key: "rights", label: T("Rights"), cls: "wrap", cell: e => e.rights != null ? rightsWords(e.rights) : dash() },
    { key: "secs", label: T("Length"), cls: "nowrap", cell: e => e.secs != null ? duration(e.secs) : dash() },
    { key: "words", label: T("Details"), cls: "wrap", cell: e => e.words ? said(e.words) : dash() },
  ], rows, h("p", { text: T("Nothing in this time and with these filters.") })));
  $("amore").textContent = rows.length >= AUDIT_LIMIT ? T("The newest {} are shown: narrow the time or the filters for the rest, or download them all as CSV.", AUDIT_LIMIT) : "";
};
$("arange").onchange = () => { auditFilters(); if ($("arange").value === "custom") $("afrom").focus(); };
$("auditform").onsubmit = async e => {
  e.preventDefault();
  S.auditDone = false;
  RENDER.audit();
  try { await loadAudit(); } catch (x) { oops(x); }
  if (S.view === "audit") RENDER.audit();
};
// The CSV needs the session's header, which a link cannot carry: fetched, then saved from memory.
$("acsv").onclick = async () => {
  const b = $("acsv");
  b.disabled = true;
  try {
    let r;
    try { r = await fetch(base + "api/admin/audit.csv?" + auditQuery(0), { headers: { Authorization: "Bearer " + token }, cache: "no-store" }); }
    catch { throw new Error(T("The team server could not be reached. Check the network, then try again.")); }
    if (!r.ok) {
      const j = await r.json().catch(() => null);
      if (r.status === 401) ended();
      throw new ApiError(j?.error ? said(j.error) : T("The team server answered {}.", r.status), r.status, j);
    }
    const url = URL.createObjectURL(await r.blob());
    const a = h("a", { href: url, download: "audit-" + new Date().toISOString().slice(0, 10) + ".csv", hidden: true });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 30000);
  } catch (e) { oops(e); } finally { b.disabled = false; }
};

// Settings.
const LOGO_MOST = 256 * 1024;
function logoPreview() {
  $("slogoprev").replaceChildren(S.logo ? h("img", { src: S.logo, alt: T("The team's logo") }) : icon("image"));
  $("slogorm").disabled = !S.logo;
}
function fillSettings() {
  const s = S.settings || {};
  $("sname").value = s.name || "";
  S.logo = typeof s.logo === "string" && s.logo.startsWith("data:image/") ? s.logo : "";
  logoPreview();
  $("srelay").value = s.relay || "";
  const o = s.oidc || null, l = s.ldap || null;
  $("soidc").checked = !!o;
  $("sissuer").value = o?.issuer || "";
  $("sclient").value = o?.client_id || "";
  $("ssecret").value = "";
  S.secretSet = !!(o?.client_secret_set || s.client_secret_set);
  $("sdomains").value = (o?.domains || []).join(", ");
  $("soidcmake").checked = !!o?.make_users;
  $("soidctrust").checked = !!o?.trust_email;
  $("sldap").checked = !!l;
  $("sldapurl").value = l?.url || "";
  $("sldapdn").value = l?.user_dn || "";
  $("sldapmake").checked = !!l?.make_users;
  settingsLabels();
}
function settingsLabels() {
  $("soidcf").hidden = !$("soidc").checked;
  $("sldapf").hidden = !$("sldap").checked;
  $("ssecret").placeholder = S.secretSet ? T("Set. Type a new one to change it.") : "";
  $("ssecret").required = $("soidc").checked && !S.secretSet;
  for (const id of ["sissuer", "sclient"]) $(id).required = $("soidc").checked;
  for (const id of ["sldapurl", "sldapdn"]) $(id).required = $("sldap").checked;
  $("sredirect").textContent = S.settings?.redirect_uri || teamUrl() + "/api/oidc/callback";
}
RENDER.settings = () => {
  if (!S.loaded.has("settings")) { $("settingsform").hidden = true; return; }
  $("settingsform").hidden = false;
  settingsLabels();
};
$("soidc").onchange = $("sldap").onchange = settingsLabels;
$("sredirectcopy").onclick = () => copy($("sredirect").textContent, $("sredirectcopy"));
$("slogopick").onclick = () => $("slogofile").click();
$("slogorm").onclick = () => { S.logo = ""; logoPreview(); $("slogopick").focus(); };
$("slogofile").onchange = () => {
  const f = $("slogofile").files[0];
  $("slogofile").value = "";
  if (!f) return;
  const type = f.type || (/\.svg$/i.test(f.name) ? "image/svg+xml" : "");
  if (!["image/png", "image/svg+xml", "image/jpeg"].includes(type)) { $("serror").textContent = T("The logo is a PNG, SVG or JPEG picture."); return; }
  if (f.size > LOGO_MOST) { $("serror").textContent = T("The logo is larger than 256 KB: make it smaller first."); return; }
  const r = new FileReader();
  r.onload = () => {
    let url = String(r.result || "");
    if (!url.startsWith("data:image/")) url = "data:" + type + ";base64," + url.split(",")[1];
    S.logo = url;
    $("serror").textContent = "";
    logoPreview();
  };
  r.onerror = () => { $("serror").textContent = T("The picture could not be read."); };
  r.readAsDataURL(f);
};
$("settingsform").onsubmit = async e => {
  e.preventDefault();
  const err = $("serror");
  err.textContent = "";
  let oidc = null, ldap = null;
  if ($("soidc").checked) {
    oidc = { issuer: $("sissuer").value.trim(), client_id: $("sclient").value.trim(), domains: $("sdomains").value.split(/[\s,;]+/).map(s => s.trim().replace(/^@/, "").toLowerCase()).filter(Boolean), make_users: $("soidcmake").checked, trust_email: $("soidctrust").checked };
    if (!/^https?:\/\/\S+$/i.test(oidc.issuer)) { err.textContent = T("The issuer is an address such as https://login.example.com."); $("sissuer").focus(); return; }
    // The secret is never sent back: left empty, the one set stays.
    if ($("ssecret").value) oidc.client_secret = $("ssecret").value;
  }
  if ($("sldap").checked) {
    ldap = { url: $("sldapurl").value.trim(), user_dn: $("sldapdn").value.trim(), make_users: $("sldapmake").checked };
    if (!/^ldaps?:\/\/\S+$/i.test(ldap.url)) { err.textContent = T("The directory's address starts with ldap:// or ldaps://."); $("sldapurl").focus(); return; }
    if (!ldap.user_dn.includes("{}")) { err.textContent = T("The user DN needs {} where the name goes.", "{}"); $("sldapdn").focus(); return; }
  }
  const body = { name: $("sname").value.trim(), logo: S.logo, relay: $("srelay").value.trim(), oidc, ldap };
  $("ssave").disabled = true;
  try {
    await api("PUT", "api/admin/settings", body);
    await Promise.all([load.settings(), load.me()]);
    fillSettings();
    brand();
    renderMe();
    toast(T("The settings are saved."));
  } catch (x) { err.textContent = x.message; } finally { $("ssave").disabled = false; }
};

// The account of the person signed in.
RENDER.account = () => {
  const u = S.user;
  const fact = (k, v) => [h("dt", { text: k }), h("dd", null, v || dash())];
  $("account").replaceChildren(
    h("section", { class: "card" }, h("dl", { class: "facts" },
      fact(T("Name"), u.name),
      fact(T("Display name"), u.display),
      fact(T("Email"), u.email),
      fact(T("Role"), u.admin ? T("Administrator") : T("Member")),
      fact(T("Groups"), (u.groups || []).length ? h("div", { class: "chips" }, u.groups.map(g => h("span", { class: "badge", text: g }))) : null),
      fact(T("Two-step sign-in"), u.totp ? T("On") : T("Off")))),
    h("section", { class: "card" }, h("dl", { class: "facts" },
      fact(T("Team"), S.team?.name),
      fact(T("Address"), h("span", { class: "mono", text: teamUrl() })))),
    h("p", { class: "hint", text: T("To reach the team's computers, sign in to the team in the SSH Remote app: Settings, Team.") }));
};

// ----- The frame: who is signed in, the navigation, which view is open --------------------------------------

function brand() {
  const t = S.team || {};
  const logo = typeof t.logo === "string" && t.logo.startsWith("data:image/") ? t.logo : "";
  for (const e of document.querySelectorAll(".logo")) {
    e.classList.toggle("img", !!logo);
    e.replaceChildren(logo ? h("img", { src: logo, alt: "" }) : mark());
  }
  for (const e of document.querySelectorAll(".bname")) e.textContent = t.name || "SSH Remote";
  document.title = (t.name ? t.name + " \u00b7 " : "") + T("SSH Remote team console");
  if (logo) { $("favicon").href = logo; $("favicon").type = logo.slice(5).split(/[;,]/)[0]; }
}
function renderMe() {
  const u = S.user;
  if (!u) return;
  $("mename").textContent = u.display || u.name;
  $("merole").textContent = u.admin ? T("Administrator") : T("Member");
  $("meinitial").textContent = (u.display || u.name || "?").trim().charAt(0).toUpperCase();
  for (const e of document.querySelectorAll(".adminonly")) e.hidden = !u.admin;
  for (const e of document.querySelectorAll(".useronly")) e.hidden = !!u.admin;
}
const NAV_ICONS = { mine: "monitor", devices: "monitor", users: "user", groups: "users", dgroups: "layers", rules: "shield", enroll: "key", audit: "list", settings: "sliders" };
for (const a of document.querySelectorAll("#nav a")) a.prepend(icon(NAV_ICONS[a.dataset.view]));
$("signout").prepend(icon("logout"));
$("adduser").prepend(icon("plus"));
$("addgroup").prepend(icon("plus"));
$("adddgroup").prepend(icon("plus"));
$("addrule").prepend(icon("plus"));
$("addtoken").prepend(icon("plus"));
$("acsv").prepend(icon("download"));
$("sredirectcopy").prepend(icon("copy"));
$("donemark").append(icon("check"));
$("menubtn").append(icon("menu"));
for (const s of document.querySelectorAll(".search")) s.prepend(icon("search"));
$("adduser").onclick = () => editUser(null);
$("addgroup").onclick = () => addGroup("groups");
$("adddgroup").onclick = () => addGroup("dgroups");
$("dsearch").oninput = $("dfilter").onchange = () => RENDER.devices();
$("usearch").oninput = () => RENDER.users();
$("msearch").oninput = () => RENDER.mine();
$("menubtn").onclick = () => {
  const open = $("side").classList.toggle("open");
  $("menubtn").setAttribute("aria-expanded", String(open));
};
addEventListener("keydown", e => {
  if (e.key === "Escape" && $("side").classList.contains("open") && !document.querySelector("dialog[open]")) {
    $("side").classList.remove("open");
    $("menubtn").setAttribute("aria-expanded", "false");
    $("menubtn").focus();
  }
});

// Draws the open view again, the keyboard's place kept (or moved where an action asked).
function render() {
  const k = S.focusAfter || document.activeElement?.dataset?.k;
  S.focusAfter = "";
  RENDER[S.view]?.();
  if (k) {
    const e = document.querySelector('[data-k="' + CSS.escape(k) + '"]');
    if (e && !e.disabled && e !== document.activeElement) e.focus();
  }
}
const VIEWS = { admin: ["devices", "users", "groups", "dgroups", "rules", "enroll", "audit", "settings", "account"], user: ["mine", "account"] };
let viewAsked = 0;
async function show(view, byHand) {
  const asked = ++viewAsked;
  if (S.view === "rules" && view !== "rules" && rulesChanged()) {
    history.replaceState(null, "", "#rules");
    const leave = await dialog({ title: T("Leave the rules unsaved?"), text: T("The changes to the access rules are not saved yet."), ok: T("Leave them"), danger: true });
    if (!leave || asked !== viewAsked) return;
    S.rules = JSON.parse(S.rulesSaved);
    history.replaceState(null, "", "#" + view);
  }
  S.view = view;
  for (const s of document.querySelectorAll(".view")) s.hidden = s.id !== "v-" + view;
  for (const a of document.querySelectorAll("#nav a, .foot a.me")) {
    if (a.dataset.view === view) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
  }
  $("side").classList.remove("open");
  $("menubtn").setAttribute("aria-expanded", "false");
  if (location.hash.slice(1) !== view) history.replaceState(null, "", "#" + view);
  if (byHand) $("h-" + view)?.focus();
  render();
  try {
    await Promise.all((NEEDS[view] || []).map(n => load[n]()));
    if (view === "audit") await loadAudit();
  } catch (e) { oops(e); }
  if (asked !== viewAsked || !S.user) return;
  S.loaded.add(view);
  if (view === "settings") fillSettings();
  render();
}
function route(byHand) {
  if (!S.user) return;
  const views = S.user.admin ? VIEWS.admin : VIEWS.user;
  const want = location.hash.slice(1);
  show(views.includes(want) ? want : views[0], byHand);
}
addEventListener("hashchange", () => route(true));
// The lists that change by themselves (who is online), looked at again while they are open.
setInterval(async () => {
  if (!S.user || document.hidden || document.querySelector("dialog[open]") || openMenu) return;
  const v = S.view;
  if (v !== "devices" && v !== "mine") return;
  try { await load[v](); } catch { return; }
  if (S.view === v) render();
}, 30000);

// ----- Signing in ------------------------------------------------------------------------------------

// The app's sign-in: /login?app=CHALLENGE&port=PORT. Once in, the page goes on to the app's own
// address on this computer (127.0.0.1:PORT) with a one-time code only that app can use.
const appQuery = new URLSearchParams(location.search);
const appNonce = /\/login$/.test(location.pathname) ? appQuery.get("app") : null;
const appPort = Number(appQuery.get("port"));
const appNonceOk = !appNonce || (/^[0-9a-f]{64}$/i.test(appNonce) && Number.isInteger(appPort) && appPort >= 1024 && appPort <= 65535);

function signinLabels() {
  $("signtitle").textContent = appNonce ? T("Sign in to the team for SSH Remote") : T("Sign in to the team console");
  $("signsub").textContent = appNonce ? T("The SSH Remote app that opened this page is signed in with you.") : (S.team?.name || "");
  $("ldaphint").hidden = !S.options.ldap;
  $("ssobox").hidden = !S.options.oidc || !appNonceOk;
  $("sso").href = base + "api/oidc/start" + (appNonce ? "?app=" + enc(appNonce) + "&port=" + appPort : "");
  // The session that comes back in the address is taken only by the tab that went for it.
  $("sso").onclick = () => { try { sessionStorage.setItem("sso-begun", "1"); } catch { /* the cookie still binds it */ } };
}
function showSignin(note) {
  S.user = null;
  S.loaded.clear();
  S.view = "";
  closeDialogs();
  $("boot").hidden = $("app").hidden = true;
  $("signin").hidden = false;
  $("signform").hidden = false;
  $("appdone").hidden = $("handoff").hidden = true;
  $("signnote").textContent = note || "";
  $("lerror").textContent = "";
  $("coderow").hidden = true;
  signinLabels();
  loadOptions().then(() => { if (!S.user) { brand(); signinLabels(); } });
  if (!appNonceOk) {
    $("lerror").textContent = T("This sign-in link is not whole. Start the sign-in again from SSH Remote.");
    for (const e of $("loginform").elements) e.disabled = true;
    return;
  }
  ($("lname").value ? $("lpw") : $("lname")).focus();
}
function showDone() {
  $("boot").hidden = $("app").hidden = true;
  $("signin").hidden = false;
  $("signform").hidden = $("handoff").hidden = true;
  $("appdone").hidden = false;
  $("donetitle").focus();
}
// A single sign-on for the app came back: one click hands it to the app on this computer, so a
// link someone else made does not hand it to theirs unasked.
function showHandoff(code) {
  $("boot").hidden = $("app").hidden = true;
  $("signin").hidden = false;
  $("signform").hidden = $("appdone").hidden = true;
  $("handoff").hidden = false;
  $("handoffgo").onclick = () => { showDone(); location.replace("http://127.0.0.1:" + appPort + "/?code=" + enc(code)); };
  $("handofftitle").focus();
}
// After a 401 to a session that was in: back to the sign-in, saying why.
function ended() {
  const was = !!S.user;
  setToken("");
  if (was) showSignin(T("Your session ended. Sign in again."));
}
$("loginform").onsubmit = async e => {
  e.preventDefault();
  $("lerror").textContent = "";
  $("signnote").textContent = "";
  const body = { name: $("lname").value.trim(), password: $("lpw").value };
  const code = $("lcode").value.replace(/\s/g, "");
  if (!$("coderow").hidden && code) body.code = code;
  if (appNonce) { body.app = appNonce; body.port = appPort; }
  $("lgo").disabled = true;
  try {
    const r = await api("POST", "api/login", body);
    $("lpw").value = $("lcode").value = "";
    if (appNonce) {
      // On to the app on this computer; only a loopback address, whatever the answer says.
      if (typeof r.to === "string" && r.to.startsWith("http://127.0.0.1:" + appPort + "/?code=")) { showDone(); location.replace(r.to); return; }
      $("lerror").textContent = T("The team server did not hand the sign-in to the app. Start the sign-in again from SSH Remote.");
      return;
    }
    setToken(r.token);
    if (r.user) S.user = r.user;
    if (location.pathname !== base || location.search) history.replaceState(null, "", base + location.hash);
    await enter();
  } catch (err) {
    if (err.data?.code === true) {
      // An authenticator is set up: its code is asked for, and an error shown only for a wrong one.
      const first = $("coderow").hidden;
      $("coderow").hidden = false;
      if (!first || body.code) $("lerror").textContent = err.message;
      $("lcode").value = "";
      $("lcode").focus();
    } else {
      $("lerror").textContent = err.message;
      // A wrong code (or password: the server does not say which): the code is typed again first.
      if (err.status === 401 && body.code) { $("lcode").value = ""; $("lcode").focus(); }
      else if (err.status === 401) { $("lpw").select(); $("lpw").focus(); }
    }
  } finally { $("lgo").disabled = false; }
};
$("lcode").addEventListener("input", () => {
  // Six digits typed: no need to press the button.
  const c = $("lcode").value.replace(/\s/g, "");
  if (/^\d{6}$/.test(c) && !$("lgo").disabled) $("loginform").requestSubmit();
});
$("signout").onclick = async () => {
  try { await api("POST", "api/logout"); } catch { /* the session ends here anyway */ }
  setToken("");
  S.rulesSaved = JSON.stringify(S.rules);
  history.replaceState(null, "", base);
  showSignin(T("You are signed out."));
};

async function enter() {
  await load.me();
  if (!S.user) throw new ApiError(T("The team server gave no account."), 0, null);
  brand();
  renderMe();
  $("boot").hidden = $("signin").hidden = true;
  $("app").hidden = false;
  route(false);
}

// Everything said again once the words of another language are in.
function labels() {
  themeButtons();
  for (const b of document.querySelectorAll(".langbtn")) { b.title = T("Language"); b.setAttribute("aria-label", T("Language")); }
}
function relabel() {
  localize();
  labels();
  brand();
  signinLabels();
  if (S.user) {
    renderMe();
    if (S.view === "settings" && S.loaded.has("settings")) settingsLabels();
    render();
  }
}

// What the sign-in offers: single sign-on, the directory, the team's name and logo.
function loadOptions() {
  return api("GET", "api/login-options").then(o => {
    S.options = { oidc: !!o.oidc, ldap: !!o.ldap };
    if (o.team) S.team = o.team;
  }, () => { /* an older server: passwords only */ });
}
async function boot() {
  // A single sign-on comes back to the console with its session in the fragment: "#session=TOKEN";
  // one that failed with its words: "#error=WORDS".
  let note = "";
  const frag = location.hash;
  const m = /^#session=([^&]+)/.exec(frag), bad = /^#error=(.+)$/.exec(frag), hand = /^#handoff=([A-Za-z0-9]+)$/.exec(frag);
  if (m || bad || hand) history.replaceState(null, "", location.pathname + location.search);
  // Only in the tab that went for it: a link with someone else's session signs nobody in as them.
  let begun = false;
  try { begun = sessionStorage.getItem("sso-begun") === "1"; sessionStorage.removeItem("sso-begun"); } catch { /* none kept */ }
  if (m && begun) setToken(decodeURIComponent(m[1]));
  const options = loadOptions();
  await Promise.all([langSoon, options]);
  if (langNow !== "en") localize();
  labels();
  brand();
  if (bad) { try { note = said(decodeURIComponent(bad[1])); } catch { note = bad[1]; } }
  if (hand && appNonce && appNonceOk) { showHandoff(hand[1]); return; }
  if (!appNonce && token) {
    try { await enter(); return; } catch (e) {
      if (e.status === 401 || e.status === 403) setToken("");
      else note = e.message;
    }
  }
  showSignin(note);
}
langReady.then(() => { if (langNow !== "en") relabel(); });
boot();
