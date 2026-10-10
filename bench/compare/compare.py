#!/usr/bin/env python3
"""rtex vs Typst vs a full LaTeX run (what Overleaf does per recompile), on the same text.

The document is the one the paper's Typst comparison uses (bench/upstream/luatex-benchmark/
typst-comparison/bench.mjs): a Lorem ipsum paragraph repeated N times, A4, 2.5 cm margins,
11 pt, justified, with a "Paragraph K marker." sentence in each. A LaTeX twin of it is
generated here (article, Latin Modern), and the middle paragraph is edited the same way.

For each document size this measures:
  - rtex: `rtex serve` on the LaTeX twin, 30 edits of the middle paragraph after the first
    layout; the session's own round-trip time per edit (`timing.total_us` of each
    ParagraphUpdate: from the moment the edit is dispatched to the engine until its result is
    decoded), median and P95 over all edits but the first;
  - pdfLaTeX and LuaLaTeX: one full run of the LaTeX twin, best of 3 after a warm-up run;
  - Typst (when `typst` is on PATH): the upstream bench.mjs, unmodified, in watch mode
    (exports the whole PDF per edit) and with `--pages 1` (≈ compile time only).

Usage:
  source build/texlive.env
  python3 bench/compare/compare.py --rtex target/release/rtex [--pages 10 100 300] [--out bench/results]
"""
import argparse, json, os, re, shutil, statistics, subprocess, sys, tempfile, time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
UPSTREAM = REPO / "bench/upstream/luatex-benchmark/typst-comparison/bench.mjs"
LOREM = ("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor "
         "incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud "
         "exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure "
         "dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. "
         "Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt "
         "mollit anim id est laborum.")
# paragraphs per page for this text and layout (bench.mjs tunes Typst to 8.65/page; the LaTeX
# twin with the same count lands within a few percent of the same page count)
PARAS_PER_PAGE = 8.65


def latex_doc(paras):
    head = ("\\documentclass[11pt]{article}\n\\usepackage[a4paper,margin=2.5cm]{geometry}\n"
            "\\usepackage[T1]{fontenc}\n\\usepackage{lmodern}\n\\pagestyle{plain}\n"
            "\\begin{document}\n\\section*{Benchmark Document}\n\n")
    body = "".join(f"{LOREM} Paragraph {i} marker.\n\n" for i in range(paras))
    return head + body + "\\end{document}\n"


