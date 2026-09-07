// AG-UI browser core: protocol transport, shared actions, and manifest-driven
// extension loading. UI-free on purpose; application chrome and extensions
// subscribe to the same event stream without copying transport logic.

export const AGUI_CLIENT_VERSION = "1";

/// The in-flight join, so a page joins the room exactly once.
///
/// Memoized at module scope rather than per caller because the shell and every
/// extension all need to know who this browser is, they all load at once, and
/// none of them holds a cookie yet on a first visit. Called twice, the server
/// correctly admits two people, it has nothing to tell the two requests apart
/// with, and one browser ends up as two participants, each seeing half its own
/// writing signed by a stranger. One promise, shared by everyone, is the fix.
let joining = null;

/// Who this browser is, as the host minted it.
///
/// Resolves to `null` when the surface has no identity endpoint or it failed:
/// a page that cannot learn who it is still renders every byline, it just shows
/// this person their own name instead of "you".
export function whoAmI() {
  if (!joining) {
    joining = fetch("/surface/me", { method: "POST", credentials: "same-origin" })
      .then((response) => (response.ok ? response.json() : null))
      .then((body) => (body && body.joined ? body : null))
      .catch(() => null);
  }
  return joining;
}

/// Replace the cached identity after a rename, so a second reader of `whoAmI`
/// does not get the name this person just stopped using.
export function rememberMe(person) {
  joining = Promise.resolve(person || null);
}

export class AgUiRequestError extends Error {
  constructor(message, { status = 0, body = null } = {}) {
    super(message);
    this.name = "AgUiRequestError";
    this.status = status;
    this.body = body;
  }
}

export class AgUiClient extends EventTarget {
  constructor({ eventsUrl = "/events" } = {}) {
    super();
    this.eventsUrl = eventsUrl;
    this.source = null;
    this.reconnectEnabled = false;
  }

  on(topic, handler) {
    const listener = (event) => handler(event.detail);
    this.addEventListener(topic, listener);
    return () => this.removeEventListener(topic, listener);
  }

  emit(topic, detail) {
    this.dispatchEvent(new CustomEvent(topic, { detail }));
  }

  connect() {
    if (this.source) return this.source;
    this.reconnectEnabled = true;
    const source = new EventSource(this.eventsUrl);
    this.source = source;
    source.onopen = () => this.emit("connection:open", { url: this.eventsUrl });
    source.onerror = (error) => this.emit("connection:error", error);
    source.onmessage = (message) => {
      let event;
      try {
        event = JSON.parse(message.data);
      } catch {
        this.restartAfterProtocolError({ error: "invalid JSON event", data: message.data });
        return;
      }
      if (!event || typeof event.type !== "string" || event.type.length === 0) {
        this.restartAfterProtocolError({ error: "event is missing a nonempty type", data: message.data });
        return;
      }
      this.emit("event", event);
      this.emit(event.type, event);
      if (event.type === "CUSTOM" && typeof event.name === "string") {
        this.emit(event.name, event);
      }
    };
    return source;
  }

  close() {
    this.reconnectEnabled = false;
    if (!this.source) return;
    this.source.close();
    this.source = null;
  }

  restartAfterProtocolError(detail) {
    this.emit("protocol:error", detail);
    const failed = this.source;
    this.source = null;
    if (failed) failed.close();
    setTimeout(() => {
      if (this.reconnectEnabled && !this.source) this.connect();
    }, 1000);
  }

  async getJson(path) {
    const response = await fetch(path, { headers: { accept: "application/json" } });
    return readJson(response);
  }

  async postJson(path, body = {}) {
    const response = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
    });
    return readJson(response);
  }

  action(name, args = {}) {
    return this.postJson("/surface/action", { name, args });
  }

  ask(question, origin = "") {
    return this.postJson("/ask", { question, origin });
  }

  interrupt() {
    return this.postJson("/interrupt");
  }

  semantic(name, value = {}) {
    return this.postJson("/semantic", { name, value });
  }

  decision(reply) {
    return this.postJson("/decision", reply);
  }
}

async function readJson(response) {
  let body;
  try {
    body = await response.json();
  } catch (error) {
    const message = response.ok
      ? "successful response was not valid JSON"
      : `${response.status} ${response.statusText || "request failed"}: non-JSON response`;
    throw new AgUiRequestError(message, {
      status: response.status,
      body: { parseError: String(error) },
    });
  }
  if (!response.ok || (body && body.ok === false)) {
    const message =
      (body && (body.error || body.result)) ||
      `${response.status} ${response.statusText || "request failed"}`;
    throw new AgUiRequestError(message, { status: response.status, body });
  }
  return body;
}

