#!/usr/bin/env node

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import path from "node:path";

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) throw new Error(`invalid argument near ${key ?? "end"}`);
    values[key.slice(2)] = value;
  }
  for (const key of ["port", "tab", "url", "driver", "input", "output", "result", "timeout-ms"]) {
    if (!values[key]) throw new Error(`missing --${key}`);
  }
  return values;
}

function remoteValue(remote) {
  if (Object.hasOwn(remote, "value")) return remote.value;
  if (remote.unserializableValue) return remote.unserializableValue;
  return remote.description ?? remote.type;
}

class CdpConnection {
  constructor(webSocketUrl, record) {
    this.webSocketUrl = webSocketUrl;
    this.record = record;
    this.socket = null;
    this.nextId = 1;
    this.pending = new Map();
    this.waiters = [];
  }

  async connect() {
    this.socket = new WebSocket(this.webSocketUrl);
    await new Promise((resolve, reject) => {
      this.socket.addEventListener("open", resolve, { once: true });
      this.socket.addEventListener("error", () => reject(new Error("CDP WebSocket connection failed")), { once: true });
    });
    this.socket.addEventListener("message", (event) => this.onMessage(JSON.parse(String(event.data))));
    this.socket.addEventListener("close", () => {
      for (const { reject } of this.pending.values()) reject(new Error("CDP WebSocket closed"));
      this.pending.clear();
    });
  }

  onMessage(message) {
    if (message.id) {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      else pending.resolve(message.result ?? {});
      return;
    }
    this.record(message);
    const retained = [];
    for (const waiter of this.waiters) {
      if (waiter.method === message.method && (!waiter.predicate || waiter.predicate(message.params ?? {}))) {
        clearTimeout(waiter.timer);
        waiter.resolve(message.params ?? {});
      } else {
        retained.push(waiter);
      }
    }
    this.waiters = retained;
    if (message.method === "Target.attachedToTarget") this.enableAttachedTarget(message).catch(this.record);
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId++;
    const message = { id, method, params };
    if (sessionId) message.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.socket.send(JSON.stringify(message));
    });
  }

  waitForEvent(method, timeoutMs, predicate = null) {
    return new Promise((resolve, reject) => {
      const waiter = { method, predicate, resolve, reject, timer: null };
      waiter.timer = setTimeout(() => {
        this.waiters = this.waiters.filter((candidate) => candidate !== waiter);
        reject(new Error(`timed out waiting for ${method}`));
      }, timeoutMs);
      this.waiters.push(waiter);
    });
  }

  async enableAttachedTarget(message) {
    const { sessionId, targetInfo } = message.params;
    this.record({ method: "AtlasExperiment.workerAttached", params: targetInfo, sessionId });
    await Promise.allSettled([
      this.send("Runtime.enable", {}, sessionId),
      this.send("Log.enable", {}, sessionId),
    ]);
  }

  close() {
    this.socket?.close();
  }
}

