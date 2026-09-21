// Repository text is always host-resolved. Viewing files never edits the map.
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const parent = path => path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
const join = (directory, name) => directory ? `${directory}/${name}` : name;
let current;
const restoreFocus = new WeakSet();

export function sourceReferences(node, claims = []) {
  const references = [node, ...claims.filter(claim => claim.about === node.id && !claim.withdrawn)];
  return references.filter((ref, index) => ref.path && references.findIndex(other =>
    other.path === ref.path && (other.lines || "") === (ref.lines || "")) === index);
}

export function closeSourceViewer(returnFocus = false) {
  if (!current) return;
  if (returnFocus) restoreFocus.add(current);
  current.hidePopover();
}

export function openSourceViewer(node, references, origin = document.activeElement) {
  closeSourceViewer();
  const panel = el("aside", "source-viewer");
  panel.id = "source-viewer";
  panel.popover = "auto";
  panel.setAttribute("role", "dialog");
  panel.setAttribute("aria-label", `Code for ${node.label}`);
  const header = el("header", "source-viewer-header");
  const title = el("h2", null, node.label);
  const close = el("button", "quiet-button", "Close");
  close.type = "button";
  close.setAttribute("aria-label", "Close code viewer");
  header.append(title, close);
  const context = el("p", "source-viewer-context", "Current working tree · read from the host.");
  const choices = el("div", "source-viewer-choices");
  const fileLabel = el("label", null, "File");
  const files = el("select");
  files.setAttribute("aria-label", "Source file");
  fileLabel.append(files);
  choices.append(fileLabel);
  const details = el("details", "source-viewer-files");
  details.append(el("summary", null, "Browse files"));
  const directoryLabel = el("div", "source-directory");
  const listing = el("div", "source-file-list");
  listing.setAttribute("aria-label", "Repository files");
  details.append(directoryLabel, listing);
  const location = el("p", "source-location");
  const notice = el("p", "source-notice");
  notice.setAttribute("role", "status");
  const pre = el("pre", "source-code");
  pre.tabIndex = 0;
  pre.setAttribute("aria-label", "Source code");
  const footer = el("div", "source-viewer-pages");
  const previous = el("button", "quiet-button", "Previous lines");
  const whole = el("button", "quiet-button", "From start");
  const next = el("button", "quiet-button", "Next lines");
  for (const button of [previous, whole, next]) button.type = "button";
  footer.append(previous, whole, next);
  panel.append(header, context, choices, details, location, notice, pre, footer);
  let request = 0, directoryRequest = 0, path = "", directory = "", excerpt;
  const options = [];
  const addOption = (reference, label) => {
    let index = options.findIndex(option => option.path === reference.path && option.lines === (reference.lines || ""));
    if (index >= 0) return index;
    index = options.length;
    options.push({path: reference.path, lines: reference.lines || ""});
    const option = el("option", null, label);
    option.value = String(index);
    files.append(option);
    return index;
  };
  for (const ref of references) addOption(ref, `Linked: ${ref.path}${ref.lines ? `:${ref.lines}` : ""}`);
  async function read(url, signal) {
    const response = await fetch(url, {cache: "no-store", signal, headers: {accept: "application/json"}});
    const payload = await response.json();
    if (!response.ok || !payload.ok) throw new Error(payload.error || `Repository read failed (${response.status})`);
    return payload;
  }
  const abort = new AbortController();
  async function load(reference, discover = false) {
    const token = ++request;
    path = reference.path;
    directory = parent(path);
    excerpt = null;
    files.value = String(addOption(reference, path));
    location.textContent = `${path}${reference.lines ? `:${reference.lines}` : ""}`;
    notice.textContent = "Reading source…";
    notice.classList.remove("source-error");
    pre.replaceChildren();
    for (const button of [previous, whole, next]) button.disabled = true;
    try {
      const payload = await read(`/atlas/source?${new URLSearchParams({path, lines: reference.lines || ""})}`, abort.signal);
      if (token !== request || current !== panel) return;
      for (const entry of payload.entry_points || []) addOption({path: entry}, `Entry point: ${entry}`);
      if (discover && payload.entry_points?.length) {
        return load({path: payload.entry_points[0]}, false);
      }
      excerpt = payload;
      location.textContent = `${payload.path}:${payload.lines} · ${payload.total_lines} lines`;
      const clippedLine = payload.truncated && !payload.text.includes("\n") && Array.from(payload.text).length >= 24_000;
      notice.textContent = clippedLine ? "This line exceeds the excerpt limit and is clipped." : payload.truncated ? "More lines available. Next lines continues the file." : "Read from the host just now.";
      if (payload.entry_points_error) notice.textContent += ` Entry points unavailable: ${payload.entry_points_error}`;
      const numbers = payload.text.split("\n").map((_, index) => payload.first_line + index);
      const gutter = el("span", "source-line-numbers", numbers.join("\n"));
      gutter.setAttribute("aria-hidden", "true");
      // One verbatim code node preserves whitespace, selection and copying.
      // Line numbers are a separate, nonselectable gutter in the same scroller.
      pre.append(gutter, el("code", "source-code-text", payload.text));
      pre.scrollTop = 0;
      pre.scrollLeft = 0;
      previous.disabled = payload.first_line <= 1;
      whole.disabled = payload.first_line <= 1;
      next.disabled = endLine() >= payload.total_lines;
      if (details.open) browse(directory);
    } catch (error) {
      if (token !== request || current !== panel || error.name === "AbortError") return;
      notice.textContent = `Could not read source: ${error.message}`;
      notice.classList.add("source-error");
    }
  }
  const endLine = () => Number(String(excerpt.lines).split("-").at(-1));
  async function browse(folder) {
    const token = ++directoryRequest;
    directoryLabel.replaceChildren(el("span", null, folder || "Repository root"));
    if (folder) {
      const up = el("button", "quiet-button", "Parent folder");
      up.type = "button";
      up.addEventListener("click", () => browse(parent(folder)));
      directoryLabel.append(up);
    }
    listing.textContent = "Reading files…";
    try {
      const payload = await read(`/atlas/files?${new URLSearchParams({path: folder})}`, abort.signal);
      if (token !== directoryRequest || current !== panel) return;
      listing.replaceChildren();
      for (const entry of payload.entries) {
        const button = el("button", "source-file", `${entry.name}${entry.is_dir ? "/" : ""}`);
        button.type = "button";
        button.addEventListener("click", () => entry.is_dir ? browse(join(folder, entry.name)) : load({path: join(folder, entry.name)}));
        listing.append(button);
      }
      if (!payload.entries.length) listing.textContent = "No readable entries.";
      if (payload.entries.length >= 240) listing.append(el("p", null, "Directory listing limited to 240 entries."));
    } catch (error) {
      if (token === directoryRequest && current === panel && error.name !== "AbortError") listing.textContent = `Could not list files: ${error.message}`;
    }
  }
  previous.addEventListener("click", () => load({path, lines: `${Math.max(1, excerpt.first_line - 200)}-${excerpt.first_line - 1}`}));
  next.addEventListener("click", () => load({path, lines: `${endLine() + 1}-${Math.min(excerpt.total_lines, endLine() + 200)}`}));
  whole.addEventListener("click", () => load({path, lines: "1-200"}));
  files.addEventListener("change", () => load(options[Number(files.value)]));
  details.addEventListener("toggle", () => { if (details.open) browse(directory); });
  close.addEventListener("click", () => closeSourceViewer(true));
  panel.addEventListener("keydown", event => {
    event.stopPropagation();
    if (event.key === "Escape") { event.preventDefault(); closeSourceViewer(true); }
  });
  panel.addEventListener("toggle", event => {
    if (event.newState !== "closed") return;
    abort.abort();
    if (current === panel) current = null;
    panel.remove();
    if (restoreFocus.has(panel) && origin?.isConnected) origin.focus({preventScroll: true});
  });
  document.body.append(panel);
  current = panel;
  panel.showPopover();
  close.focus({preventScroll: true});
  if (references.length) load(references[0], true);
  else { notice.textContent = "No source linked to this object yet."; choices.hidden = details.hidden = footer.hidden = true; }
  return panel;
}