export async function loadExtensions(client, { manifestUrl = "/extensions" } = {}) {
  if (!(client instanceof AgUiClient)) {
    throw new TypeError("loadExtensions requires an AgUiClient");
  }
  const manifest = validateManifest(await client.getJson(manifestUrl));
  const loaded = [];
  const seen = new Set();

  for (const extension of manifest.extensions) {
    if (seen.has(extension.id)) {
      loaded.push(extensionFailure(extension, null, `duplicate extension id: ${extension.id}`));
      continue;
    }
    seen.add(extension.id);
    const mount = document.getElementById(extension.mount);

    if (extension.loading === "host") {
      loaded.push({ id: extension.id, status: "host", extension, mount, instance: null });
      continue;
    }

    if (!mount) {
      loaded.push(extensionFailure(extension, null, `mount #${extension.mount} was not found`));
      continue;
    }
    mount.dataset.extensionId = extension.id;
    mount.dataset.extensionState = "loading";

    try {
      if (!extension.module) throw new Error("lazy extension has no module URL");
      const moduleUrl = sameOriginUrl(extension.module);
      const imported = await import(moduleUrl.href);
      const activate = imported.activate || imported.default;
      if (typeof activate !== "function") {
        throw new Error("entry module must export activate(ctx) or a default function");
      }
      const extensionClient = Object.freeze({
        on: (topic, handler) => client.on(topic, handler),
      });
      const context = Object.freeze({
        client: extensionClient,
        extension: Object.freeze({ ...extension }),
        mount,
        // Who this browser is, so an extension can tell this person's own
        // writing from everybody else's. Handed down rather than fetched per
        // extension: joining is the one request that must happen exactly once
        // per page, and every caller doing it themselves is how one browser
        // became several participants.
        whoAmI,
        action: (name, args) => {
          if (!extension.actions.includes(name)) {
            throw new Error(`extension ${extension.id} is not allowed to invoke action ${name}`);
          }
          return client.action(name, args);
        },
        semantic: (name, value) => client.semantic(name, value),
      });
      const instance = (await activate(context)) || null;
      mount.dataset.extensionState = "ready";
      const result = { id: extension.id, status: "ready", extension, mount, instance };
      loaded.push(result);
      emitDocument("agui:extension-loaded", result);
    } catch (error) {
      const result = extensionFailure(extension, mount, error);
      loaded.push(result);
      if (extension.required) showRequiredFailure(mount, result.error);
    }
  }

  const runtime = {
    manifest,
    loaded,
    byId: new Map(loaded.map((entry) => [entry.id, entry])),
    errors: loaded.filter((entry) => entry.status === "error"),
  };
  emitDocument("agui:extensions-ready", runtime);
  return runtime;
}

export function validateManifest(manifest) {
  if (!manifest || manifest.protocol !== "ag-ui" || manifest.schemaVersion !== 1) {
    throw new Error("unsupported AG-UI extension manifest");
  }
  if (!Array.isArray(manifest.extensions)) {
    throw new Error("extension manifest is missing extensions[]");
  }
  for (const extension of manifest.extensions) {
    if (!extension || typeof extension.id !== "string" || typeof extension.mount !== "string") {
      throw new Error("extension manifest contains an invalid descriptor");
    }
    if (extension.loading !== "lazy" && extension.loading !== "host") {
      throw new Error(`extension ${extension.id} has an unsupported loading mode`);
    }
    for (const field of ["actions", "events", "capabilities"]) {
      if (!Array.isArray(extension[field]) || !extension[field].every((value) => typeof value === "string")) {
        throw new Error(`extension ${extension.id} has invalid ${field}`);
      }
    }
    if (extension.loading === "lazy") sameOriginUrl(extension.module);
    if (extension.wasm) {
      sameOriginUrl(extension.wasm.js);
      sameOriginUrl(extension.wasm.wasm);
    }
  }
  return manifest;
}

function sameOriginUrl(path) {
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) {
    throw new Error(`extension asset must be a same-origin absolute path: ${String(path)}`);
  }
  const url = new URL(path, location.href);
  if (url.origin !== location.origin) {
    throw new Error(`cross-origin extension asset rejected: ${url.href}`);
  }
  return url;
}

function extensionFailure(extension, mount, error) {
  const message = error instanceof Error ? error.message : String(error);
  if (mount) mount.dataset.extensionState = "error";
  const result = { id: extension.id, status: "error", extension, mount, error: message };
  console.error(`[ag-ui] extension ${extension.id} failed:`, error);
  emitDocument("agui:extension-error", result);
  return result;
}

function showRequiredFailure(mount, message) {
  const error = document.createElement("div");
  error.className = "agui-extension-error";
  error.setAttribute("role", "alert");
  error.textContent = `Extension unavailable: ${message}`;

  if (!mount || mount.tagName === "CANVAS") {
    if (mount) mount.setAttribute("aria-label", `Required extension failed: ${message}`);
    // A canvas cannot contain fallback DOM, and full-screen canvases commonly
    // cover ordinary siblings. Keep required failures visibly above the app so
    // a broken renderer can never look like a successfully blank surface.
    Object.assign(error.style, {
      position: "fixed",
      inset: "1rem 1rem auto",
      zIndex: "2147483647",
      padding: "0.75rem 1rem",
      border: "1px solid #ef4444",
      borderRadius: "0.5rem",
      background: "#450a0a",
      color: "#fecaca",
      font: "600 0.875rem/1.4 system-ui, sans-serif",
    });
    (mount?.parentElement || document.body || document.documentElement).appendChild(error);
    return;
  }

  mount.setAttribute("aria-label", `Required extension failed: ${message}`);
  mount.replaceChildren(error);
}

function emitDocument(name, detail) {
  document.dispatchEvent(new CustomEvent(name, { detail }));
}
