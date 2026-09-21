const choices = new Set(["slate", "paper", "sand", "contrast"]);
const key = "atlas.theme.v1";
let preference = "slate";
try { preference = localStorage.getItem(key) || preference; } catch { /* Storage may be unavailable. */ }

function apply(value) {
  const theme = choices.has(value) ? value : "slate";
  document.documentElement.dataset.theme = theme;
  document.querySelector('meta[name="color-scheme"]')?.setAttribute("content", ["paper", "sand"].includes(theme) ? "light" : "dark");
  for (const button of document.querySelectorAll("[data-theme-choice]")) {
    button.setAttribute("aria-pressed", String(button.dataset.themeChoice === theme));
  }
}
apply(preference);

export function installThemePicker() {
  const group = document.createElement("fieldset");
  group.className = "theme-picker";
  const legend = document.createElement("legend");
  legend.textContent = "Appearance";
  group.append(legend);
  for (const [value, title] of [["slate", "Slate"], ["paper", "Paper"], ["sand", "Sand"], ["contrast", "Contrast"]]) {
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.themeChoice = value;
    button.textContent = title;
    button.addEventListener("click", () => {
      apply(value);
      try { localStorage.setItem(key, value); } catch { /* The selected theme still applies to this tab. */ }
    });
    group.append(button);
  }
  document.querySelector(".workspace-file-actions")?.prepend(group);
  apply(document.documentElement.dataset.theme);
  window.addEventListener("storage", event => { if (event.key === key) apply(event.newValue); });
}
