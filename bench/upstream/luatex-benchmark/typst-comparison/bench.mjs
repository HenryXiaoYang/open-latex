/**
 * Typst incremental recompile benchmark (engine-agnostic — drives the
 * `typst` CLI only). Reproduces the Typst column of the comparison table.
 *
 * 1. Tunes a Lorem document to a target page count (binary search).
 * 2. Compiles once cold (records cold time).
 * 3. Spawns `typst watch`, waits for the initial compile, then edits one
 *    paragraph mid-document N times and parses the "compiled in X" line
 *    from watch's output.
 *
 * Usage:
 *   node bench.mjs <target-pages> <edits>
 *   node bench.mjs 10  30
 *   node bench.mjs 100 30
 *   node bench.mjs 300 30
 *
 * Requires Typst on PATH. The wording of the watch timing line is version-
 * dependent; the regex in handleLine() covers ms / s / µs forms.
 *
 * $TYPST_EXTRA (space-separated) is appended to the `typst watch` arguments,
 * e.g. TYPST_EXTRA="--pages 1" to export a single page (≈ compile time only).
 */

import { spawn, execFileSync } from 'node:child_process';
import { writeFileSync, readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const TARGET_PAGES = Number(process.argv[2] || 300);
const EDITS = Number(process.argv[3] || 30);
// Edit position: 'mid' (default) edits the middle paragraph (a typical edit,
// triggering ~half the document's reflow cascade); 'end' edits the last
// paragraph (best case for Typst, minimal cascade). Position is irrelevant to
// LuaLaTeX, which only ever recompiles the edited paragraph.
const EDIT_POS = (process.argv[4] || 'mid').toLowerCase();
const DOC = resolve(__dirname, 'doc.typ');
const PDF = resolve(__dirname, 'doc.pdf');

const LOREM = `Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.`;

const HEADER = `#set page(paper: "a4", margin: 2.5cm)
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true, leading: 0.65em)

= Benchmark Document

`;

function buildDoc(paraCount) {
  let body = HEADER;
  for (let i = 0; i < paraCount; i++) {
    body += `${LOREM} Paragraph ${i} marker.\n\n`;
  }
  return body;
}

function compileOnce() {
  const t0 = performance.now();
  execFileSync('typst', ['compile', DOC, PDF], { stdio: 'pipe' });
  return performance.now() - t0;
}

// Binary-search the paragraph count that yields ~TARGET_PAGES pages.
function tuneToTargetPages() {
  let paras = 900; // initial guess for ~300 pages
  let pages = 0;
  for (let it = 0; it < 8; it++) {
    writeFileSync(DOC, buildDoc(paras));
    compileOnce();
    const str = readFileSync(PDF).toString('latin1');
    pages = (str.match(/\/Type\s*\/Page[^s]/g) || []).length;
    console.log(`  tune: paras=${paras} -> pages=${pages}`);
    if (Math.abs(pages - TARGET_PAGES) <= 5) break;
    paras = Math.round(paras * (TARGET_PAGES / pages));
  }
  return { paras, pages };
}

function stats(arr) {
  const s = [...arr].sort((a, b) => a - b);
  return {
    min: s[0],
    p50: s[Math.floor(s.length / 2)],
    p95: s[Math.floor(s.length * 0.95)],
    max: s[s.length - 1],
    avg: arr.reduce((a, b) => a + b, 0) / arr.length,
  };
}

async function main() {
  console.log(`\nTypst incremental recompile benchmark — target ${TARGET_PAGES} pages, ${EDITS} edits\n`);

  console.log('Tuning document size to target pages…');
  const { paras, pages } = tuneToTargetPages();
  console.log(`  Using ${paras} paragraphs -> ${pages} pages\n`);

  console.log('Cold compile (CLI, 3 runs):');
  const coldRuns = [];
  for (let i = 0; i < 3; i++) {
    const ms = compileOnce();
    coldRuns.push(ms);
    console.log(`  run ${i + 1}: ${ms.toFixed(0)} ms`);
  }
  const coldAvg = coldRuns.reduce((a, b) => a + b, 0) / coldRuns.length;
  console.log(`  avg: ${coldAvg.toFixed(0)} ms\n`);

  console.log(`Incremental (typst watch, ${EDITS} edits):`);
  const watch = spawn('typst', ['watch', DOC, PDF, ...(process.env.TYPST_EXTRA ? process.env.TYPST_EXTRA.split(' ') : [])], { stdio: ['ignore', 'pipe', 'pipe'] });

  const incrementalTimes = [];
  let initialCompileSeen = false;
  let pendingResolve = null;
  let buffered = '';

  function handleLine(line) {
    const m = /compiled .*? in ([\d.]+)\s*(ms|s|µs|us)/.exec(line);
    if (!m) return;
    const val = parseFloat(m[1]);
    const unit = m[2];
    const ms = unit === 's' ? val * 1000 : unit === 'ms' ? val : val / 1000;
    if (!initialCompileSeen) {
      initialCompileSeen = true;
      console.log(`  initial watch compile: ${ms.toFixed(0)} ms`);
      return;
    }
    if (pendingResolve) {
      const r = pendingResolve;
      pendingResolve = null;
      r(ms);
    }
  }

  function onData(chunk) {
    buffered += chunk.toString();
    let idx;
    while ((idx = buffered.indexOf('\n')) !== -1) {
      handleLine(buffered.slice(0, idx));
      buffered = buffered.slice(idx + 1);
    }
  }
  watch.stderr.on('data', onData);
  watch.stdout.on('data', onData);

  await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error('Watch initial compile timeout')), 60_000);
    const iv = setInterval(() => {
      if (initialCompileSeen) { clearInterval(iv); clearTimeout(timer); res(); }
    }, 20);
  });

  const editPara = EDIT_POS === 'end' ? paras - 1 : Math.floor(paras / 2);
  for (let i = 0; i < EDITS; i++) {
    const doc = readFileSync(DOC, 'utf8');
    const edited = doc.replace(
      new RegExp(`Paragraph ${editPara} marker[^.]*\\.`),
      `Paragraph ${editPara} marker edit${i}.`,
    );
    const t0 = performance.now();
    const promise = new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error('recompile timeout')), 30_000);
      pendingResolve = (ms) => { clearTimeout(timer); res({ reported: ms, wall: performance.now() - t0 }); };
    });
    writeFileSync(DOC, edited);
    const { reported, wall } = await promise;
    incrementalTimes.push(reported);
    if (i < 3 || i >= EDITS - 2) {
      console.log(`  #${(i + 1).toString().padStart(2)}: reported=${reported.toFixed(1).padStart(6)} ms  wall=${wall.toFixed(0).padStart(4)} ms`);
    } else if (i === 3) {
      console.log('  …');
    }
  }

  watch.kill();

  const steady = incrementalTimes.slice(1); // drop first (warm-up)
  const s = stats(steady);
  console.log(`\nIncremental recompile (${steady.length} edits, warm-up excluded):`);
  console.log(`  avg: ${s.avg.toFixed(1)} ms   p50: ${s.p50.toFixed(1)} ms   p95: ${s.p95.toFixed(1)} ms   min: ${s.min.toFixed(1)} ms   max: ${s.max.toFixed(1)} ms`);

  console.log(`\nSUMMARY`);
  console.log(`  Typst version:       ${execFileSync('typst', ['--version']).toString().trim()}`);
  console.log(`  Edit position:       ${EDIT_POS} (paragraph ${editPara} of ${paras})`);
  console.log(`  Document size:       ${pages} pages, ${paras} paragraphs`);
  console.log(`  Cold compile (avg):  ${coldAvg.toFixed(0)} ms`);
  console.log(`  Incremental (avg):   ${s.avg.toFixed(1)} ms`);
  console.log(`  Incremental (p50):   ${s.p50.toFixed(1)} ms`);
}

main().catch((e) => { console.error(e); process.exit(1); });
