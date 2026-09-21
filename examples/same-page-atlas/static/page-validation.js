// Installed before the application modules, so activation errors are visible
// to the page and to the connected model as well as the developer console.
(() => {
  const key = "atlas.validation-client.v1";
  let client;
  try { client = JSON.parse(sessionStorage.getItem(key) || "null"); } catch { /* new client below */ }
  if (!client?.id || !Number.isSafeInteger(client.sequence)) client = { id: crypto.randomUUID(), sequence: 0 };
  const errors = [];
  let rendered = null;
  let receipt = null;
  let timer;
  let sending = false;
  let dirty = false;
  let online = false;

  function show() {
    const panel = document.getElementById("page-validation");
    if (!panel) return;
    const layoutErrors = rendered?.errors || receipt?.layout?.errors || [];
    const status = errors.length || layoutErrors.length ? "failed" : !online ? "pending" : receipt?.status || "pending";
    panel.dataset.status = status;
    const messages = [
      ...errors.map(error => error.message),
      ...layoutErrors.map(error => `${error.detail} [${error.ids.join(", ")}]`),
      ...(receipt?.browser?.errors || []).map(error => error.message),
    ];
    const unique = [...new Set(messages)];
    panel.querySelector("summary").textContent = status === "failed"
      ? `Page issues (${unique.length})` : status === "passed" ? "Page checks passed" : "Page checks pending";
    const body = panel.querySelector(".page-validation-body");
    body.replaceChildren();
    for (const message of unique.length ? unique : [status === "passed"
      ? "Measured layout and browser runtime checks passed for the current document."
      : !online ? "Waiting for the page validation connection."
      : "Waiting for a measured browser render of the current document."]) {
      const line = document.createElement("p");
      line.textContent = message;
      body.append(line);
    }
    if (status === "failed") {
      const line = document.createElement("p");
      line.textContent = "These issues are returned to the connected agent. Runtime errors remain until the page reloads successfully.";
      body.append(line);
    }
  }

  async function publish() {
    if (sending) { dirty = true; return; }
    sending = true;
    dirty = false;
    const reportAtSend = rendered;
    const sentErrors = errors.length;
    client.sequence += 1;
    try { sessionStorage.setItem(key, JSON.stringify(client)); } catch { /* identity remains valid for this page */ }
    try {
      const response = await fetch("/atlas/validation", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ client_id: client.id, sequence: client.sequence,
          revision: rendered?.revision || null, errors: [...errors] }),
        signal: AbortSignal.timeout(4000),
      });
      if (!response.ok) throw new Error(`Page validation delivery failed (${response.status})`);
      const result = await response.json();
      // A reply about the preceding render must not repaint a newer page green.
      if (reportAtSend === rendered && sentErrors === errors.length) receipt = result;
      online = true;
    } catch {
      online = false;
      receipt = null;
    } finally {
      sending = false;
      show();
      if (dirty) schedule();
    }
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(publish, 200);
  }

  function error(code, value) {
    const message = String(value?.stack || value?.message || value || "Unknown page error").slice(0, 1000);
    if (!errors.some(item => item.code === code && item.message === message) && errors.length < 32) {
      errors.push({ code, message });
    }
    receipt = null;
    show();
    schedule();
  }

  window.addEventListener("error", event => {
    if (event instanceof ErrorEvent) error("browser_exception", event.error || event.message);
    else {
      const target = event.target;
      if (target instanceof HTMLElement) error("resource_error", `Could not load ${target.getAttribute("src") || target.getAttribute("href") || target.tagName}`);
    }
  }, true);
  window.addEventListener("unhandledrejection", event => error("unhandled_rejection", event.reason));
  document.addEventListener("agui:extensions-ready", event => {
    for (const failure of event.detail?.errors || []) {
      error("extension_error", `${failure.id}: ${failure.error}`);
    }
  });
  document.addEventListener("DOMContentLoaded", () => { show(); schedule(); });
  setInterval(publish, 5000);

  window.atlasValidation = Object.freeze({
    error,
    invalidate() { rendered = null; receipt = null; show(); schedule(); },
    rendered(report) { rendered = report; receipt = null; show(); schedule(); },
    snapshot() { return { rendered, receipt, errors: [...errors], online }; },
  });
})();
