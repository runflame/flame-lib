#!/usr/bin/env node
// Prints the next release candidate of an npm package: the crate's version
// as Cargo.lock records it, with `-rc.N`, N one past the highest `-rc.` of
// that version the registry has.
//
//   node flamewallet-ffi/scripts/next-rc.mjs flamewallet-ffi @runflame/wallet-rn
//   → 0.0.1-rc.4
//
// Refuses when the plain version is already published: an rc of a released
// version sorts before the release, and would be a step back. `--published
// '<json array>'` stands in for the registry, to try the arithmetic offline.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const [crate, pkg, flag, published] = process.argv.slice(2);
if (!crate || !pkg || (flag && flag !== '--published')) {
    console.error('usage: next-rc.mjs <crate> <npm package> [--published <json array>]');
    process.exit(2);
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

/** The version of `name` in Cargo.lock; a workspace member appears once. */
function lockedVersion(name) {
    const lock = readFileSync(resolve(root, 'Cargo.lock'), 'utf8');
    const versions = lock
        .split('[[package]]')
        .map(block => ({
            name: /^name = "(.*)"$/m.exec(block)?.[1],
            version: /^version = "(.*)"$/m.exec(block)?.[1]
        }))
        .filter(entry => entry.name === name)
        .map(entry => entry.version);
    if (versions.length !== 1) {
        throw new Error(`Cargo.lock has ${versions.length} entries for ${name}`);
    }
    return versions[0];
}

/** Every version the registry has of `name`; none for a package it lacks. */
function registryVersions(name) {
    try {
        const out = execFileSync('npm', ['view', name, 'versions', '--json', '--prefer-online'], {
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'pipe']
        });
        const parsed = JSON.parse(out || '[]');
        // One version comes back as a string, several as an array.
        return Array.isArray(parsed) ? parsed : [parsed];
    } catch (error) {
        if (`${error.stdout}${error.stderr}`.includes('E404')) {
            return [];
        }
        throw error;
    }
}

function nextCandidate() {
    const version = lockedVersion(crate);
    if (!/^\d+\.\d+\.\d+$/.test(version)) {
        throw new Error(`${crate} is ${version}; a release candidate is cut from a plain X.Y.Z`);
    }
    const versions = published ? JSON.parse(published) : registryVersions(pkg);
    if (versions.includes(version)) {
        throw new Error(`${pkg}@${version} is released; bump ${crate}'s version before the next candidate`);
    }
    const candidate = new RegExp(`^${version.replaceAll('.', '\\.')}-rc\\.(\\d+)$`);
    const highest = versions.reduce((max, v) => Math.max(max, Number(candidate.exec(v)?.[1] ?? 0)), 0);
    return `${version}-rc.${highest + 1}`;
}

try {
    console.log(nextCandidate());
} catch (error) {
    // One line in a CI log, not a stack trace.
    console.error(`next-rc: ${error.message}`);
    process.exit(1);
}
