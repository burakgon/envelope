#!/usr/bin/env node
// Checks the interface translations against the English source catalog.
//
//   npm run i18n:check            errors fail the run (exit code 1)
//   npm run i18n:check -- -v      also lists messages that are identical to English
//
// Errors: invalid JSON or non-string values, missing or extra keys, placeholders that
// differ from English, a select turned into plain text, unparsable messages, plural
// branches the language doesn't have, or plural forms (other than "one") the language
// needs for whole numbers but the message lacks (e.g. Polish "few" and "many"), and
// plurals or selects in `native.*` messages (the desktop shell only fills in {name}).
// Warnings: English messages no source file seems to use.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dir = join(root, 'src/lib/i18n/locales');
const verbose = process.argv.includes('-v') || process.argv.includes('--verbose');

let errors = 0;
let warnings = 0;
const error = (file, msg) => {
  errors++;
  console.error(`  ✗ ${file}: ${msg}`);
};
const warn = (file, msg) => {
  warnings++;
  console.warn(`  ! ${file}: ${msg}`);
};

/** Parses the message syntax used by src/lib/i18n/message.ts and lists its arguments. */
function analyze(src) {
  let i = 0;
  const args = new Map(); // name -> 'text' | 'plural' | 'select'
  const plurals = []; // { name, branches: string[] }
  const fail = (what) => {
    throw new Error(`${what} at offset ${i}`);
  };
  const space = () => {
    while (i < src.length && /\s/.test(src[i])) i++;
  };
  const word = () => {
    const start = i;
    while (i < src.length && !/[\s,{}]/.test(src[i])) i++;
    return src.slice(start, i);
  };
  const note = (name, kind) => {
    const had = args.get(name);
    if (had && had !== kind && !(had === 'text' && kind === 'plural') && !(had === 'plural' && kind === 'text')) {
      fail(`"${name}" is used both as ${had} and ${kind}`);
    }
    if (!had || kind !== 'text') args.set(name, kind);
  };
  function body(nested) {
    while (i < src.length) {
      const c = src[i];
      if (c === '{') {
        i++;
        placeholder();
      } else if (c === '}' && nested) return;
      else i++;
    }
  }
  function placeholder() {
    space();
    const name = word();
    if (!name) fail('expected a placeholder name');
    space();
    if (src[i] === '}') {
      i++;
      note(name, 'text');
      return;
    }
    if (src[i] !== ',') fail('expected "," or "}"');
    i++;
    space();
    const type = word();
    if (type !== 'plural' && type !== 'select') fail(`unknown placeholder type "${type}"`);
    space();
    if (src[i] !== ',') fail('expected ","');
    i++;
    note(name, type);
    const branches = [];
    for (;;) {
      space();
      if (i >= src.length) fail('unclosed placeholder');
      if (src[i] === '}') {
        i++;
        break;
      }
      const key = word();
      if (!key) fail('expected a branch name');
      space();
      if (src[i] !== '{') fail('expected "{"');
      i++;
      branches.push(key);
      body(true);
      if (src[i] !== '}') fail('unclosed branch');
      i++;
    }
    if (!branches.includes('other')) fail(`"${name}" has no "other" branch`);
    if (type === 'plural') plurals.push({ name, branches });
  }
  body(false);
  return { args, plurals };
}

function loadCatalog(file) {
  const path = join(dir, file);
  let data;
  try {
    data = JSON.parse(readFileSync(path, 'utf8'));
  } catch (e) {
    error(file, `invalid JSON: ${e.message}`);
    return null;
  }
  if (!data || typeof data !== 'object' || Array.isArray(data)) {
    error(file, 'must be a flat JSON object');
    return null;
  }
  for (const [k, v] of Object.entries(data)) {
    if (typeof v !== 'string') error(file, `"${k}" must be a string`);
  }
  return data;
}

/**
 * Plural forms beyond "one" and "other" that a language uses for ordinary whole numbers
 * (Polish "few" and "many"; the French/Spanish/Italian/Portuguese "many" only starts at a million).
 * "one" isn't required: in languages like Turkish it usually reads the same as "other".
 */
function neededCategories(lang) {
  const rules = new Intl.PluralRules(lang);
  const needed = new Set();
  for (let n = 0; n <= 1000; n++) needed.add(rules.select(n));
  needed.delete('one');
  needed.delete('other');
  return needed;
}

const sourceFile = 'en.json';
const en = loadCatalog(sourceFile);
if (!en) process.exit(1);

/** `native.*` texts are filled in by the desktop shell, which only knows {name} placeholders. */
function plainOnly(file, key, info) {
  if (!key.startsWith('native.')) return;
  for (const [name, kind] of info.args) {
    if (kind !== 'text') error(file, `"${key}": {${name}} can't be a ${kind} in a native.* message (only {name} placeholders)`);
  }
}

const english = new Map();
for (const [key, message] of Object.entries(en)) {
  try {
    const info = analyze(message);
    english.set(key, info);
    plainOnly(sourceFile, key, info);
  } catch (e) {
    error(sourceFile, `"${key}": ${e.message}`);
  }
}

