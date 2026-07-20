// B. External dependency extraction for geotiff.js
//
// AST-based (not line-regex) extraction of package-qualified import sources:
// static import/export-from, dynamic import(), and require(). Distinguishes
// real runtime references from type-only ones (JSDoc `@import`, inline
// `import("...")` type nodes, `import type`), which are excluded from the
// runtime dependency count per spec and reported separately.

import { Project, Node, SyntaxKind } from "ts-morph";
import path from "node:path";
import fs from "node:fs";
import { builtinModules } from "node:module";

const REPO_ROOT = "/home/hakanbiris/github/geotiff.js";
const OUT_DIR = path.join(import.meta.dirname, "..", "data");

const project = new Project({ tsConfigFilePath: path.join(REPO_ROOT, "tsconfig.json") });
const sourceFiles = project.getSourceFiles();

function relFile(sf) {
  return path.relative(REPO_ROOT, sf.getFilePath());
}

function isPackageSpecifier(spec) {
  return !spec.startsWith(".") && !spec.startsWith("/");
}

function packageIdentity(spec) {
  const parts = spec.split("/");
  if (spec.startsWith("@")) return parts.slice(0, 2).join("/");
  return parts[0];
}

// package -> { runtimeFiles: Set, runtimeBindings: Set, typeOnlyFiles: Set, kinds: Set }
const found = new Map();

function record(pkgSpecifier, sf, kind, bindingNames, typeOnly) {
  if (!isPackageSpecifier(pkgSpecifier)) return;
  const pkg = packageIdentity(pkgSpecifier);
  if (!found.has(pkg)) {
    found.set(pkg, { runtimeFiles: new Set(), runtimeBindings: new Set(), typeOnlyFiles: new Set(), kinds: new Set() });
  }
  const rec = found.get(pkg);
  rec.kinds.add(kind);
  if (typeOnly) {
    rec.typeOnlyFiles.add(relFile(sf));
  } else {
    rec.runtimeFiles.add(relFile(sf));
    for (const b of bindingNames) rec.runtimeBindings.add(b);
  }
}

for (const sf of sourceFiles) {
  // static import declarations: import X, {Y} from 'pkg'; import * as Z from 'pkg';
  for (const imp of sf.getImportDeclarations()) {
    const spec = imp.getModuleSpecifierValue();
    const typeOnly = imp.isTypeOnly();
    const bindings = [];
    const def = imp.getDefaultImport();
    if (def) bindings.push(`default as ${def.getText()}`);
    const ns = imp.getNamespaceImport();
    if (ns) bindings.push(`* as ${ns.getText()}`);
    for (const ni of imp.getNamedImports()) bindings.push(ni.getName());
    record(spec, sf, "import", bindings, typeOnly);
  }

  // re-exports: export { x } from 'pkg'; export * from 'pkg';
  for (const exp of sf.getExportDeclarations()) {
    const spec = exp.getModuleSpecifierValue();
    if (!spec) continue;
    const typeOnly = exp.isTypeOnly();
    const bindings = exp.getNamedExports().map((n) => n.getName());
    record(spec, sf, "export-from", bindings.length ? bindings : ["*"], typeOnly);
  }

  sf.forEachDescendant((node) => {
    // dynamic import('pkg')
    if (Node.isCallExpression(node) && node.getExpression().getKind() === SyntaxKind.ImportKeyword) {
      const arg = node.getArguments()[0];
      if (arg && Node.isStringLiteral(arg)) {
        record(arg.getLiteralValue(), sf, "dynamic-import", ["(dynamic)"], false);
      }
    }
    // require('pkg')
    if (Node.isCallExpression(node) && Node.isIdentifier(node.getExpression()) && node.getExpression().getText() === "require") {
      const arg = node.getArguments()[0];
      if (arg && Node.isStringLiteral(arg)) {
        record(arg.getLiteralValue(), sf, "require", ["(require)"], false);
      }
    }
    // inline JSDoc/TS type-only reference: import("pkg").Foo used as a type
    if (Node.isImportTypeNode(node)) {
      const arg = node.getArgument();
      const literal = Node.isLiteralTypeNode(arg) ? arg.getLiteral() : undefined;
      if (literal && Node.isStringLiteral(literal)) {
        record(literal.getLiteralValue(), sf, "type-reference (import-type)", [], true);
      }
    }
    // `/** @import ... from "pkg" */` JSDoc import tags
    if (node.getKind() === SyntaxKind.JSDocImportTag) {
      const spec = node.getModuleSpecifier?.();
      if (spec && Node.isStringLiteral(spec)) {
        record(spec.getLiteralValue(), sf, "jsdoc-@import", [], true);
      }
    }
  });
}

// self-reference to the project's own package name
const pkgJson = JSON.parse(fs.readFileSync(path.join(REPO_ROOT, "package.json"), "utf8"));
found.delete(pkgJson.name);

const manifestDeps = new Set(Object.keys(pkgJson.dependencies ?? {}));
const manifestPeerDeps = new Set(Object.keys(pkgJson.peerDependencies ?? {}));
const manifestDevDeps = new Set(Object.keys(pkgJson.devDependencies ?? {}));
const manifestRuntime = new Set([...manifestDeps, ...manifestPeerDeps]);

// Node.js built-in modules are never expected to appear in package.json
// dependencies - flagging them as "ghost" would be a false positive.
const NODE_BUILTINS = new Set(builtinModules);

const rows = [];
for (const [pkg, rec] of found) {
  const isRuntimeUsed = rec.runtimeFiles.size > 0;
  const inManifestRuntime = manifestRuntime.has(pkg);
  const inManifestDev = manifestDevDeps.has(pkg);
  let classification;
  if (NODE_BUILTINS.has(pkg)) classification = "node builtin (not an npm package)";
  else if (isRuntimeUsed && inManifestRuntime) classification = "matched";
  else if (isRuntimeUsed && !inManifestRuntime && !inManifestDev) classification = "ghost (imported, not in manifest)";
  else if (isRuntimeUsed && inManifestDev) classification = "ghost (imported at runtime, but only in devDependencies)";
  else if (!isRuntimeUsed && rec.typeOnlyFiles.size > 0) classification = "type-only reference";
  else classification = "unknown";

  rows.push({
    package: pkg,
    classification,
    runtimeFiles: [...rec.runtimeFiles].sort(),
    runtimeBindings: [...rec.runtimeBindings].sort(),
    typeOnlyFiles: [...rec.typeOnlyFiles].sort(),
    kinds: [...rec.kinds].sort(),
    inManifestDependencies: manifestDeps.has(pkg),
    inManifestPeerDependencies: manifestPeerDeps.has(pkg),
    inManifestDevDependencies: inManifestDev,
  });
}

// dead dependencies: declared in manifest dependencies/peerDependencies but never imported at runtime anywhere in src
const deadDeps = [...manifestRuntime].filter((p) => !found.has(p) || found.get(p).runtimeFiles.size === 0).sort();

rows.sort((a, b) => a.package.localeCompare(b.package));

fs.writeFileSync(path.join(OUT_DIR, "external-deps.json"), JSON.stringify({ rows, deadDeps }, null, 2));

console.error(`Found ${rows.length} distinct external package identifiers referenced from src/.`);
console.error("\n--- rows ---");
for (const r of rows) {
  console.error(`${r.package}\t[${r.classification}]\tfiles=${r.runtimeFiles.length}\tbindings=${r.runtimeBindings.join(",")}`);
}
console.error("\n--- dead dependencies (in manifest dependencies/peerDependencies, never imported in src) ---");
console.error(deadDeps);
