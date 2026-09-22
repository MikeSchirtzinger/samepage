// Integrity-checked capability packages. No aggregate WASM or unverified retry.

function localUrl(path) {
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//") ||
      path.includes("..") || path.includes("%") || path.includes("\\")) {
    throw new Error(`artifact URL must be a same-origin absolute path: ${String(path)}`);
  }
  const url = new URL(path, location.href);
  if (url.origin !== location.origin || url.pathname.includes("..") || url.search || url.hash) {
    throw new Error(`invalid artifact URL: ${path}`);
  }
  return url.href;
}

function validHash(hash) { return typeof hash === "string" && /^[a-f0-9]{64}$/i.test(hash); }

function validateFile(file) {
  if (!file || !validHash(file.sha256) || !Number.isSafeInteger(file.bytes) || file.bytes < 1) {
    throw new Error(`invalid artifact identity: ${JSON.stringify(file)}`);
  }
  localUrl(file.url);
}

export function validateCapabilityManifest(manifest) {
  if (!manifest || manifest.protocol !== "ag-ui" || manifest.schemaVersion !== 1 ||
      manifest.capabilitySchemaVersion !== 1 || !Array.isArray(manifest.extensions)) {
    throw new Error("unsupported capability manifest version");
  }
  if (manifest.document) {
    const doc = manifest.document;
    if (doc.abi !== "samepage.document.v1" || doc.layoutVersion !== 1) {
      throw new Error("unsupported document ABI or layout version");
    }
    validateFile({ url: doc.js, sha256: doc.jsSha256, bytes: doc.jsBytes });
    validateFile({ url: doc.wasm, sha256: doc.sha256, bytes: doc.bytes });
  }
  const seen = new Map();
  for (const extension of manifest.extensions) {
    if (seen.has(extension.id)) throw new Error(`duplicate capability id ${extension.id}`);
    const artifact = extension.artifact;
    if (extension.nativeOnly) {
      const native = extension.nativeOnly;
      if (artifact || extension.loading !== "host" || extension.module ||
          native.id !== extension.id || native.version !== extension.version ||
          native.abi !== "samepage.capability.v1" || native.artifact?.type !== "native_only" ||
          typeof native.artifact.reason !== "string" || !native.artifact.reason.trim() ||
          !Array.isArray(native.kinds) || native.kinds.length ||
          !Array.isArray(native.props) || native.props.length ||
          !Array.isArray(native.reads) || native.reads.length ||
          !Array.isArray(native.actions) || native.actions.length ||
          !Array.isArray(native.visual_actions) || native.visual_actions.length ||
          !Array.isArray(native.depends_on) || !Array.isArray(native.triggers) || native.triggers.length) {
        throw new Error(`${extension.id}: invalid NativeOnly host adapter`);
      }
    }
    if (artifact) {
      if (artifact.kind !== "capability" || artifact.abi !== "samepage.capability.v1" ||
          extension.loading !== "on-demand" || extension.module !== artifact.entry?.url) {
        throw new Error(`${extension.id}: invalid capability artifact`);
      }
      for (const file of [artifact.entry, artifact.glue, artifact.wasm]) validateFile(file);
      const descriptor = artifact.descriptor;
      if (!descriptor || descriptor.id !== extension.id || descriptor.version !== extension.version ||
          descriptor.abi !== artifact.abi || descriptor.artifact?.type !== "wasm" ||
          !Number.isInteger(descriptor.schema) || descriptor.schema < 1 ||
          !Array.isArray(descriptor.kinds) || !Array.isArray(descriptor.props) ||
          !Array.isArray(descriptor.reads) || !Array.isArray(descriptor.visual_actions) ||
          !Array.isArray(descriptor.depends_on) || !Array.isArray(descriptor.triggers) ||
          descriptor.triggers.length === 0 || !Array.isArray(descriptor.actions)) {
        throw new Error(`${extension.id}: invalid host descriptor`);
      }
      for (const action of descriptor.actions) {
        if (typeof action.name !== "string" || !action.name ||
            !["human", "agent", "both"].includes(action.audience) ||
            !Array.isArray(action.aliases) ||
            !action.input_schema || typeof action.input_schema !== "object" ||
            Array.isArray(action.input_schema)) {
          throw new Error(`${extension.id}: invalid action declaration`);
        }
      }
      if (!Array.isArray(extension.actions)) throw new Error(`${extension.id}: missing browser action inventory`);
      const declared = descriptor.actions.filter((action) => action.audience !== "agent")
        .map((action) => action.name).sort();
      const exposed = [...extension.actions].sort();
      if (JSON.stringify(declared) !== JSON.stringify(exposed)) {
        throw new Error(`${extension.id}: browser action inventory differs from descriptor`);
      }
      for (const trigger of descriptor.triggers) {
        if (trigger.on === "always") {
          if (trigger.name !== undefined) throw new Error(`${extension.id}: invalid always trigger`);
        } else if (!["kind", "tool", "route"].includes(trigger.on) ||
                   typeof trigger.name !== "string" || !trigger.name) {
          throw new Error(`${extension.id}: invalid trigger`);
        }
        if (trigger.on === "tool" && !descriptor.actions.some((action) => action.name === trigger.name)) {
          throw new Error(`${extension.id}: tool trigger ${trigger.name} is not owned`);
        }
      }
    } else if (extension.loading === "on-demand") {
      throw new Error(`${extension.id}: on-demand extension has no verified artifact`);
    }
    for (const dep of (artifact?.descriptor || extension.nativeOnly)?.depends_on || []) {
      const parent = seen.get(dep.id);
      if (!parent || parent.version !== dep.version || (!parent.artifact && !parent.nativeOnly)) {
        throw new Error(`${extension.id}: dependency ${dep.id}@${dep.version} lacks a verified package or NativeOnly host adapter`);
      }
    }
    seen.set(extension.id, extension);
  }
  if ([...seen.values()].some((extension) => extension.artifact) && !manifest.document) {
    throw new Error("capability manifest is missing the document primitive");
  }
  return manifest;
}

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

