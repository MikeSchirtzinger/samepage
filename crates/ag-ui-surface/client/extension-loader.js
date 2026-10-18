// Extension discovery and activation, separate from the transport core.
import { AgUiClient, whoAmI } from "/_agui/client.js";
import {
  CapabilityLoader,
  triggeredCapabilityIdsForView,
  validateCapabilityManifest,
} from "/_agui/capability-loader.js";

export async function loadExtensions(client, { manifestUrl = "/extensions", createApplicationContext } = {}) {
  if (!(client instanceof AgUiClient)) {
    throw new TypeError("loadExtensions requires an AgUiClient");
  }
  const manifest = validateManifest(await client.getJson(manifestUrl));
  if (manifest.capabilitySchemaVersion !== undefined) validateCapabilityManifest(manifest);
  const capabilityLoader = manifest.capabilitySchemaVersion === 1 ? new CapabilityLoader(manifest) : null;
  // A capability-aware app cannot enter the event stream without its one
  // verified document primitive. C4 constructs the one Replica from this.
  let documentModule = null;
  if (capabilityLoader) {
    try {
      documentModule = await capabilityLoader.loadDocument();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      showRequiredFailure(null, `Document unavailable: ${message}`);
      emitDocument("agui:document-error", { error: message });
      throw error;
    }
  }
  const application = createApplicationContext
    ? await createApplicationContext({ documentModule, manifest })
    : null;
  const loaded = [];
  const seen = new Set();
  const failEntry = (entry, error) => {
    entry.status = "failed";
    entry.error = error instanceof Error ? error.message : String(error);
    if (entry.mount) entry.mount.dataset.extensionState = "failed";
    if (entry.extension.required && !entry.failureShown) {
      showRequiredFailure(entry.mount, entry.error);
      entry.failureShown = true;
    }
  };

  const activateEntry = async (extension, imported, verifiedWasm = null) => {
    const mount = document.getElementById(extension.mount);
    if (!mount) throw new Error(`mount #${extension.mount} was not found`);
    const activate = imported.activate || imported.default;
    if (typeof activate !== "function") {
      throw new Error("entry module must export activate(ctx) or a default function");
    }
    const extensionClient = Object.freeze({ on: (topic, handler) => client.on(topic, handler) });
    const context = Object.freeze({
      client: extensionClient,
      extension: Object.freeze({ ...extension }),
      mount,
      whoAmI,
      documentModule,
      application,
      wasm: verifiedWasm,
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
    return instance;
  };

  const activateCapability = async (extension, imported, wasm) => {
    const entry = byId.get(extension.id);
    try {
      const instance = await activateEntry(extension, imported, wasm);
      Object.assign(entry, { status: "ready", instance, error: undefined });
      emitDocument("agui:extension-loaded", entry);
      return instance;
    } catch (error) {
      const failed = extensionFailure(extension, entry.mount, error);
      failEntry(entry, failed.error);
      throw error;
    }
  };
  const byId = new Map();

  for (const extension of manifest.extensions) {
    if (seen.has(extension.id)) {
      loaded.push(extensionFailure(extension, null, `duplicate extension id: ${extension.id}`));
      continue;
    }
    seen.add(extension.id);
    const mount = document.getElementById(extension.mount);

    if (extension.loading === "host") {
      const result = { id: extension.id, status: "host", extension, mount, instance: null };
      loaded.push(result); byId.set(extension.id, result);
      continue;
    }

    if (!mount) {
      loaded.push(extensionFailure(extension, null, `mount #${extension.mount} was not found`));
      continue;
    }
    mount.dataset.extensionId = extension.id;
    if (extension.loading === "on-demand") {
      mount.dataset.extensionState = "pending";
      const result = { id: extension.id, status: "pending", extension, mount, instance: null };
      loaded.push(result); byId.set(extension.id, result);
      continue;
    }
    mount.dataset.extensionState = "loading";

    try {
      if (!extension.module) throw new Error("lazy extension has no module URL");
      const moduleUrl = sameOriginUrl(extension.module);
      const imported = await import(moduleUrl.href);
      const instance = await activateEntry(extension, imported);
      const result = { id: extension.id, status: "ready", extension, mount, instance };
      loaded.push(result); byId.set(extension.id, result);
      emitDocument("agui:extension-loaded", result);
    } catch (error) {
      const result = extensionFailure(extension, mount, error);
      loaded.push(result); byId.set(extension.id, result);
      if (extension.required) showRequiredFailure(mount, result.error);
    }
  }

  const activate = async (id) => {
    if (!capabilityLoader) throw new Error("this surface has no capability loader");
    const entry = byId.get(id);
    if (!entry) throw new Error(`unknown extension ${id}`);
    if (entry.status === "ready") return entry.instance;
    if (entry.status === "failed") throw new Error(entry.error);
    entry.status = "loading";
    if (entry.mount) entry.mount.dataset.extensionState = "loading";
    try {
      return await capabilityLoader.activate(id, activateCapability);
    } catch (error) {
      for (const [failedId, failedError] of capabilityLoader.failures) {
        const failedEntry = byId.get(failedId);
        if (failedEntry) failEntry(failedEntry, failedError);
      }
      failEntry(entry, error);
      throw error;
    }
  };
  const activateFor = (on, name) => capabilityLoader
    ? Promise.all(manifest.extensions
        .filter((extension) => extension.artifact?.descriptor.triggers.some((trigger) =>
          trigger.on === on && (on === "always" || trigger.name === name)))
        .map((extension) => activate(extension.id)))
    : Promise.resolve([]);
  const activateForView = (view) => capabilityLoader
    ? Promise.all(triggeredCapabilityIdsForView(manifest.extensions, view).map(activate))
    : Promise.resolve([]);
  if (capabilityLoader) {
    // Dependencies and Always artifacts are verified and ready before /events.
    await activateFor("always", null);
    await activateFor("route", location.pathname);
  }

  const runtime = {
    manifest,
    loaded,
    byId,
    documentModule,
    application,
    capabilityLoader,
    activate,
    activateForKind: (kind) => activateFor("kind", kind),
    activateForView,
    activateForTool: (tool) => activateFor("tool", tool),
    activateForRoute: (route) => activateFor("route", route),
    get errors() { return loaded.filter((entry) => entry.status === "error" || entry.status === "failed"); },
  };
  if (capabilityLoader) client.beforeAction = runtime.activateForTool;
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
    if (extension.loading !== "lazy" && extension.loading !== "host" && extension.loading !== "on-demand") {
      throw new Error(`extension ${extension.id} has an unsupported loading mode`);
    }
    for (const field of ["actions", "events", "capabilities"]) {
      if (!Array.isArray(extension[field]) || !extension[field].every((value) => typeof value === "string")) {
        throw new Error(`extension ${extension.id} has invalid ${field}`);
      }
    }
    if (extension.loading === "lazy" || extension.loading === "on-demand") sameOriginUrl(extension.module);
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
