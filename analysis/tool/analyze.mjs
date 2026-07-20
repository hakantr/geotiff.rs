// A. Internal dependency analysis for geotiff.js
//
// Pipeline: element inventory -> per-element findReferences -> per-reference
// symbol resolution (override-merge correction) -> innermost-container caller
// assignment -> edge collapsing. Writes ../data/elements.json, edges.json,
// stats.json for downstream DB build / reporting.

import { Project, Node, SyntaxKind } from "ts-morph";
import path from "node:path";
import fs from "node:fs";

const REPO_ROOT = "/home/hakanbiris/github/geotiff.js";
const OUT_DIR = path.join(import.meta.dirname, "..", "data");
fs.mkdirSync(OUT_DIR, { recursive: true });

const project = new Project({
  tsConfigFilePath: path.join(REPO_ROOT, "tsconfig.json"),
});
const sourceFiles = project.getSourceFiles();
console.error(`Loaded ${sourceFiles.length} source files from tsconfig.json`);

function relFile(sf) {
  return path.relative(REPO_ROOT, sf.getFilePath());
}

// ---------------------------------------------------------------------------
// A.1 Element inventory
// ---------------------------------------------------------------------------

let nextId = 1;
const elements = new Map(); // id -> element record
const declKeyToId = new Map(); // "file::start" -> id  (for symbol->element matching)
const containerSpans = []; // {id, start, end, file} (for caller containment search)
const idToNameNode = new Map(); // id -> identifier node to run findReferences() on (absent for constructors)

function nodeKey(node) {
  return `${node.getSourceFile().getFilePath()}::${node.getStart()}`;
}

function registerDeclKey(node, id) {
  declKeyToId.set(nodeKey(node), id);
}

// `declNode` MUST be the node kind that `checker.getSymbolAtLocation(...).getDeclarations()`
// actually returns for a reference to this element (e.g. the FunctionDeclaration/
// MethodDeclaration/ClassDeclaration/VariableDeclaration node) - NOT its name
// identifier - otherwise symbol->element matching silently misses everything.
// `nameNode`, if given, is the identifier findReferences() is run against.
function newElement(kind, name, { declNode, spanNode, nameNode, extraDeclNodes = [] }, extra = {}) {
  const id = `E${String(nextId++).padStart(5, "0")}`;
  const sf = declNode.getSourceFile();
  const rec = {
    id,
    kind,
    name,
    file: relFile(sf),
    line: declNode.getStartLineNumber(),
    ...extra,
  };
  elements.set(id, rec);
  registerDeclKey(declNode, id);
  for (const n of extraDeclNodes) registerDeclKey(n, id);
  const span = spanNode ?? declNode;
  containerSpans.push({
    id,
    file: sf.getFilePath(),
    start: span.getStart(),
    end: span.getEnd(),
  });
  if (nameNode) idToNameNode.set(id, nameNode);
  return id;
}

// enclosing-name chain, for readable qualified names of nested functions
function enclosingChainName(node) {
  const parts = [];
  let cur = node.getParent();
  while (cur) {
    if (Node.isMethodDeclaration(cur) || Node.isFunctionDeclaration(cur) || Node.isConstructorDeclaration(cur)) {
      const cls = cur.getFirstAncestor((a) => Node.isClassDeclaration(a));
      const nm = Node.isConstructorDeclaration(cur) ? "constructor" : cur.getName?.() ?? "<anon>";
      parts.unshift(cls ? `${cls.getName() ?? "<anon>"}.${nm}` : nm);
    } else if (Node.isClassDeclaration(cur)) {
      // stop qualifying past the class boundary handled above
    } else if (Node.isVariableDeclaration(cur) && (Node.isArrowFunction(cur.getInitializer()) || Node.isFunctionExpression(cur.getInitializer()))) {
      parts.unshift(cur.getName());
    }
    cur = cur.getParent();
  }
  return parts;
}

const classElements = new Map(); // ClassDeclaration node -> id
const classConstructorElements = new Map(); // ClassDeclaration node -> id

