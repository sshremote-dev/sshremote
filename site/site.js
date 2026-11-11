// Shared by every page: icons, the theme switch, the menu and copy buttons.
(function () {
  const P = {
    logo: '<rect x="2.5" y="4" width="19" height="14" rx="2.5"/><path d="M6.5 9l3 2.5-3 2.5M11.5 14h5"/><path d="M8 21h8"/>',
    terminal: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/>',
    panes: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16M12 12h9"/>',
    screen: '<rect x="2.5" y="4" width="19" height="13" rx="2"/><path d="M8 21h8M12 17v4"/><path d="M9.5 9.5l2 4 1-2 2-.5-5-1.5z"/>',
    image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="9.5" r="1.6"/><path d="M21 16l-5-5-8 9"/>',
    folder: '<path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z"/><path d="M9 13h6M12 10v6"/>',
    project: '<path d="M6 3h12v18l-6-4-6 4z"/>',
    robot: '<rect x="4" y="8" width="16" height="11" rx="3"/><path d="M12 4v4M9 13v1M15 13v1M2 13v2M22 13v2"/><circle cx="12" cy="3.5" r="1"/>',
    globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18"/>',
    puzzle: '<path d="M9 4h3a2 2 0 114 0h0v4a2 2 0 110 4v4h-4a2 2 0 10-4 0H4v-4a2 2 0 100-4V4z"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M15 8l2 2"/>',
    shield: '<path d="M12 3l8 3v6c0 4.5-3.4 8-8 9-4.6-1-8-4.5-8-9V6z"/><path d="M9 12l2 2 4-4"/>',
    relay: '<circle cx="5" cy="12" r="2.5"/><circle cx="19" cy="12" r="2.5"/><circle cx="12" cy="5" r="2.5"/><path d="M7 10.5l3-3.5M17 10.5l-3-3.5M7.5 12h9"/>',
    link: '<path d="M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1-1"/>',
    download: '<path d="M12 3v12M7 10l5 5 5-5M4 20h16"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 00-2-2H6a2 2 0 00-2 2v8a2 2 0 002 2h2"/>',
    check: '<path d="M5 12.5l4.5 4.5L19 7"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l5 5"/>',
    palette: '<path d="M12 3a9 9 0 100 18c1.2 0 2-.8 2-2 0-1.5 1-2.5 2.5-2.5H18a3 3 0 003-3C21 7 17 3 12 3z"/><circle cx="7.5" cy="11" r="1.2"/><circle cx="10" cy="7" r="1.2"/><circle cx="15" cy="7.5" r="1.2"/>',
    code: '<path d="M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/>',
    cpu: '<rect x="6" y="6" width="12" height="12" rx="2"/><rect x="9.5" y="9.5" width="5" height="5"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>',
    bolt: '<path d="M13 2L5 13h6l-1 9 8-11h-6z"/>',
    tunnel: '<path d="M3 20V11a9 9 0 0118 0v9"/><path d="M8 20v-8a4 4 0 018 0v8"/>',
    text: '<path d="M4 6V4h16v2M12 4v16M9 20h6"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    moon: '<path d="M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z"/>',
    menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
    github: '<path d="M9 19c-4 1.3-4-2-6-2.5M15 21v-3.5c0-1 .1-1.4-.5-2 2.8-.3 5.5-1.4 5.5-6a4.6 4.6 0 00-1.3-3.2 4.3 4.3 0 00-.1-3.2s-1-.3-3.4 1.3a11.6 11.6 0 00-6 0C6.8 2.8 5.8 3.1 5.8 3.1a4.3 4.3 0 00-.1 3.2A4.6 4.6 0 004.4 9.5c0 4.6 2.7 5.7 5.5 6-.6.6-.6 1.2-.5 2V21"/>',
    star: '<path d="M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z"/>',
    lock: '<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 018 0v3"/>',
    windows: '<path d="M3 5.5l7.5-1v7H3zM12.5 4.2L21 3v8.5h-8.5zM3 13h7.5v7L3 19zM12.5 13H21v8l-8.5-1.2z"/>',
    linux: '<path d="M12 3c-2.5 0-3.5 2-3.5 4.5 0 2-1 3-2 4.5S4.5 15.5 5 17.5c.3 1.3 2 2.5 3.5 2.5h7c1.5 0 3.2-1.2 3.5-2.5.5-2-.5-4-1.5-5.5s-2-2.5-2-4.5C15.5 5 14.5 3 12 3z"/><circle cx="10.5" cy="7.5" r=".6"/><circle cx="13.5" cy="7.5" r=".6"/><path d="M11 10h2"/>',
    box: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>'
  };
  const icon = (n) => '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">' + (P[n] || P.box) + '</svg>';
  window.sshIcon = icon;
  const paint = (root) => root.querySelectorAll('[data-i]').forEach((e) => { e.innerHTML = icon(e.dataset.i); e.removeAttribute('data-i'); });
  window.sshPaint = paint;

  // Theme: the system's unless chosen; the choice is kept on this browser only.
  const root = document.documentElement;
  const saved = localStorage.getItem('theme');
  if (saved) root.dataset.theme = saved;
  const dark = () => root.dataset.theme ? root.dataset.theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches;

  // Copy buttons: beside every pre.cmd and anything with data-copy.
  async function copy(btn, text) {
    try { await navigator.clipboard.writeText(text); } catch (e) {
      const t = document.createElement('textarea'); t.value = text; document.body.appendChild(t); t.select();
      document.execCommand('copy'); t.remove();
    }
    btn.innerHTML = icon('check'); btn.classList.add('done'); btn.setAttribute('aria-label', 'Copied');
    setTimeout(() => { btn.innerHTML = icon('copy'); btn.classList.remove('done'); btn.setAttribute('aria-label', 'Copy'); }, 1500);
  }
  window.sshCopyable = (scope) => scope.querySelectorAll('pre.cmd:not([data-ready])').forEach((pre) => {
    pre.dataset.ready = '1';
    const b = document.createElement('button');
    b.className = 'copy'; b.type = 'button'; b.setAttribute('aria-label', 'Copy'); b.innerHTML = icon('copy');
    b.addEventListener('click', () => copy(b, pre.querySelector('code').innerText.replace(/\s+#.*$/gm, '').trim()));
    pre.appendChild(b);
  });

  document.addEventListener('DOMContentLoaded', () => {
    paint(document);
    const t = document.getElementById('theme');
    if (t) {
      const show = () => { t.innerHTML = icon(dark() ? 'sun' : 'moon'); t.setAttribute('aria-label', dark() ? 'Use the light theme' : 'Use the dark theme'); };
      show();
      t.addEventListener('click', () => { const v = dark() ? 'light' : 'dark'; root.dataset.theme = v; localStorage.setItem('theme', v); show(); });
      matchMedia('(prefers-color-scheme: dark)').addEventListener('change', show);
    }
    const m = document.getElementById('menu'), nav = document.getElementById('nav');
    if (m && nav) m.addEventListener('click', () => { const o = nav.classList.toggle('open'); m.setAttribute('aria-expanded', o); });
    const here = location.pathname.split('/').pop() || 'index.html';
    document.querySelectorAll('#nav a').forEach((a) => { if (a.getAttribute('href') === here) a.setAttribute('aria-current', 'page'); });
    window.sshCopyable(document);
  });
})();
