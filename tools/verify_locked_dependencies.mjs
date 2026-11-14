/** Read-only comparison of the installed physical layout with package-lock v2/v3. */
import { readFile, readdir, realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const isObject = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
const normalized = (value) => process.platform === 'win32' ? value.toLowerCase() : value;

export async function verifyLockedDependencies(projectDirectory) {
  const root = path.resolve(projectDirectory);
  const lock = JSON.parse(await readFile(path.join(root, 'package-lock.json'), 'utf8'));
  if (![2, 3].includes(lock.lockfileVersion) || !isObject(lock.packages)) {
    throw new Error('Expected a package-lock v2/v3 packages inventory.');
  }
  const expected = new Map();
  for (const [location, entry] of Object.entries(lock.packages)) {
    if (location === '') continue;
    const validLocation = /^(?:node_modules\/(?:@[^/]+\/)?[^/]+)(?:\/node_modules\/(?:@[^/]+\/)?[^/]+)*$/.test(location);
    if (!validLocation || location.includes('\\') || location.split('/').some((part) => part === '.' || part === '..')) {
      throw new Error(`Unsupported lockfile package location: ${location}`);
    }
    if (!isObject(entry) || typeof entry.version !== 'string' || entry.link) {
      throw new Error(`Expected a locked package version at ${location}; workspace links require separate verification.`);
    }
    expected.set(location, entry);
  }

  const installed = new Map();
  const problems = [];
  const ancestors = new Set();
  async function isDirectory(directory) {
    try {
      if (!(await stat(directory)).isDirectory()) throw new Error(`Not a directory: ${directory}`);
      return true;
    } catch (error) {
      if (error.code === 'ENOENT') return false;
      throw error;
    }
  }
  async function visitPackage(directory, location) {
    if (!(await isDirectory(directory))) {
      problems.push({ kind: 'unreadable-package', location });
      return;
    }
    // Read the real package manifest, not node_modules/.package-lock.json.
    // Logical locations remain stable when node_modules itself is a junction.
    const manifest = JSON.parse(await readFile(path.join(directory, 'package.json'), 'utf8'));
    if (!isObject(manifest) || typeof manifest.version !== 'string') {
      throw new Error(`Missing installed package version: ${location}`);
    }
    installed.set(location, manifest.version);
    await visitModules(path.join(directory, 'node_modules'), `${location}/node_modules`);
  }
  async function visitModules(directory, location) {
    if (!(await isDirectory(directory))) return;
    const physical = normalized(await realpath(directory));
    if (ancestors.has(physical)) throw new Error(`Cyclic node_modules link at ${location}`);
    ancestors.add(physical);
    try {
      const names = (await readdir(directory)).sort();
      for (const name of names) {
        if (name.startsWith('.')) continue; // npm metadata, .bin and tool caches.
        const child = path.join(directory, name);
        if (name.startsWith('@')) {
          if (!(await isDirectory(child))) throw new Error(`Unreadable package scope: ${location}/${name}`);
          for (const scoped of (await readdir(child)).sort()) {
            if (!scoped.startsWith('.')) await visitPackage(path.join(child, scoped), `${location}/${name}/${scoped}`);
          }
        } else {
          await visitPackage(child, `${location}/${name}`);
        }
      }
    } finally {
      ancestors.delete(physical);
    }
  }
  await visitModules(path.join(root, 'node_modules'), 'node_modules');
  for (const [location, actual] of installed) {
    const entry = expected.get(location);
    if (!entry) problems.push({ kind: 'extra', location, actual });
    else if (entry.version !== actual) problems.push({ kind: 'version', location, expected: entry.version, actual });
  }
  let optionalMissing = 0;
  for (const [location, entry] of expected) {
    if (installed.has(location)) continue;
    if (entry.optional === true) optionalMissing++;
    else problems.push({ kind: 'missing', location, expected: entry.version });
  }
  return { ok: problems.length === 0, installedPackages: installed.size, optionalMissing, problems };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length > 3) throw new Error('Usage: node verify_locked_dependencies.mjs [project-directory]');
    const report = await verifyLockedDependencies(process.argv[2] ?? process.cwd());
    console.log(JSON.stringify(report, null, 2));
    if (!report.ok) process.exitCode = 1;
  } catch (error) {
    console.error(`Locked dependency verification failed: ${error.message}`);
    process.exitCode = 1;
  }
}