for (const sf of sourceFiles) {
  sf.forEachDescendant((node) => {
    if (Node.isClassDeclaration(node)) {
      const nameNode = node.getNameNode();
      const name = node.getName() ?? "<anonymous class>";
      const id = newElement("class", name, { declNode: node, spanNode: node, nameNode }, { hasName: !!nameNode });
      classElements.set(node, id);
    } else if (Node.isConstructorDeclaration(node)) {
      const cls = node.getFirstAncestor((a) => Node.isClassDeclaration(a));
      const clsName = cls?.getName() ?? "<anonymous class>";
      const id = newElement("constructor", `${clsName}.constructor`, { declNode: node, spanNode: node });
      classConstructorElements.set(cls, id);
    } else if (Node.isMethodDeclaration(node) || Node.isGetAccessorDeclaration(node) || Node.isSetAccessorDeclaration(node)) {
      const cls = node.getFirstAncestor((a) => Node.isClassDeclaration(a));
      const clsName = cls?.getName() ?? "<anonymous class>";
      const prefix = Node.isGetAccessorDeclaration(node) ? "get:" : Node.isSetAccessorDeclaration(node) ? "set:" : "";
      const methodName = node.getName();
      newElement("method", `${clsName}.${prefix}${methodName}`, { declNode: node, spanNode: node, nameNode: node.getNameNode() });
    } else if (Node.isFunctionDeclaration(node)) {
      const name = node.getName();
      if (!name) return; // anonymous function declarations (rare / invalid) - skip
      const chain = enclosingChainName(node);
      const qname = chain.length ? `${chain.join(">")}>${name}` : name;
      newElement("function", qname, { declNode: node, spanNode: node, nameNode: node.getNameNode() });
    } else if (Node.isVariableDeclaration(node)) {
      const init = node.getInitializer();
      if (init && (Node.isArrowFunction(init) || Node.isFunctionExpression(init))) {
        const name = node.getName();
        const chain = enclosingChainName(node);
        const qname = chain.length ? `${chain.join(">")}>${name}` : name;
        // per spec: map every AST location representing this element to the
        // same id (variable declaration AND the arrow/function expression it holds)
        newElement("function-var", qname, {
          declNode: node,
          spanNode: init,
          nameNode: node.getNameNode(),
          extraDeclNodes: [init],
        });
      }
    }
  });
}

console.error(`Inventoried ${elements.size} elements ` +
  `(classes=${[...elements.values()].filter(e => e.kind === "class").length}, ` +
  `constructors=${[...elements.values()].filter(e => e.kind === "constructor").length}, ` +
  `methods=${[...elements.values()].filter(e => e.kind === "method").length}, ` +
  `functions=${[...elements.values()].filter(e => e.kind === "function").length}, ` +
  `function-vars=${[...elements.values()].filter(e => e.kind === "function-var").length})`);

// ---------------------------------------------------------------------------
// caller lookup: smallest containing registered span
// ---------------------------------------------------------------------------

function findCaller(refNode) {
  const file = refNode.getSourceFile().getFilePath();
  const pos = refNode.getStart();
  let best = null;
  for (const c of containerSpans) {
    if (c.file !== file) continue;
    if (pos >= c.start && pos < c.end) {
      if (!best || (c.end - c.start) < (best.end - best.start)) best = c;
    }
  }
  return best ? best.id : null;
}

// ---------------------------------------------------------------------------
// symbol resolution helper (follows aliases / re-exports)
// ---------------------------------------------------------------------------

function resolveRealSymbol(sym) {
  let real = sym;
  let hops = 0;
  while (real && typeof real.getAliasedSymbol === "function") {
    const aliased = real.getAliasedSymbol();
    if (!aliased) break;
    real = aliased;
    hops++;
    if (hops > 10) break; // guard against pathological cycles
  }
  return real;
}

function declaredElementIdForSymbol(sym) {
  if (!sym) return null;
  const real = resolveRealSymbol(sym);
  const decls = real.getDeclarations();
  for (const d of decls) {
    const id = declKeyToId.get(nodeKey(d));
    if (id) return id;
  }
  return null;
}

// ---------------------------------------------------------------------------
// A.2 / A.3 reference collection + caller assignment
// ---------------------------------------------------------------------------

const edgeMap = new Map(); // "callerId->calleeId" -> {callerId, calleeId, occurrences, fallback}
const stats = {
  totalRawReferenceEntries: 0,
  totalDefinitionEntries: 0,
  attributedToOtherElement: 0, // override-merge correction discarding
  excludedOutsideCatalog: 0, // resolves fine, but not one of our tracked elements
  unresolvedFallback: 0, // symbol could not be resolved at all -> counted to processing element
  orphanNoCaller: 0, // reference not contained in any registered element (import lines, top-level)
  selfReferenceDiscarded: 0,
  constructorRedirects: 0,
  superConstructorEdges: 0,
  finalEdgeCount: 0,
};

// raw (pre-correction) merged-group size per element - used for validation 5b
const rawGroupSize = new Map();

function addEdge(callerId, calleeId, fallback = false) {
  if (!callerId) {
    stats.orphanNoCaller++;
    return;
  }
  if (callerId === calleeId) {
    stats.selfReferenceDiscarded++;
    return;
  }
  const key = `${callerId}->${calleeId}`;
  let e = edgeMap.get(key);
  if (!e) {
    e = { callerId, calleeId, occurrences: 0, fallback: false };
    edgeMap.set(key, e);
  }
  e.occurrences++;
  if (fallback) e.fallback = true;
}

