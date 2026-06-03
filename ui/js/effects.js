/* ═══════════════════════════════════════════════════════════
   BACKPACK // MOTION
   Minimal, tasteful motion: a flat background layer and a
   subtle scroll-reveal. Classic system cursor. No deps.
   ═══════════════════════════════════════════════════════════ */
(function () {
  'use strict';

  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  function el(tag, cls) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    return n;
  }

  function injectBackground() {
    if (document.querySelector('.cy-bg')) return;
    document.body.prepend(el('div', 'cy-bg'));
  }

  function startReveal() {
    const targets = document.querySelectorAll('.section, .page-header, .stats-grid, .ai-note-card');
    targets.forEach((t) => { if (!t.hasAttribute('data-reveal')) t.setAttribute('data-reveal', ''); });
    document.querySelectorAll('[data-reveal]').forEach((t, i) => { t.style.transitionDelay = (Math.min(i, 6) * 0.06) + 's'; });

    if (reduceMotion || !('IntersectionObserver' in window)) {
      document.querySelectorAll('[data-reveal]').forEach((t) => t.classList.add('in'));
      return;
    }
    const io = new IntersectionObserver((entries) => {
      entries.forEach((en) => { if (en.isIntersecting) { en.target.classList.add('in'); io.unobserve(en.target); } });
    }, { threshold: 0.08 });
    document.querySelectorAll('[data-reveal]').forEach((t) => io.observe(t));
    // safety: never leave content hidden
    setTimeout(() => document.querySelectorAll('[data-reveal]:not(.in)').forEach((t) => t.classList.add('in')), 1600);
  }

  function init() {
    injectBackground();
    startReveal();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
