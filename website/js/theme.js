// Light/dark mode for every page. Loaded from <head>, so <html data-theme> is set before the page first draws (no
// flash): the visitor's choice, kept in localStorage, or else the system's, which is then followed as it changes.
// Adds the switch button (styled as button.theme in style.css) and fires "themechange" on window after each change.
(() => {
  const root = document.documentElement, system = matchMedia("(prefers-color-scheme: dark)");
  let chosen = null;
  try { chosen = localStorage.getItem("theme"); } catch {}
  if (chosen !== "light" && chosen !== "dark") chosen = null;

  let button = null;
  function apply(theme) {
    root.dataset.theme = theme;
    if (button) {
      const label = theme === "dark" ? "Switch to light mode" : "Switch to dark mode";
      button.title = label;
      button.setAttribute("aria-label", label);
    }
    dispatchEvent(new Event("themechange"));
  }
  apply(chosen ?? (system.matches ? "dark" : "light"));
  system.addEventListener("change", (e) => { if (!chosen) apply(e.matches ? "dark" : "light"); });

  document.addEventListener("DOMContentLoaded", () => {
    button = document.createElement("button");
    button.className = "theme";
    button.innerHTML =
      '<svg class="moon" viewBox="0 0 16 16"><path d="M6.2 1.3a6.8 6.8 0 1 0 8.5 8.5A5.6 5.6 0 0 1 6.2 1.3z"/></svg>' +
      '<svg class="sun" viewBox="0 0 16 16"><circle cx="8" cy="8" r="3.2"/><path d="M7.3 0h1.4v2.6H7.3zm0 13.4h1.4V16H7.3zM0 7.3h2.6v1.4H0zm13.4 0H16v1.4h-2.6zM2.1 3.1l1-1 1.9 1.9-1 1zm8.9 8.9 1-1 1.9 1.9-1 1zM3.1 13.9l-1-1 1.9-1.9 1 1zm8.9-8.9-1-1 1.9-1.9 1 1z"/></svg>';
    button.onclick = () => {
      chosen = root.dataset.theme === "dark" ? "light" : "dark";
      try { localStorage.setItem("theme", chosen); } catch {}
      apply(chosen);
    };
    document.body.prepend(button);
    apply(root.dataset.theme);
  });
})();
