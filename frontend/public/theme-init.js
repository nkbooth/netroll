// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
// FOUC guard: stamp the theme before first paint. Mirrors src/ui/theme.ts
// (storage key + dark default) — change both together; theme-fouc.test.ts
// fails if they drift.
//
// Kept OUT of index.html as an inline script so the served CSP can be
// `script-src 'self'` with no hash to rotate. Loaded render-blocking from
// <head>: `defer`/`async` would paint the body first, which is the exact
// flash this prevents.
(function () {
  var theme = 'dark';
  try {
    var stored = localStorage.getItem('netroll-theme');
    if (stored === 'light' || stored === 'dark') theme = stored;
  } catch (error) {
    // Storage unavailable — keep the dark default.
  }
  document.documentElement.dataset.theme = theme;
})();