async function verifiedBytes(file) {
  validateFile(file);
  const response = await fetch(localUrl(file.url), { credentials: "same-origin", cache: "no-store" });
  if (!response.ok) throw new Error(`${file.url}: HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength !== file.bytes) throw new Error(`${file.url}: byte count mismatch`);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const hash = [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  if (hash !== file.sha256.toLowerCase()) throw new Error(`${file.url}: SHA-256 mismatch`);
  return bytes;
}

async function importVerified(bytes) {
  const url = URL.createObjectURL(new Blob([bytes], { type: "text/javascript" }));
  try { return await import(url); }
  finally { URL.revokeObjectURL(url); }
}

export class CapabilityLoader {
  constructor(manifest) {
    validateCapabilityManifest(manifest);
    this.manifest = manifest;
    this.byId = new Map(manifest.extensions.map((extension) => [extension.id, extension]));
    this.instances = new Map();
    this.pending = new Map();
    this.failures = new Map();
    this.document = null;
  }

  async loadDocument() {
    if (this.document) return this.document;
    if (!this.manifest.document) return null;
    if (this.failures.has("$document")) throw this.failures.get("$document");
    if (this.pending.has("$document")) return this.pending.get("$document");
    const doc = this.manifest.document;
    const task = (async () => {
      // Both identities are checked before JavaScript import or WASM init.
      const [js, wasm] = await Promise.all([
        verifiedBytes({ url: doc.js, sha256: doc.jsSha256, bytes: doc.jsBytes }),
        verifiedBytes({ url: doc.wasm, sha256: doc.sha256, bytes: doc.bytes }),
      ]);
      const module = await importVerified(js);
      if (typeof module.default !== "function") throw new Error("document glue has no init function");
      await module.default({ module_or_path: wasm });
      if (module.abi?.() !== doc.abi || module.layout_version?.() !== doc.layoutVersion) {
        throw new Error("document exported ABI or layout version mismatch");
      }
      this.document = module;
      return module;
    })();
    this.pending.set("$document", task);
    try { return await task; }
    catch (error) { this.failures.set("$document", error); throw error; }
    finally { this.pending.delete("$document"); }
  }

  async activate(id, activateEntry) {
    if (this.instances.has(id)) return this.instances.get(id);
    if (this.failures.has(id)) throw this.failures.get(id);
    if (this.pending.has(id)) return this.pending.get(id);
    const extension = this.byId.get(id);
    if (!extension?.artifact) throw new Error(`capability ${id} is missing`);
    const task = (async () => {
      if (!this.document) await this.loadDocument();
      for (const dep of extension.artifact.descriptor.depends_on) {
        if (this.byId.get(dep.id)?.nativeOnly) continue;
        await this.activate(dep.id, activateEntry);
      }
      const { entry, glue, wasm, descriptor, abi } = extension.artifact;
      // All three byte checks complete before import or instantiation.
      const [entryBytes, glueBytes, wasmBytes] = await Promise.all([
        verifiedBytes(entry), verifiedBytes(glue), verifiedBytes(wasm),
      ]);
      const module = await importVerified(glueBytes);
      if (typeof module.default !== "function") throw new Error(`${id}: wasm glue has no init function`);
      await module.default({ module_or_path: wasmBytes });
      if (module.abi?.() !== abi) throw new Error(`${id}: exported ABI mismatch`);
      const result = typeof module.descriptor === "function" ? JSON.parse(module.descriptor()) : null;
      if (result?.abi !== abi || result?.error || canonical(result?.ok) !== canonical(descriptor)) {
        throw new Error(`${id}: exported descriptor differs from host`);
      }
      const entryModule = await importVerified(entryBytes);
      const instance = await activateEntry(extension, entryModule, module);
      this.instances.set(id, instance ?? null);
      return instance ?? null;
    })();
    this.pending.set(id, task);
    try { return await task; }
    catch (error) { this.failures.set(id, error); throw error; }
    finally { this.pending.delete(id); }
  }

  async activateFor(on, name, activateEntry) {
    const matches = this.manifest.extensions.filter((extension) =>
      extension.artifact?.descriptor.triggers.some((trigger) => trigger.on === on &&
        (on === "always" || trigger.name === name)));
    return Promise.all(matches.map((extension) => this.activate(extension.id, activateEntry)));
  }
}
