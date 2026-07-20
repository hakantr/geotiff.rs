// A.4 Store elements/edges in a queryable SQLite DB and run the two
// canonical queries: most-depended-upon, and leaf-but-foundational.

import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import fs from "node:fs";

const DATA_DIR = path.join(import.meta.dirname, "..", "data");
const DB_PATH = path.join(DATA_DIR, "analysis.sqlite");

fs.rmSync(DB_PATH, { force: true });
const db = new DatabaseSync(DB_PATH);

db.exec(`
  CREATE TABLE elements (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    raw_group_size INTEGER
  );
  CREATE TABLE edges (
    caller_id TEXT NOT NULL,
    callee_id TEXT NOT NULL,
    occurrences INTEGER NOT NULL,
    fallback INTEGER NOT NULL,
    PRIMARY KEY (caller_id, callee_id),
    FOREIGN KEY (caller_id) REFERENCES elements(id),
    FOREIGN KEY (callee_id) REFERENCES elements(id)
  );
  CREATE INDEX idx_edges_callee ON edges(callee_id);
  CREATE INDEX idx_edges_caller ON edges(caller_id);
`);

const elements = JSON.parse(fs.readFileSync(path.join(DATA_DIR, "elements.json"), "utf8"));
const edges = JSON.parse(fs.readFileSync(path.join(DATA_DIR, "edges.json"), "utf8"));

const insertElement = db.prepare(
  "INSERT INTO elements (id, kind, name, file, line, raw_group_size) VALUES (?, ?, ?, ?, ?, ?)"
);
for (const e of elements) {
  insertElement.run(e.id, e.kind, e.name, e.file, e.line, e.rawGroupSize);
}

const insertEdge = db.prepare(
  "INSERT INTO edges (caller_id, callee_id, occurrences, fallback) VALUES (?, ?, ?, ?)"
);
for (const e of edges) {
  insertEdge.run(e.callerId, e.calleeId, e.occurrences, e.fallback ? 1 : 0);
}

console.error(`Loaded ${elements.length} elements, ${edges.length} edges into ${DB_PATH}`);

// -----------------------------------------------------------------------
// Query 1: most depended-upon elements (dependent count = distinct callers)
// -----------------------------------------------------------------------
const topDependedUpon = db.prepare(`
  SELECT el.id, el.kind, el.name, el.file, el.line, COUNT(DISTINCT ed.caller_id) AS dependent_count
  FROM elements el
  JOIN edges ed ON ed.callee_id = el.id
  GROUP BY el.id
  ORDER BY dependent_count DESC, el.name ASC
  LIMIT 25
`).all();

// -----------------------------------------------------------------------
// Query 2: "leaf but foundational" - no outgoing edges among tracked
// elements, yet depended upon by others. Independence is only relative to
// tracked elements (does not account for calls into external APIs/builtins).
// -----------------------------------------------------------------------
const leafButFoundational = db.prepare(`
  SELECT el.id, el.kind, el.name, el.file, el.line, COUNT(DISTINCT ed.caller_id) AS dependent_count
  FROM elements el
  JOIN edges ed ON ed.callee_id = el.id
  WHERE el.id NOT IN (SELECT DISTINCT caller_id FROM edges)
  GROUP BY el.id
  ORDER BY dependent_count DESC, el.name ASC
  LIMIT 25
`).all();

// integrity checks used by the validation step
const dupCallerCalleePairs = db.prepare(`
  SELECT caller_id, callee_id, COUNT(*) c FROM edges GROUP BY caller_id, callee_id HAVING c > 1
`).all();
const selfEdges = db.prepare(`SELECT * FROM edges WHERE caller_id = callee_id`).all();

fs.writeFileSync(path.join(DATA_DIR, "query-top-depended-upon.json"), JSON.stringify(topDependedUpon, null, 2));
fs.writeFileSync(path.join(DATA_DIR, "query-leaf-but-foundational.json"), JSON.stringify(leafButFoundational, null, 2));
fs.writeFileSync(path.join(DATA_DIR, "integrity-checks.json"), JSON.stringify({
  duplicateCallerCalleePairs: dupCallerCalleePairs.length,
  selfEdges: selfEdges.length,
}, null, 2));

console.error("\nTop 10 most depended-upon:");
for (const r of topDependedUpon.slice(0, 10)) {
  console.error(`  ${r.dependent_count}\t${r.name} (${r.kind}) [${r.file}:${r.line}]`);
}
console.error("\nTop 10 leaf-but-foundational:");
for (const r of leafButFoundational.slice(0, 10)) {
  console.error(`  ${r.dependent_count}\t${r.name} (${r.kind}) [${r.file}:${r.line}]`);
}
console.error("\nIntegrity: duplicate caller/callee pairs =", dupCallerCalleePairs.length, "; self edges =", selfEdges.length);

db.close();
