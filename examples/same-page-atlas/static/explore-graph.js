// A local lens over canonical subjects and relationships. It creates neither
// model facts nor a second layout. Unknown detail stays unknown.
export function focusGraph(page, rootId) {
  const nodes = new Map((page.nodes || []).map(node => [node.id, node]));
  if (!nodes.has(rootId)) return null;
  const inside = new Set([rootId]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const node of nodes.values()) {
      if (inside.has(node.parent) && !inside.has(node.id)) { inside.add(node.id); changed = true; }
    }
  }
  const visible = new Set(inside);
  for (const edge of page.edges || []) {
    if (inside.has(edge.from) && nodes.has(edge.to)) visible.add(edge.to);
    if (inside.has(edge.to) && nodes.has(edge.from)) visible.add(edge.from);
  }
  // Ancestor frames are context in the breadcrumb, not huge empty boxes in
  // a detail view. Their other descendants must not enter through a back edge.
  let parent = nodes.get(rootId).parent;
  const seen = new Set();
  while (parent && !seen.has(parent)) { seen.add(parent); visible.delete(parent); parent = nodes.get(parent)?.parent; }
  return { visible, inside };
}

export function linkedDetailViews(page, rootId) {
  return (page.explanations || []).filter(flow => {
    const { about = [], scene_ids = [] } = flow.definition;
    // A whole-page tour mentioning every card is not detail for every card.
    return about.includes(rootId) && (scene_ids.length > 0 || about.length === 1);
  });
}
