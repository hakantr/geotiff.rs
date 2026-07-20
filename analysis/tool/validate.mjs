// A.5 Validation: override-clustering signal check (5b) + integrity (5c).
// (5a - cross-check against grep - was done manually, see report.)

import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import fs from "node:fs";

const DATA_DIR = path.join(import.meta.dirname, "..", "data");
const db = new DatabaseSync(path.join(DATA_DIR, "analysis.sqlite"), { readOnly: true });

const elements = db.prepare("SELECT * FROM elements").all();

// group methods/accessors by their bare member name (part after the last '.')
// across different classes - candidate override sets
const byBareName = new Map();
for (const el of elements) {
  if (el.kind !== "method") continue;
  const bare = el.name.split(".").pop();
  if (!byBareName.has(bare)) byBareName.set(bare, []);
  byBareName.get(bare).push(el);
}

const dependentCountStmt = db.prepare(
  "SELECT COUNT(DISTINCT caller_id) c FROM edges WHERE callee_id = ?"
);

const overrideGroups = [];
for (const [bare, els] of byBareName) {
  if (els.length < 2) continue; // not an override candidate (only one class has this member name)
  const rows = els.map((el) => ({
    name: el.name,
    file: el.file,
    line: el.line,
    rawGroupSize: el.raw_group_size,
    correctedDependentCount: dependentCountStmt.get(el.id).c,
  }));
  const rawValues = new Set(rows.map((r) => r.rawGroupSize));
  const correctedValues = new Set(rows.map((r) => r.correctedDependentCount));
  overrideGroups.push({
    bareName: bare,
    members: rows,
    rawAllIdentical: rawValues.size === 1,
    correctedAllIdentical: correctedValues.size === 1 && [...correctedValues][0] > 0,
  });
}

overrideGroups.sort((a, b) => b.members.length - a.members.length);

const suspicious = overrideGroups.filter((g) => g.correctedAllIdentical && g.members.every(m => m.correctedDependentCount > 0));

fs.writeFileSync(path.join(DATA_DIR, "validation-override-groups.json"), JSON.stringify(overrideGroups, null, 2));

console.error(`Found ${overrideGroups.length} candidate override groups (method name shared by >=2 classes).`);
console.error(`Groups where raw (pre-correction) merged count was identical across all members: ${overrideGroups.filter(g => g.rawAllIdentical).length}`);
console.error(`Groups where CORRECTED count is still identical (>0) across all members (potential correction failure): ${suspicious.length}`);
if (suspicious.length) {
  console.error(JSON.stringify(suspicious, null, 2));
}

console.error("\nSample groups (top 8 by member count):");
for (const g of overrideGroups.slice(0, 8)) {
  console.error(`\n  ${g.bareName}: raw identical=${g.rawAllIdentical}`);
  for (const m of g.members) {
    console.error(`    ${m.name.padEnd(35)} raw=${m.rawGroupSize}\tcorrected=${m.correctedDependentCount}\t[${m.file}:${m.line}]`);
  }
}

db.close();