// English messages nothing seems to use (keys built at runtime count by their prefix).
function sourceFiles(d, out = []) {
  for (const name of readdirSync(d)) {
    const p = join(d, name);
    if (statSync(p).isDirectory()) sourceFiles(p, out);
    else if (/\.(svelte|ts)$/.test(name)) out.push(p);
  }
  return out;
}
const code = sourceFiles(join(root, 'src'))
  .map((p) => readFileSync(p, 'utf8'))
  .join('\n');
const prefixes = [...code.matchAll(/[`'"]([a-zA-Z][\w.]*\.)\$\{/g)].map((m) => m[1]);
// native.* texts are handed to the desktop shell, which looks them up by key.
const shellDir = join(root, 'src-tauri/src');
const shell = readdirSync(shellDir)
  .filter((f) => f.endsWith('.rs'))
  .map((f) => readFileSync(join(shellDir, f), 'utf8'))
  .join('\n');
const shellKeys = new Set([...shell.matchAll(/"(native\.[\w.]+)"/g)].map((m) => m[1]).filter((k) => !k.startsWith('native.test.')));
for (const key of shellKeys) if (!(key in en)) warn(sourceFile, `src-tauri uses "${key}", which has no English message`);
for (const key of Object.keys(en)) {
  if (key.startsWith('native.')) {
    if (!shellKeys.has(key)) warn(sourceFile, `"${key}" is not used by src-tauri`);
    continue;
  }
  if (code.includes(`'${key}'`) || code.includes(`"${key}"`) || code.includes(`\`${key}\``)) continue;
  if (prefixes.some((p) => key.startsWith(p))) continue;
  warn(sourceFile, `"${key}" looks unused`);
}

const locales = readdirSync(dir)
  .filter((f) => f.endsWith('.json') && f !== sourceFile)
  .sort();

console.log(`${sourceFile}: ${english.size} messages`);
for (const file of locales) {
  const lang = file.replace(/\.json$/, '');
  const before = errors;
  const catalog = loadCatalog(file);
  if (!catalog) continue;
  let supported;
  try {
    supported = new Set(new Intl.PluralRules(lang).resolvedOptions().pluralCategories);
  } catch {
    error(file, `"${lang}" is not a valid language tag`);
    continue;
  }
  const needed = neededCategories(lang);
  const missing = Object.keys(en).filter((k) => !(k in catalog));
  const extra = Object.keys(catalog).filter((k) => !(k in en));
  if (missing.length) error(file, `${missing.length} missing: ${missing.join(', ')}`);
  if (extra.length) error(file, `${extra.length} not in English: ${extra.join(', ')}`);

  const same = [];
  for (const [key, message] of Object.entries(catalog)) {
    const source = english.get(key);
    if (!source || typeof message !== 'string') continue;
    if (message === en[key] && /[a-z]{3}/i.test(message)) same.push(key);
    if (!message.trim()) {
      error(file, `"${key}" is empty`);
      continue;
    }
    let info;
    try {
      info = analyze(message);
    } catch (e) {
      error(file, `"${key}": ${e.message}`);
      continue;
    }
    plainOnly(file, key, info);
    const want = [...source.args.keys()].sort();
    const have = [...info.args.keys()].sort();
    if (want.join() !== have.join()) {
      error(file, `"${key}" has placeholders {${have.join('}, {')}} but English has {${want.join('}, {')}}`);
    }
    for (const [name, kind] of source.args) {
      if (kind === 'select' && info.args.get(name) !== 'select') error(file, `"${key}": {${name}} must stay a select`);
    }
    for (const { name, branches } of info.plurals) {
      const bad = branches.filter((b) => !b.startsWith('=') && !supported.has(b));
      if (bad.length) error(file, `"${key}": ${lang} has no plural form ${bad.join(', ')} (it uses ${[...supported].join(', ')})`);
      const lacking = [...needed].filter((c) => !branches.includes(c));
      if (lacking.length) error(file, `"${key}": {${name}} needs the ${lacking.join(', ')} plural form${lacking.length > 1 ? 's' : ''} in ${lang}`);
    }
  }
  const status = errors === before ? 'ok' : `${errors - before} error${errors - before === 1 ? '' : 's'}`;
  console.log(`${file}: ${Object.keys(catalog).length} messages, ${status}${same.length ? ` (${same.length} identical to English)` : ''}`);
  if (verbose && same.length) console.log(`    identical: ${same.join(', ')}`);
}

console.log('');
if (errors) {
  console.error(`i18n: ${errors} error${errors === 1 ? '' : 's'}, ${warnings} warning${warnings === 1 ? '' : 's'} in ${relative(root, dir)}`);
  process.exit(1);
}
console.log(`i18n: ${locales.length + 1} catalogs ok${warnings ? `, ${warnings} warning${warnings === 1 ? '' : 's'}` : ''}`);
