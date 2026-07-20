// Derives a leaf-first port order from the internal dependency graph
// (analysis.sqlite): Kahn's-algorithm topological waves over the
// caller->callee edges, ported callee-before-caller. Elements with zero
// outgoing edges among tracked elements are Wave 1 (true leaves - either no
// real dependencies, or dependencies only on external APIs/builtins outside
// our catalog). Cycles (mutual recursion / call-back-up patterns) are
// reported separately as groups that must be ported together.

import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import fs from "node:fs";

const DATA_DIR = path.join(import.meta.dirname, "..", "data");
const db = new DatabaseSync(path.join(DATA_DIR, "analysis.sqlite"), { readOnly: true });

const elements = db.prepare("SELECT * FROM elements").all();
const edges = db.prepare("SELECT DISTINCT caller_id, callee_id FROM edges").all();

const outAdj = new Map(); // id -> Set(callee ids it depends on)
const inAdj = new Map(); // id -> Set(caller ids that depend on it)
for (const el of elements) {
  outAdj.set(el.id, new Set());
  inAdj.set(el.id, new Set());
}
for (const e of edges) {
  outAdj.get(e.caller_id).add(e.callee_id);
  inAdj.get(e.callee_id).add(e.caller_id);
}

const remaining = new Set(elements.map((e) => e.id));
const waves = [];
const elementById = new Map(elements.map((e) => [e.id, e]));

while (remaining.size > 0) {
  const wave = [...remaining].filter((id) => {
    for (const dep of outAdj.get(id)) {
      if (remaining.has(dep)) return false;
    }
    return true;
  });
  if (wave.length === 0) break; // remaining nodes are all in cycles
  for (const id of wave) remaining.delete(id);
  waves.push(wave);
}

// whatever's left is involved in a cycle - group into weakly-connected
// clusters among the remaining nodes for reporting
const cycleNodes = [...remaining];
const cycleGroups = [];
const visited = new Set();
for (const start of cycleNodes) {
  if (visited.has(start)) continue;
  const group = [];
  const stack = [start];
  while (stack.length) {
    const n = stack.pop();
    if (visited.has(n)) continue;
    visited.add(n);
    group.push(n);
    for (const nb of outAdj.get(n)) if (cycleNodes.includes(nb) && !visited.has(nb)) stack.push(nb);
    for (const nb of inAdj.get(n)) if (cycleNodes.includes(nb) && !visited.has(nb)) stack.push(nb);
  }
  cycleGroups.push(group);
}

function fmt(id) {
  const e = elementById.get(id);
  return { id, name: e.name, kind: e.kind, file: e.file, line: e.line, dependents: inAdj.get(id).size, dependsOn: outAdj.get(id).size };
}

const out = {
  totalElements: elements.length,
  totalEdges: edges.length,
  waveCount: waves.length,
  waves: waves.map((w, i) => ({
    wave: i + 1,
    count: w.length,
    elements: w.map(fmt).sort((a, b) => b.dependents - a.dependents),
  })),
  cycleGroups: cycleGroups.map((g) => g.map(fmt)),
};

fs.writeFileSync(path.join(DATA_DIR, "port-order.json"), JSON.stringify(out, null, 2));

console.error(`${waves.length} waves computed over ${elements.length} elements.`);
console.error(`Wave sizes: ${waves.map((w) => w.length).join(", ")}`);
console.error(`Nodes left in cycles (ported as a group): ${cycleNodes.length} across ${cycleGroups.length} group(s)`);
console.error("\nWave 1 (true leaves, first to port) - top 20 by dependents:");
for (const e of out.waves[0].elements.slice(0, 20)) {
  console.error(`  deps=${e.dependents}\t${e.name} (${e.kind}) [${e.file}:${e.line}]`);
}
console.error(`\n...and ${out.waves[0].elements.length - 20} more in wave 1.`);

if (cycleGroups.length) {
  console.error("\nCycle groups:");
  for (const g of cycleGroups) {
    console.error("  -", g.map((id) => elementById.get(id).name).join(" <-> "));
  }
}

db.close();
