import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { verifyLockedDependencies } from './verify_locked_dependencies.mjs';

const fixtureBase = path.resolve('D:/DevCache/ClashTrafficMonitorTauri/locked-dependency-tests');
const checker = fileURLToPath(new URL('./verify_locked_dependencies.mjs', import.meta.url));

async function fixture(t) {
  await mkdir(fixtureBase, { recursive: true });
  const directory = await mkdtemp(path.join(fixtureBase, 'fixture-'));
  t.after(async () => {
    const relative = path.relative(fixtureBase, directory);
    assert.ok(relative.startsWith('fixture-') && !relative.includes(path.sep));
    assert.equal(path.dirname(await realpath(directory)), await realpath(fixtureBase));
    await rm(directory, { recursive: true });
  });
  return directory;
}
async function json(file, value) {
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, `${JSON.stringify(value)}\n`);
}
async function lock(directory, packages) {
  await json(path.join(directory, 'package-lock.json'), { lockfileVersion: 3, packages: { '': {}, ...packages } });
}
async function install(directory, location, version, dependencies = {}) {
  await json(path.join(directory, location, 'package.json'), { name: path.basename(location), version, dependencies });
}
async function contentHashes(directory) {
  const result = {};
  async function visit(current, relative) {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const file = path.join(current, entry.name), name = path.posix.join(relative, entry.name);
      if (entry.isDirectory()) await visit(file, name);
      else result[name] = createHash('sha256').update(await readFile(file)).digest('hex');
    }
  }
  await visit(directory, '');
  return result;
}

test('matches exact scoped/nested versions without modifying source files', async (t) => {
  const directory = await fixture(t);
  await lock(directory, { 'node_modules/@scope/tool': { version: '1.0.0' }, 'node_modules/@scope/tool/node_modules/child': { version: '2.0.0' } });
  await install(directory, 'node_modules/@scope/tool', '1.0.0');
  await install(directory, 'node_modules/@scope/tool/node_modules/child', '2.0.0');
  await json(path.join(directory, 'node_modules/.package-lock.json'), { stale: true });
  const before = await contentHashes(directory);
  const report = await verifyLockedDependencies(directory);
  assert.equal(report.ok, true);
  assert.equal(report.installedPackages, 2);
  assert.deepEqual(await contentHashes(directory), before);
});

test('rejects a transitive version different from lock even within its semver range', async (t) => {
  const directory = await fixture(t);
  await lock(directory, { 'node_modules/parent': { version: '1.0.0' }, 'node_modules/child': { version: '1.1.0' } });
  await install(directory, 'node_modules/parent', '1.0.0', { child: '^1.0.0' });
  await install(directory, 'node_modules/child', '1.2.0');
  const report = await verifyLockedDependencies(directory);
  assert.equal(report.ok, false);
  assert.deepEqual(report.problems, [{ kind: 'version', location: 'node_modules/child', expected: '1.1.0', actual: '1.2.0' }]);
  const cli = spawnSync(process.execPath, [checker, directory], { encoding: 'utf8' });
  assert.equal(cli.status, 1);
  assert.equal(JSON.parse(cli.stdout).ok, false);
});

test('rejects packages absent from the lockfile', async (t) => {
  const directory = await fixture(t);
  await lock(directory, {});
  await install(directory, 'node_modules/extra', '1.0.0');
  const report = await verifyLockedDependencies(directory);
  assert.equal(report.ok, false);
  assert.equal(report.problems[0].kind, 'extra');
});

test('rejects missing required packages, including nested packages', async (t) => {
  const directory = await fixture(t);
  await lock(directory, { 'node_modules/parent': { version: '1.0.0' }, 'node_modules/parent/node_modules/child': { version: '2.0.0' } });
  await install(directory, 'node_modules/parent', '1.0.0');
  const report = await verifyLockedDependencies(directory);
  assert.equal(report.ok, false);
  assert.deepEqual(report.problems, [{ kind: 'missing', location: 'node_modules/parent/node_modules/child', expected: '2.0.0' }]);
});

test('allows absent optional packages but checks their exact version when installed', async (t) => {
  const directory = await fixture(t);
  await lock(directory, { 'node_modules/native': { version: '1.0.0', optional: true } });
  assert.deepEqual(await verifyLockedDependencies(directory), { ok: true, installedPackages: 0, optionalMissing: 1, problems: [] });
  await install(directory, 'node_modules/native', '1.0.1');
  assert.equal((await verifyLockedDependencies(directory)).problems[0].kind, 'version');
});

test('preserves logical lockfile locations through a Windows node_modules junction', async (t) => {
  const directory = await fixture(t);
  const project = path.join(directory, 'project'), physical = path.join(directory, 'physical-modules');
  await lock(project, { 'node_modules/@scope/tool': { version: '1.0.0' }, 'node_modules/@scope/tool/node_modules/child': { version: '2.0.0' } });
  await mkdir(physical, { recursive: true });
  await symlink(physical, path.join(project, 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  await install(project, 'node_modules/@scope/tool', '1.0.0');
  await install(project, 'node_modules/@scope/tool/node_modules/child', '2.0.0');
  assert.equal((await verifyLockedDependencies(project)).installedPackages, 2);
  assert.equal((await verifyLockedDependencies(project)).ok, true);
  assert.equal(await realpath(path.join(project, 'node_modules')), await realpath(physical));
});