async function targetFor(port, tabId) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`);
  if (!response.ok) throw new Error(`Chrome target listing failed with HTTP ${response.status}`);
  const targets = await response.json();
  const target = targets.find((candidate) => candidate.id === tabId);
  if (!target?.webSocketDebuggerUrl) throw new Error(`pinned Chrome tab ${tabId} is unavailable`);
  return target;
}

function expressionFailure(result, expression) {
  const details = result.exceptionDetails;
  if (!details) return null;
  const summary = details.exception?.description ?? details.text ?? "unknown page exception";
  return new Error(`page expression failed: ${summary}\nExpression: ${expression.slice(0, 240)}`);
}

function diagnosticCounts(diagnostics, outputDir) {
  const consoleErrors = diagnostics.filter((event) => (
    event.method === "Runtime.consoleAPICalled" && event.params?.type === "error"
  ));
  const logErrors = diagnostics.filter((event) => (
    event.method === "Log.entryAdded" && event.params?.entry?.level === "error"
  ));
  const knownOrtWarning = (event) => {
    const text = event.method === "Runtime.consoleAPICalled"
      ? (event.params?.args || []).map((argument) => argument.value || argument.description || "").join(" ")
      : event.params?.entry?.text || "";
    return text.includes("[W:onnxruntime:") && text.includes("VerifyEachNodeIsAssignedToAnEp");
  };
  const faviconMiss = (event) => (
    event.method === "Log.entryAdded"
    && event.params?.entry?.url?.endsWith("/favicon.ico")
    && event.params?.entry?.text?.includes("404")
  );
  const knownRuntimeWarnings = [
    ...consoleErrors.filter(knownOrtWarning),
    ...logErrors.filter(knownOrtWarning),
  ];
  const knownFaviconMisses = logErrors.filter(faviconMiss);
  return {
    path: path.join(outputDir, "browser-events.jsonl"),
    count: diagnostics.length,
    worker_attachments: diagnostics.filter((event) => event.method === "AtlasExperiment.workerAttached").length,
    runtime_exceptions: diagnostics.filter((event) => event.method === "Runtime.exceptionThrown").length,
    console_errors: consoleErrors.length,
    log_errors: logErrors.length,
    known_ort_provider_warnings: knownRuntimeWarnings.length,
    known_favicon_404s: knownFaviconMisses.length,
    unexpected_console_errors: consoleErrors.filter((event) => !knownOrtWarning(event)).length,
    unexpected_log_errors: logErrors.filter((event) => !knownOrtWarning(event) && !faviconMiss(event)).length,
    network_failures: diagnostics.filter((event) => event.method === "Network.loadingFailed").length,
  };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const outputDir = path.resolve(args.output);
  const resultPath = path.resolve(args.result);
  await mkdir(outputDir, { recursive: true });
  const diagnostics = [];
  const startedAt = performance.now();
  const record = (message) => {
    const normalized = message instanceof Error
      ? { method: "AtlasExperiment.driverError", params: { message: message.message, stack: message.stack } }
      : message;
    diagnostics.push({ at_ms: Math.round((performance.now() - startedAt) * 1000) / 1000, ...normalized });
  };
  let connection;
  let browserInfo = null;
  let result = { status: "fail", error: "driver did not start" };
  try {
    const target = await targetFor(Number(args.port), args.tab);
    connection = new CdpConnection(target.webSocketDebuggerUrl, record);
    await connection.connect();
    await Promise.all([
      connection.send("Page.enable"),
      connection.send("Runtime.enable"),
      connection.send("Log.enable"),
      connection.send("Network.enable"),
    ]);
    // `Log.enable` replays entries retained by a long-lived visible tab. The
    // receipt is for this navigation, not for an intentional host restart
    // that happened before the driver attached.
    await connection.send("Log.clear");
    await connection.send("Network.setCacheDisabled", { cacheDisabled: true });
    await connection.send("Target.setAutoAttach", {
      autoAttach: true,
      waitForDebuggerOnStart: false,
      flatten: true,
    });
    browserInfo = await connection.send("Browser.getVersion");

    const evaluate = async (expression, options = {}) => {
      const response = await connection.send("Runtime.evaluate", {
        expression,
        awaitPromise: options.awaitPromise ?? true,
        returnByValue: options.returnByValue ?? true,
        userGesture: options.userGesture ?? false,
      });
      const failure = expressionFailure(response, expression);
      if (failure) throw failure;
      return remoteValue(response.result);
    };

    const navigate = async (url) => {
      const loaded = connection.waitForEvent("Page.loadEventFired", 20_000);
      const response = await connection.send("Page.navigate", { url });
      if (response.errorText) throw new Error(`navigation failed: ${response.errorText}`);
      await loaded;
      await evaluate("document.readyState");
    };

    const waitFor = async (expression, options = {}) => {
      const timeoutMs = options.timeoutMs ?? 20_000;
      const intervalMs = options.intervalMs ?? 50;
      const deadline = performance.now() + timeoutMs;
      let lastError = null;
      while (performance.now() < deadline) {
        try {
          const value = await evaluate(expression);
          if (value) return value;
        } catch (error) {
          if (options.retryOnError !== true) throw error;
          lastError = error;
        }
        await new Promise((resolve) => setTimeout(resolve, intervalMs));
      }
      throw new Error(`waitFor timed out after ${timeoutMs} ms${lastError ? `: ${lastError.message}` : ""}`);
    };

    const screenshot = async (name) => {
      if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*\.png$/.test(name)) throw new Error(`invalid screenshot name: ${name}`);
      const response = await connection.send("Page.captureScreenshot", { format: "png", fromSurface: true });
      const destination = path.join(outputDir, name);
      await writeFile(destination, Buffer.from(response.data, "base64"));
      return destination;
    };

    const setViewport = async (width, height, options = {}) => {
      if (!Number.isInteger(width) || width < 320 || width > 7680
          || !Number.isInteger(height) || height < 320 || height > 4320) {
        throw new Error("viewport width and height must be bounded integers");
      }
      const deviceScaleFactor = options.deviceScaleFactor ?? 1;
      if (!Number.isFinite(deviceScaleFactor) || deviceScaleFactor <= 0 || deviceScaleFactor > 4) {
        throw new Error("viewport deviceScaleFactor must be between 0 and 4");
      }
      await connection.send("Emulation.setDeviceMetricsOverride", {
        width,
        height,
        deviceScaleFactor,
        mobile: options.mobile === true,
      });
      const measured = await evaluate(`(() => ({
        width: innerWidth,
        height: innerHeight,
        deviceScaleFactor: devicePixelRatio,
      }))()`);
      if (measured.width !== width || measured.height !== height) {
        throw new Error(`Chrome measured ${measured.width}x${measured.height} after requesting ${width}x${height}`);
      }
      return measured;
    };

    const elementCenter = async (selector) => evaluate(`(() => {
      const element = document.querySelector(${JSON.stringify(selector)});
      if (!element) throw new Error("element not found: " + ${JSON.stringify(selector)});
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) throw new Error("element has no visible bounds");
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, width: rect.width, height: rect.height };
    })()`);

    const trustedClick = async (selector) => {
      await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!element) throw new Error("element not found");
        window.__atlasExperimentLastTrustedInput = null;
        const record = (event) => { window.__atlasExperimentLastTrustedInput = { type: event.type, isTrusted: event.isTrusted }; };
        element.addEventListener("click", record, { once: true });
      })()`);
      const center = await elementCenter(selector);
      await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: center.x, y: center.y });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mousePressed", x: center.x, y: center.y, button: "left", buttons: 1, clickCount: 1,
      });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x: center.x, y: center.y, button: "left", buttons: 0, clickCount: 1,
      });
      const observed = await evaluate("window.__atlasExperimentLastTrustedInput");
      if (observed?.isTrusted !== true) throw new Error(`trusted click was not observed for ${selector}`);
      return observed;
    };

    const trustedNavigationClick = async (selector) => {
      const token = `atlas-nav-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const expected = await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLAnchorElement)) throw new Error("navigation link not found");
        element.addEventListener("click", (event) => {
          const destination = new URL(element.href);
          destination.searchParams.set("__atlas_trusted_navigation", event.isTrusted
            ? ${JSON.stringify(token)}
            : "untrusted");
          element.href = destination.href;
        }, { once: true, capture: true });
        return element.href;
      })()`);
      const center = await elementCenter(selector);
      const loaded = connection.waitForEvent("Page.loadEventFired", 20_000);
      await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: center.x, y: center.y });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mousePressed", x: center.x, y: center.y, button: "left", buttons: 1, clickCount: 1,
      });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x: center.x, y: center.y, button: "left", buttons: 0, clickCount: 1,
      });
      await loaded;
      const observed = await evaluate(`(() => {
        const locationUrl = new URL(location.href);
        const proof = locationUrl.searchParams.get("__atlas_trusted_navigation");
        locationUrl.searchParams.delete("__atlas_trusted_navigation");
        history.replaceState(history.state, "", locationUrl);
        return { type: "click", isTrusted: proof === ${JSON.stringify(token)}, arrived: location.href };
      })()`);
      if (observed.isTrusted !== true) {
        throw new Error(`trusted navigation click was not observed for ${selector}`);
      }
      return { ...observed, expected };
    };

    const trustedCanvasClick = async (selector, sourceX, sourceY, sourceSize) => {
      if (!Array.isArray(sourceSize) || sourceSize.length !== 2
          || !sourceSize.every((value) => Number.isFinite(value) && value > 0)) {
        throw new Error("canvas sourceSize must contain two positive numbers");
      }
      if (!Number.isFinite(sourceX) || !Number.isFinite(sourceY)
          || sourceX < 0 || sourceY < 0 || sourceX > sourceSize[0] || sourceY > sourceSize[1]) {
        throw new Error("canvas source point is outside sourceSize");
      }
      await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLCanvasElement)) throw new Error("canvas not found");
        window.__atlasExperimentLastTrustedCanvasInput = null;
        const record = (event) => {
          window.__atlasExperimentLastTrustedCanvasInput = {
            type: event.type,
            isTrusted: event.isTrusted,
            detail: event.detail,
            targetMatches: event.target === element,
            target: event.target?.className || event.target?.tagName || null,
          };
        };
        document.addEventListener("click", record, { once: true, capture: true });
      })()`);
      const geometry = await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLCanvasElement)) throw new Error("canvas not found");
        const rect = element.getBoundingClientRect();
        const hits = [];
        const columns = 48;
        const rows = 32;
        for (let column = 0; column <= columns; column += 1) {
          for (let row = 0; row <= rows; row += 1) {
            const x = rect.left + rect.width * column / columns;
            const y = rect.top + rect.height * row / rows;
            if (document.elementsFromPoint(x, y).includes(element)) hits.push([x, y]);
          }
        }
        const style = getComputedStyle(element);
        return {
          rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
          hitRegion: hits.length ? {
            x: Math.min(...hits.map((point) => point[0])),
            y: Math.min(...hits.map((point) => point[1])),
            right: Math.max(...hits.map((point) => point[0])),
            bottom: Math.max(...hits.map((point) => point[1])),
            count: hits.length,
          } : null,
          style: { display: style.display, pointerEvents: style.pointerEvents, objectFit: style.objectFit },
          viewport: { width: innerWidth, height: innerHeight, devicePixelRatio },
        };
      })()`);
      if (!geometry.hitRegion) throw new Error(`canvas has no hit-testable region: ${JSON.stringify(geometry)}`);
      // Source coordinates belong to the canvas, not to the coarse grid of
      // sampled hit-test points above. Using the sampled inset shifted small
      // semantic targets by tens of source pixels and could turn a point click
      // into a whole-region mask. Keep the hit grid only as the visibility
      // gate, then map through the exact canvas rectangle.
      const bounds = geometry.rect;
      const scale = Math.min(bounds.width / sourceSize[0], bounds.height / sourceSize[1]);
      const imageWidth = sourceSize[0] * scale;
      const imageHeight = sourceSize[1] * scale;
      const x = bounds.x + (bounds.width - imageWidth) / 2 + sourceX * scale;
      const y = bounds.y + (bounds.height - imageHeight) / 2 + sourceY * scale;
      const hit = await evaluate(`(() => {
        const target = document.elementFromPoint(${JSON.stringify(x)}, ${JSON.stringify(y)});
        const canvas = document.querySelector(${JSON.stringify(selector)});
        return {
          matches: target === canvas,
          target: target?.className || target?.tagName || null,
          x: ${JSON.stringify(x)},
          y: ${JSON.stringify(y)},
        };
      })()`);
      if (hit?.matches !== true) {
        throw new Error(`canvas point hit ${hit?.target ?? "nothing"} instead of ${selector} at (${x}, ${y}): ${JSON.stringify(geometry)}`);
      }
      await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mousePressed", x, y, button: "left", buttons: 1, clickCount: 1,
      });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x, y, button: "left", buttons: 0, clickCount: 1,
      });
      const observed = await evaluate("window.__atlasExperimentLastTrustedCanvasInput");
      if (observed?.isTrusted !== true || observed?.targetMatches !== true) {
        throw new Error(`trusted canvas click reached ${observed?.target ?? "nothing"} instead of ${selector}`);
      }
      return { ...observed, sourcePoint: [sourceX, sourceY], viewportPoint: [x, y] };
    };

    const trustedFill = async (selector, value) => {
      if (typeof value !== "string") throw new Error("trustedFill value must be a string");
      await trustedClick(selector);
      await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
          throw new Error("text input not found");
        }
        window.__atlasExperimentTrustedTextEvents = [];
        const record = (event) => window.__atlasExperimentTrustedTextEvents.push({
          type: event.type,
          isTrusted: event.isTrusted,
          value: element.value,
        });
        element.addEventListener("beforeinput", record);
        element.addEventListener("input", record);
      })()`);
      await connection.send("Input.dispatchKeyEvent", {
        type: "rawKeyDown", key: "a", code: "KeyA", modifiers: 4, commands: ["selectAll"],
      });
      await connection.send("Input.dispatchKeyEvent", {
        type: "keyUp", key: "a", code: "KeyA", modifiers: 4,
      });
      await connection.send("Input.insertText", { text: value });
      const observed = await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        return { value: element.value, events: window.__atlasExperimentTrustedTextEvents };
      })()`);
      if (observed.value !== value) {
        throw new Error(`trusted text entry produced ${JSON.stringify(observed.value)} instead of ${JSON.stringify(value)} for ${selector}`);
      }
      if (!observed.events?.some((event) => event.isTrusted === true && event.type === "input")) {
        throw new Error(`trusted text input was not observed for ${selector}`);
      }
      return observed;
    };

    const shadowElementCenter = async (hostSelector, selector) => evaluate(`(() => {
      const host = document.querySelector(${JSON.stringify(hostSelector)});
      const element = host?.shadowRoot?.querySelector(${JSON.stringify(selector)});
      if (!element) throw new Error("shadow element not found: " + ${JSON.stringify(`${hostSelector} -> ${selector}`)});
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) throw new Error("shadow element has no visible bounds");
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, width: rect.width, height: rect.height };
    })()`);

    const trustedShadowClick = async (hostSelector, selector) => {
      await evaluate(`(() => {
        const host = document.querySelector(${JSON.stringify(hostSelector)});
        const element = host?.shadowRoot?.querySelector(${JSON.stringify(selector)});
        if (!element) throw new Error("shadow element not found");
        window.__atlasExperimentLastTrustedShadowInput = null;
        const record = (event) => {
          window.__atlasExperimentLastTrustedShadowInput = {
            type: event.type,
            isTrusted: event.isTrusted,
            targetMatches: event.target === element,
          };
        };
        element.addEventListener("click", record, { once: true });
      })()`);
      const center = await shadowElementCenter(hostSelector, selector);
      await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: center.x, y: center.y });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mousePressed", x: center.x, y: center.y, button: "left", buttons: 1, clickCount: 1,
      });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x: center.x, y: center.y, button: "left", buttons: 0, clickCount: 1,
      });
      const observed = await evaluate("window.__atlasExperimentLastTrustedShadowInput");
      if (observed?.isTrusted !== true || observed?.targetMatches !== true) {
        throw new Error(`trusted shadow click was not observed for ${hostSelector} -> ${selector}`);
      }
      return observed;
    };

    const trustedShadowFill = async (hostSelector, selector, value) => {
      if (typeof value !== "string") throw new Error("trustedShadowFill value must be a string");
      await trustedShadowClick(hostSelector, selector);
      await evaluate(`(() => {
        const host = document.querySelector(${JSON.stringify(hostSelector)});
        const element = host?.shadowRoot?.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
          throw new Error("shadow text input not found");
        }
        window.__atlasExperimentTrustedShadowTextEvents = [];
        const record = (event) => window.__atlasExperimentTrustedShadowTextEvents.push({
          type: event.type,
          isTrusted: event.isTrusted,
          value: element.value,
        });
        element.addEventListener("beforeinput", record);
        element.addEventListener("input", record);
      })()`);
      await connection.send("Input.dispatchKeyEvent", {
        type: "rawKeyDown", key: "a", code: "KeyA", modifiers: 4, commands: ["selectAll"],
      });
      await connection.send("Input.dispatchKeyEvent", {
        type: "keyUp", key: "a", code: "KeyA", modifiers: 4,
      });
      await connection.send("Input.insertText", { text: value });
      const observed = await evaluate(`(() => {
        const host = document.querySelector(${JSON.stringify(hostSelector)});
        const element = host?.shadowRoot?.querySelector(${JSON.stringify(selector)});
        return { value: element?.value, events: window.__atlasExperimentTrustedShadowTextEvents };
      })()`);
      if (observed.value !== value) {
        throw new Error(`trusted shadow text entry produced ${JSON.stringify(observed.value)} instead of ${JSON.stringify(value)} for ${hostSelector} -> ${selector}`);
      }
      if (!observed.events?.some((event) => event.isTrusted === true && event.type === "input")) {
        throw new Error(`trusted shadow text input was not observed for ${hostSelector} -> ${selector}`);
      }
      return observed;
    };

    const trustedKey = async (key, code = key) => {
      if (typeof key !== "string" || !key || typeof code !== "string" || !code) {
        throw new Error("trustedKey needs a key and code");
      }
      await evaluate(`(() => {
        window.__atlasExperimentLastTrustedKey = null;
        document.addEventListener("keydown", (event) => {
          window.__atlasExperimentLastTrustedKey = {
            type: event.type,
            isTrusted: event.isTrusted,
            key: event.key,
            code: event.code,
          };
        }, { once: true, capture: true });
      })()`);
      await connection.send("Input.dispatchKeyEvent", { type: "rawKeyDown", key, code });
      await connection.send("Input.dispatchKeyEvent", { type: "keyUp", key, code });
      const observed = await evaluate("window.__atlasExperimentLastTrustedKey");
      if (observed?.isTrusted !== true || observed?.key !== key) {
        throw new Error(`trusted key ${key} was not observed`);
      }
      return observed;
    };

    const trustedRange = async (selector, fraction, steps = 8) => {
      if (!(fraction >= 0 && fraction <= 1)) throw new Error("range fraction must be between 0 and 1");
      const bounds = await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!(element instanceof HTMLInputElement) || element.type !== "range") throw new Error("range input not found");
        const rect = element.getBoundingClientRect();
        window.__atlasExperimentTrustedRangeEvents = [];
        const record = (event) => window.__atlasExperimentTrustedRangeEvents.push({
          type: event.type, isTrusted: event.isTrusted, value: element.value,
        });
        element.addEventListener("input", record);
        element.addEventListener("change", record);
        return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
      })()`);
      const y = bounds.y + bounds.height / 2;
      const startX = bounds.x + 1;
      const endX = bounds.x + Math.max(1, bounds.width - 2) * fraction;
      await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: startX, y });
      await connection.send("Input.dispatchMouseEvent", {
        type: "mousePressed", x: startX, y, button: "left", buttons: 1, clickCount: 1,
      });
      for (let step = 1; step <= steps; step += 1) {
        const x = startX + ((endX - startX) * step) / steps;
        await connection.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "left", buttons: 1 });
      }
      await connection.send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x: endX, y, button: "left", buttons: 0, clickCount: 1,
      });
      const observed = await evaluate(`(() => ({
        value: document.querySelector(${JSON.stringify(selector)}).value,
        events: window.__atlasExperimentTrustedRangeEvents,
      }))()`);
      if (!observed.events?.some((event) => event.isTrusted === true && event.type === "input")) {
        throw new Error(`trusted range input was not observed for ${selector}`);
      }
      return observed;
    };

    await navigate(args.url);
    await connection.send("Page.bringToFront");
    const input = JSON.parse(await readFile(path.resolve(args.input), "utf8"));
    const driverModule = await import(pathToFileURL(path.resolve(args.driver)).href);
    if (typeof driverModule.run !== "function") throw new Error("experiment driver must export async function run(api, input)");
    const api = {
      evaluate,
      navigate,
      screenshot,
      setViewport,
      trustedClick,
      trustedNavigationClick,
      trustedCanvasClick,
      trustedFill,
      trustedShadowClick,
      trustedShadowFill,
      trustedKey,
      trustedRange,
      waitFor,
      sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    };
    const driverPromise = driverModule.run(api, input);
    let timeoutHandle;
    const timeoutPromise = new Promise((_, reject) => {
      timeoutHandle = setTimeout(
        () => reject(new Error(`driver timed out after ${args["timeout-ms"]} ms`)),
        Number(args["timeout-ms"]),
      );
    });
    let driverReceipt;
    try {
      driverReceipt = await Promise.race([driverPromise, timeoutPromise]);
    } finally {
      clearTimeout(timeoutHandle);
    }
    const fatalDiagnostics = diagnostics.filter((event) => [
      "Runtime.exceptionThrown",
      "Inspector.targetCrashed",
      "Target.targetCrashed",
    ].includes(event.method));
    if (fatalDiagnostics.length > 0) {
      throw new Error(`browser emitted ${fatalDiagnostics.length} uncaught exception or target crash event(s)`);
    }
    const counts = diagnosticCounts(diagnostics, outputDir);
    if (counts.unexpected_console_errors > 0 || counts.unexpected_log_errors > 0) {
      throw new Error(`browser emitted ${counts.unexpected_console_errors} unexpected console error(s) and ${counts.unexpected_log_errors} unexpected log error(s)`);
    }
    result = {
      status: "pass",
      duration_ms: Math.round((performance.now() - startedAt) * 1000) / 1000,
      browser: browserInfo,
      driver_receipt: driverReceipt,
      diagnostics: counts,
    };
  } catch (error) {
    record(error);
    result = {
      status: "fail",
      duration_ms: Math.round((performance.now() - startedAt) * 1000) / 1000,
      browser: browserInfo,
      error: error instanceof Error ? error.message : String(error),
      diagnostics: diagnosticCounts(diagnostics, outputDir),
    };
  } finally {
    await writeFile(
      path.join(outputDir, "browser-events.jsonl"),
      diagnostics.map((event) => JSON.stringify(event)).join("\n") + (diagnostics.length ? "\n" : ""),
    );
    await writeFile(resultPath, JSON.stringify(result, null, 2) + "\n");
    connection?.close();
  }
  if (result.status !== "pass") process.exitCode = 1;
}

main().catch(async (error) => {
  process.stderr.write(`${error.stack ?? error}\n`);
  process.exitCode = 1;
});