function isNewExpressionCallee(refNode) {
  const parent = refNode.getParent();
  return Node.isNewExpression(parent) && parent.getExpression() === refNode;
}

console.error(`idToNameNode has ${idToNameNode.size} findable elements ` +
  `(of ${elements.size} total; constructors have no name identifier and are handled separately)`);

for (const [id, nameNode] of idToNameNode) {
  const el = elements.get(id);
  let referencedSymbols;
  try {
    referencedSymbols = nameNode.findReferences();
  } catch (err) {
    console.error(`findReferences failed for ${id} (${el.name}): ${err.message}`);
    continue;
  }

  let groupNonDefCount = 0;

  for (const rs of referencedSymbols) {
    for (const entry of rs.getReferences()) {
      const refNode = entry.getNode();
      if (entry.isDefinition()) {
        stats.totalDefinitionEntries++;
        continue;
      }
      stats.totalRawReferenceEntries++;
      groupNonDefCount++;

      const sym = refNode.getSymbol();
      let calleeId = null;
      let fallback = false;

      if (!sym) {
        // symbol could not be resolved at all: count toward the element being processed
        calleeId = id;
        fallback = true;
        stats.unresolvedFallback++;
      } else {
        const resolvedId = declaredElementIdForSymbol(sym);
        if (resolvedId === null) {
          stats.excludedOutsideCatalog++;
          continue; // resolves outside our catalog (external lib, param, etc.) - not an edge
        }
        if (resolvedId !== id) {
          // override-merge correction: this raw entry actually belongs to a
          // different declaration in the hierarchy; it will be (or was)
          // correctly attributed during that element's own pass.
          stats.attributedToOtherElement++;
          continue;
        }
        calleeId = resolvedId;
      }

      // constructor redirect: `new ClassName(...)` should attribute to the
      // explicit constructor element, if the class defines one.
      if (el.kind === "class" && isNewExpressionCallee(refNode)) {
        const clsNode = [...classElements.entries()].find(([, cid]) => cid === id)?.[0];
        const ctorId = clsNode ? classConstructorElements.get(clsNode) : null;
        if (ctorId) {
          calleeId = ctorId;
          stats.constructorRedirects++;
        }
      }

      const callerId = findCaller(refNode);
      addEdge(callerId, calleeId, fallback);
    }
  }

  rawGroupSize.set(id, groupNonDefCount);
}

// ---------------------------------------------------------------------------
// explicit `super(...)` constructor calls (not reachable via findReferences,
// since `super` is a keyword, not a name-resolvable identifier)
// ---------------------------------------------------------------------------

for (const sf of sourceFiles) {
  sf.forEachDescendant((node) => {
    if (!Node.isCallExpression(node)) return;
    const expr = node.getExpression();
    if (expr.getKind() !== SyntaxKind.SuperKeyword) return;

    const ctorNode = node.getFirstAncestor((a) => Node.isConstructorDeclaration(a));
    if (!ctorNode) return;
    const callerId = declKeyToId.get(nodeKey(ctorNode));
    const subclass = ctorNode.getFirstAncestor((a) => Node.isClassDeclaration(a));
    const baseExpr = subclass?.getExtends();
    if (!baseExpr) return;
    const baseSym = baseExpr.getExpression().getSymbol();
    const baseId = declaredElementIdForSymbol(baseSym);
    if (!baseId) return; // base class not in our catalog (e.g. `extends Error`)
    const baseClassNode = [...classElements.entries()].find(([, cid]) => cid === baseId)?.[0];
    const baseCtorId = baseClassNode ? classConstructorElements.get(baseClassNode) : null;
    if (!baseCtorId) return; // base class has no explicit constructor - nothing executes there

    addEdge(callerId, baseCtorId, false);
    stats.superConstructorEdges++;
  });
}

stats.finalEdgeCount = edgeMap.size;

// ---------------------------------------------------------------------------
// persist
// ---------------------------------------------------------------------------

const elementsOut = [...elements.values()].map((e) => ({
  ...e,
  rawGroupSize: rawGroupSize.get(e.id) ?? null,
}));
const edgesOut = [...edgeMap.values()];

fs.writeFileSync(path.join(OUT_DIR, "elements.json"), JSON.stringify(elementsOut, null, 2));
fs.writeFileSync(path.join(OUT_DIR, "edges.json"), JSON.stringify(edgesOut, null, 2));
fs.writeFileSync(path.join(OUT_DIR, "stats.json"), JSON.stringify(stats, null, 2));

console.error("\n=== stats ===");
console.error(stats);
console.error(`\nWrote ${elementsOut.length} elements, ${edgesOut.length} edges to ${OUT_DIR}`);
