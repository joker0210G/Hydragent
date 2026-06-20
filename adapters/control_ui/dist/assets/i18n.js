// Hydragent Control UI — lightweight i18n helper
// Pulls `./lang/<locale>.json` once on first use, falls back to English.
(function () {
  const SUPPORTED = ["en", "zh-CN", "de", "es", "fr", "ja-JP", "pt-BR"];
  const FALLBACK = "en";
  const cache = new Map();
  const subscribers = new Set();
  let current = FALLBACK;
  let messages = {};

  function detect() {
    const stored = localStorage.getItem("hydra.locale");
    if (stored && SUPPORTED.includes(stored)) return stored;
    const nav = (navigator.language || "en").toLowerCase();
    const match = SUPPORTED.find((l) => l.toLowerCase() === nav) ||
                  SUPPORTED.find((l) => l.toLowerCase().startsWith(nav.split("-")[0]));
    return match || FALLBACK;
  }

  async function load(locale) {
    if (cache.has(locale)) return cache.get(locale);
    try {
      const res = await fetch(`./lang/${locale}.json`, { cache: "no-cache" });
      if (!res.ok) throw new Error(res.statusText);
      const data = await res.json();
      cache.set(locale, data);
      return data;
    } catch (e) {
      console.warn("i18n: failed to load", locale, e);
      cache.set(locale, {});
      return {};
    }
  }

  function t(key, vars) {
    const raw = messages[key] || (cache.get(FALLBACK) || {})[key] || key;
    if (!vars) return raw;
    return Object.keys(vars).reduce(
      (acc, k) => acc.replace(new RegExp(`\\{${k}\\}`, "g"), vars[k]),
      raw
    );
  }

  function onChange(fn) { subscribers.add(fn); }

  function applyToDom() {
    document.documentElement.lang = current;
    document.querySelectorAll("[data-i18n]").forEach((el) => {
      const key = el.getAttribute("data-i18n");
      const v = t(key);
      if (typeof v === "string") el.textContent = v;
    });
    document.querySelectorAll("[data-i18n-attr]").forEach((el) => {
      el.getAttribute("data-i18n-attr").split(",").forEach((spec) => {
        const [attr, key] = spec.split(":").map((s) => s.trim());
        if (attr && key) el.setAttribute(attr, t(key));
      });
    });
    subscribers.forEach((fn) => { try { fn(current); } catch (e) { /* ignore */ } });
  }

  async function setLocale(locale) {
    if (!SUPPORTED.includes(locale)) locale = FALLBACK;
    current = locale;
    localStorage.setItem("hydra.locale", locale);
    messages = await load(locale);
    applyToDom();
  }

  window.HydraI18n = {
    init: async function () {
      const target = detect();
      await setLocale(target);
      return current;
    },
    t, setLocale,
    get locale() { return current; },
    get supported() { return SUPPORTED.slice(); },
    onChange,
  };
})();
