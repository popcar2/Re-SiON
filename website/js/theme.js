// Light/dark mode for every page. Loaded from <head>, so <html data-theme> is set before the page first draws (no
// flash): the visitor's choice, kept in localStorage, or else the system's, which is then followed as it changes.
// Adds the switch button (styled as button.theme in style.css) and the GitHub link in the opposite corner (a.github),
// and fires "themechange" on window after each change.
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
    const github = document.createElement("a");
    github.className = "github";
    github.href = "https://github.com/popcar2/Re-SiON";
    github.target = "_blank";
    github.rel = "noopener";
    github.title = "Re-SiON on GitHub";
    github.setAttribute("aria-label", github.title);
    github.innerHTML = '<svg viewBox="0 0 16 16"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>';
    document.body.prepend(button, github);
    apply(root.dataset.theme);
  });
})();
