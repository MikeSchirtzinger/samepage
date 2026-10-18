// One vocabulary for the marks people put on things and the words for source
// trust, shared by every capability so a glyph means one thing on the page.
export const MARKS = Object.freeze([
  Object.freeze({ id: "question", glyph: "?", title: "I have a question" }),
  Object.freeze({ id: "important", glyph: "!", title: "This matters" }),
  Object.freeze({ id: "agree", glyph: "✓", title: "I agree" }),
  Object.freeze({ id: "disagree", glyph: "✗", title: "I think this is wrong" }),
]);
export const MARK_GLYPH = Object.freeze(Object.fromEntries(MARKS.map((mark) => [mark.id, mark.glyph])));
export const MARK_WORDS = Object.freeze(Object.fromEntries(MARKS.map((mark) => [mark.id, mark.title])));
// Source trust is said in words only; no glyph that could be read as a mark.
export const TIER_WORDS = Object.freeze({ verified: "verified", drifted: "drifted", claimed: "claimed", "gate-met": "gate met", "gate-unmet": "gate not met" });
export function tierWords(tier) {
  const word = TIER_WORDS[tier?.tier] || String(tier?.tier || "").replace(/-/g, " ");
  const chip = String(tier?.chip || "").trim();
  const rest = chip.toLowerCase().startsWith(word) ? chip.slice(word.length).trim() : chip.split(/\s+/).slice(1).join(" ");
  return rest ? `${word} ${rest}` : word;
}
// The card control that opens Review is a speech bubble, never a question mark.
export function speechIcon() {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 16 16"); svg.setAttribute("width", "14"); svg.setAttribute("height", "14"); svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", "M2 3.5A1.5 1.5 0 0 1 3.5 2h9A1.5 1.5 0 0 1 14 3.5v6A1.5 1.5 0 0 1 12.5 11H7l-3 3v-3h-.5A1.5 1.5 0 0 1 2 9.5z");
  path.setAttribute("fill", "none"); path.setAttribute("stroke", "currentColor"); path.setAttribute("stroke-width", "1.4"); path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}
