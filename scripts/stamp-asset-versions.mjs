#!/usr/bin/env node
// Cache-busts the site's own CSS/JS: rewrites every reference like
//   href="/style.css"  or  src="/api-utils.js?v=1a2b3c4d"
// in apps/web/*.html to carry ?v=<first 8 hex of the file's SHA-256>.
// A changed file gets a new URL, so no browser or CDN cache can keep
// serving the old copy (Caddy already sends Cache-Control: no-cache, but
// that alone wasn't enough in practice — a stale style.css needed a hard
// refresh). An unchanged file keeps its URL, so it stays cached.
//
// Runs automatically in .github/workflows/deploy.yml before every deploy
// and commits the result, so nobody has to remember it. Safe to run by
// hand any time:
//   node scripts/stamp-asset-versions.mjs          # rewrite in place
//   node scripts/stamp-asset-versions.mjs --check  # exit 1 if anything's stale
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, writeFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const webRoot = join(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'web');
const checkOnly = process.argv.includes('--check');

const hashCache = new Map();
function versionFor(assetPath) {
  if (!hashCache.has(assetPath)) {
    const file = join(webRoot, assetPath);
    hashCache.set(
      assetPath,
      existsSync(file) ? createHash('sha256').update(readFileSync(file)).digest('hex').slice(0, 8) : null,
    );
  }
  return hashCache.get(assetPath);
}

// Root-relative .css/.js references in href= or src=, with or without an
// existing ?v=. Anything else in the query string is left alone (no match).
const ASSET_REF = /((?:href|src)=")(\/[\w./-]+\.(?:css|js))(?:\?v=[0-9a-f]*)?(")/g;

const changed = [];
for (const name of readdirSync(webRoot).filter((f) => f.endsWith('.html'))) {
  const file = join(webRoot, name);
  const before = readFileSync(file, 'utf8');
  const after = before.replace(ASSET_REF, (match, attr, assetPath, quote) => {
    const version = versionFor(assetPath.slice(1));
    return version ? `${attr}${assetPath}?v=${version}${quote}` : match;
  });
  if (after !== before) {
    changed.push(name);
    if (!checkOnly) writeFileSync(file, after);
  }
}

if (changed.length === 0) {
  console.log('Asset versions are up to date.');
} else if (checkOnly) {
  console.error(`Stale asset versions in: ${changed.join(', ')}\nRun: node scripts/stamp-asset-versions.mjs`);
  process.exit(1);
} else {
  console.log(`Stamped asset versions in: ${changed.join(', ')}`);
}