def full_run(engine, project):
    subprocess.run([engine, "-interaction=batchmode", "main.tex"], cwd=project,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    best = None
    for _ in range(3):
        t0 = time.perf_counter()
        subprocess.run([engine, "-interaction=batchmode", "main.tex"], cwd=project,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        t = time.perf_counter() - t0
        best = t if best is None else min(best, t)
    log = (project / "main.log").read_text(errors="replace")
    m = re.search(r"Output written on .*?\((\d+) pages?", log, re.S)
    return best, int(m.group(1)) if m else None


def rtex_edits(rtex, project, build, paras, edits):
    p = subprocess.Popen([rtex, "serve", "--project", str(project), "--build", str(build)],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)

    def send(o):
        p.stdin.write(json.dumps(o) + "\n")
        p.stdin.flush()

    def wait(pred, timeout=600):
        end = time.time() + timeout
        while time.time() < end:
            line = p.stdout.readline()
            if not line:
                raise SystemExit("rtex serve ended")
            o = json.loads(line)
            if pred(o):
                return o
        raise SystemExit("timeout")

    wait(lambda o: o.get("event") == "LayoutUpdate"
         and o["convergence"]["state"] in ("Converged", "PassLimitReached"))
    text = (project / "main.tex").read_text()
    k = paras // 2
    times = []
    for i in range(edits):
        m = re.search(rf"Paragraph {k} marker[^.]*\.", text)
        new = f"Paragraph {k} marker edit{i}."
        start, end = len(text[:m.start()].encode()), len(text[:m.end()].encode())
        send({"cmd": "edit", "path": "main.tex", "start": start, "end": end, "text": new})
        text = text[:m.start()] + new + text[m.end():]
        ev = wait(lambda o: o.get("event") in ("ParagraphUpdate", "BackgroundScheduled"))
        if ev["event"] != "ParagraphUpdate" or ev["status"] != "ok":
            raise SystemExit(f"edit {i} not live: {ev.get('event')} {ev.get('status')} {ev.get('reasons')}")
        times.append(ev["timing"]["total_us"] / 1000)
        time.sleep(0.05)  # separate keystrokes, like bench.mjs's sequential edits
    send({"cmd": "quit"})
    p.wait(timeout=60)
    steady = sorted(times[1:])
    return statistics.median(steady), steady[int(len(steady) * 0.95)]


def typst_bench(pages, extra):
    with tempfile.TemporaryDirectory() as d:
        shutil.copy(UPSTREAM, d)
        env = dict(os.environ, TYPST_EXTRA=extra) if extra else dict(os.environ)
        out = subprocess.run(["node", "bench.mjs", str(pages), "30"], cwd=d, env=env,
                             capture_output=True, text=True).stdout
    get = lambda k: re.search(rf"{k}:\s+(.+)", out).group(1).strip()
    return {"version": get("Typst version"), "pages": int(get("Document size").split()[0]),
            "p50_ms": float(get(r"Incremental \(p50\)").split()[0])}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rtex", default=str(REPO / "target/release/rtex"))
    ap.add_argument("--pages", type=int, nargs="+", default=[10, 100, 300])
    ap.add_argument("--edits", type=int, default=30)
    ap.add_argument("--out", default=str(REPO / "bench/results"))
    a = ap.parse_args()
    have_typst = shutil.which("typst") and shutil.which("node")
    rows = []
    for pages in a.pages:
        paras = round(pages * PARAS_PER_PAGE)
        with tempfile.TemporaryDirectory() as d:
            proj = Path(d) / "project"
            proj.mkdir()
            (proj / "main.tex").write_text(latex_doc(paras))
            pdf_t, n_pages = full_run("pdflatex", proj)
            lua_t, _ = full_run("lualatex", proj)
            for f in proj.iterdir():
                if f.name != "main.tex":
                    f.unlink()
            r50, r95 = rtex_edits(a.rtex, proj, Path(d) / "build", paras, a.edits)
        row = {"target_pages": pages, "paragraphs": paras, "latex_pages": n_pages,
               "rtex_p50_ms": round(r50, 2), "rtex_p95_ms": round(r95, 2),
               "pdflatex_s": round(pdf_t, 2), "lualatex_s": round(lua_t, 2)}
        if have_typst:
            full = typst_bench(pages, "")
            one = typst_bench(pages, "--pages 1")
            row.update(typst_version=full["version"], typst_pages=full["pages"],
                       typst_watch_p50_ms=full["p50_ms"], typst_one_page_p50_ms=one["p50_ms"])
        print(json.dumps(row), flush=True)
        rows.append(row)

    stamp = time.strftime("%Y%m%d-%H%M")
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / f"compare-{stamp}.json").write_text(json.dumps(rows, indent=1) + "\n")
    cpu = next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")),
               "") if os.path.exists("/proc/cpuinfo") else ""
    md = [f"# rtex vs Typst vs a full LaTeX run ({time.strftime('%Y-%m-%d')})", "",
          f"Machine: {cpu} ({os.cpu_count()} threads). Method: bench/compare/compare.py.", "",
          "| Pages | rtex, per edit (P95) | Typst watch | Typst, one page exported | pdfLaTeX run | LuaLaTeX run |",
          "|---|---|---|---|---|---|"]
    for r in rows:
        tw = f"{r['typst_watch_p50_ms']:.0f} ms" if "typst_watch_p50_ms" in r else "—"
        t1 = f"{r['typst_one_page_p50_ms']:.0f} ms" if "typst_one_page_p50_ms" in r else "—"
        md.append(f"| {r['latex_pages']} | {r['rtex_p50_ms']:.2f} ms ({r['rtex_p95_ms']:.2f}) | {tw} | {t1} "
                  f"| {r['pdflatex_s']:.2f} s | {r['lualatex_s']:.2f} s |")
    if rows and "typst_version" in rows[0]:
        md += ["", f"Typst: {rows[0]['typst_version']}."]
    (out / f"compare-{stamp}.md").write_text("\n".join(md) + "\n")
    print(f"written to {out}/compare-{stamp}.md")


if __name__ == "__main__":
    main()
