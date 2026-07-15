// Chud — ModScan (mod malware scanner). Self-contained; main.js routes the
// "modscan" page here via window.renderModscan(). Status + manual-scan surface
// for Chud's structural + VirusTotal reputation check on downloaded mods.
(function () {
  "use strict";
  const S = window.ChudShared;
  const esc = S.esc;
  const inv = S.invoke;

  const st = {
    installed: null, scanningAll: false, folderResult: null, showClean: false,
    rescanning: {}, rescanResult: {},
    manualPath: "", manualLoading: false, manualResult: null,
  };
  let root = null;

  // ── browser-preview mocks (same pattern as MOCK_STATE / MOCK_MODS) ──
  const MOCK_INSTALLED = {
    "mock-clean-mod": { name: "Starfall Ahri", champ: "Ahri", version: "1.0.0", size_mb: 12.4, file: "mods/starfall_ahri.fantome", scan_verdict: "clean", scan_sha: "3a7f9c1e" },
    "mock-flagged-mod": { name: "Suspicious VFX Pack", champ: "", version: "1.2.0", size_mb: 4.1, file: "mods/sketchy_vfx.fantome", scan_verdict: "suspicious", scan_sha: "9be0123a" },
  };
  const MOCK_FOLDER_RESULT = {
    total: 2, clean: 1, flagged: 1,
    results: [
      { file: "mods/starfall_ahri.fantome", name: "Starfall Ahri", verdict: "clean", findings: [], sha256: "3a7f9c1e" },
      { file: "mods/sketchy_vfx.fantome", name: "Suspicious VFX Pack", verdict: "suspicious", findings: [{ severity: "warning", code: "embedded_script", entry: "meta/lua/init.lua", detail: "Contains an embedded Lua script — uncommon for a VFX-only mod." }], sha256: "9be0123a" },
    ],
  };
  const MOCK_RESCAN = {
    "mock-clean-mod": { verdict: "clean", sha256: "3a7f9c1e", blocking: false, findings: [] },
    "mock-flagged-mod": { verdict: "suspicious", sha256: "9be0123a", blocking: false, findings: [{ severity: "warning", code: "embedded_script", entry: "meta/lua/init.lua", detail: "Contains an embedded Lua script — uncommon for a VFX-only mod." }], vt: { known: true, verdict: "suspicious", vt: { malicious: 2, total: 71 } } },
  };
  function mockManualScan(path) {
    const bad = /bad|malware|virus|suspicious/i.test(path);
    return { file: path, name: path.split(/[\\/]/).pop(), scan: bad
      ? { verdict: "malicious", sha256: "deadbeef", blocking: true, findings: [{ severity: "critical", code: "known_malware", detail: "Matches a known malicious signature." }], vt: { known: true, verdict: "malicious", vt: { malicious: 54, total: 71 } } }
      : { verdict: "clean", sha256: "cafebabe", blocking: false, findings: [] } };
  }

  // ── toasts (reuse the app's #toasts container) ──
  function toast(title, msg, tone) {
    const wrap = document.getElementById("toasts"); if (!wrap) return;
    const el = document.createElement("div");
    el.className = `toast ${tone === "success" ? "success" : tone === "danger" ? "danger" : tone === "warning" ? "warning" : ""}`;
    el.innerHTML = `<div class="toast-bar"></div><div><div class="toast-title">${esc(title)}</div>${msg ? `<div class="toast-msg">${esc(msg)}</div>` : ""}</div>`;
    wrap.appendChild(el);
    setTimeout(() => { el.classList.add("out"); setTimeout(() => el.remove(), 300); }, 3400);
  }

  // ── verdict badge — one mapping, reused everywhere (malicious loudest) ──
  const VERDICT_INFO = {
    clean: { label: "Clean", cls: "ok" },
    suspicious: { label: "Suspicious", cls: "warn" },
    malicious: { label: "Malicious", cls: "bad" },
    error: { label: "Unreadable", cls: "err" },
    "": { label: "Not scanned", cls: "none" },
  };
  function verdictBadge(v) {
    const info = VERDICT_INFO[v || ""] || VERDICT_INFO[""];
    return `<span class="chip ms-v-${info.cls}"><span class="ms-dot"></span>${esc(info.label)}</span>`;
  }

  const SEV_COLOR = { critical: "var(--red)", malicious: "var(--red)", warning: "var(--amber)", suspicious: "var(--amber)", info: "var(--text-muted)" };
  const sevColor = (s) => SEV_COLOR[s] || SEV_COLOR.info;

  function findingsHtml(findings) {
    if (!findings || !findings.length) return "";
    return `<div class="ms-findings">${findings.map((f) => `
      <div class="ms-finding"><span class="ms-fdot" style="background:${sevColor(f.severity)}"></span>
        <div><span class="ms-fcode">${esc((f.code || "finding").toUpperCase())}</span>${f.entry ? ` <span class="ms-fentry">${esc(f.entry)}</span>` : ""}
        ${f.detail ? `<div class="ms-fdetail">${esc(f.detail)}</div>` : ""}</div>
      </div>`).join("")}</div>`;
  }
  function vtLine(scan) {
    if (!scan || !scan.vt || !scan.vt.known) return "";
    const vt = scan.vt.vt || {};
    return `<div class="ms-vt">VirusTotal: ${vt.malicious || 0}/${vt.total || 0} engines flagged</div>`;
  }

  async function load() {
    try {
      const state = await inv("library_state");
      st.installed = (state && state.installed) || (S.hasBackend ? {} : MOCK_INSTALLED);
    } catch (e) { console.error("modscan load failed", e); st.installed = S.hasBackend ? {} : MOCK_INSTALLED; }
  }

  async function scanAll() {
    if (st.scanningAll) return;
    st.scanningAll = true; st.folderResult = null; paint();
    try {
      const r = await inv("modscan_scan_folder");
      st.folderResult = r || (S.hasBackend ? { total: 0, clean: 0, flagged: 0, results: [] } : MOCK_FOLDER_RESULT);
    } catch (e) { toast("Scan failed", String(e).slice(0, 120), "danger"); st.folderResult = { total: 0, clean: 0, flagged: 0, results: [] }; }
    st.scanningAll = false; paint();
  }

  async function rescan(id) {
    if (st.rescanning[id]) return;
    st.rescanning[id] = true; paint();
    try {
      const r = await inv("modscan_rescan", { modId: id });
      const scan = r || (S.hasBackend ? null : (MOCK_RESCAN[id] || { verdict: "clean", findings: [] }));
      if (scan) {
        st.rescanResult[id] = scan;
        if (st.installed[id]) st.installed[id].scan_verdict = scan.verdict;
      } else {
        toast("Re-scan failed", "The backend didn't return a result.", "danger");
      }
    } catch (e) { toast("Re-scan failed", String(e).slice(0, 120), "danger"); }
    delete st.rescanning[id]; paint();
  }

  async function scanPath() {
    const path = (st.manualPath || "").trim();
    if (!path) { toast("Enter a path", "Paste the full path to a .fantome file first.", "warning"); return; }
    st.manualLoading = true; paint();
    try {
      const r = await inv("modscan_scan_path", { path });
      st.manualResult = r || (S.hasBackend ? null : mockManualScan(path));
      if (!st.manualResult) toast("Scan failed", "Could not scan that path.", "danger");
    } catch (e) { toast("Scan failed", String(e).slice(0, 120), "danger"); }
    st.manualLoading = false; paint();
  }

  // ── folder scan results ──
  function flaggedRow(x) {
    const isErr = x.verdict === "error";
    return `<div class="ms-frow ${isErr ? "ms-frow-err" : ""}">
      <div class="ms-frow-top"><span class="ms-frow-name" title="${esc(x.name || x.file || "")}">${esc(x.name || x.file || "unknown")}</span>${verdictBadge(isErr ? "error" : x.verdict)}</div>
      ${isErr ? `<div class="ms-fdetail">Could not read this file — it may be corrupt or in an unsupported format.</div>` : findingsHtml(x.findings)}
    </div>`;
  }
  function cleanRow(x) {
    return `<div class="ms-crow"><span class="ms-crow-name" title="${esc(x.name || x.file || "")}">${esc(x.name || x.file || "unknown")}</span>${verdictBadge("clean")}</div>`;
  }
  function scanAllResultHtml() {
    if (st.scanningAll) return `<div class="glass ms-card ms-scanning"><div class="ms-spinner"></div><div>Scanning your mods folder… this can take a few seconds.</div></div>`;
    const r = st.folderResult;
    if (!r) return "";
    const results = r.results || [];
    const flagged = results.filter((x) => x.verdict && x.verdict !== "clean");
    const clean = results.filter((x) => !x.verdict || x.verdict === "clean");
    return `<div class="glass ms-card">
      <div class="ms-summary ${(r.flagged || 0) > 0 ? "ms-summary-warn" : ""}"><b>${r.total || 0}</b> mod${(r.total || 0) === 1 ? "" : "s"} scanned · <span class="ms-sum-ok">${r.clean || 0} clean</span> · <span class="ms-sum-bad">${r.flagged || 0} flagged</span></div>
      ${flagged.length ? `<div class="ms-flagged-list">${flagged.map(flaggedRow).join("")}</div>` : ""}
      ${clean.length ? `<div class="ms-showclean" data-showclean="1">${st.showClean ? "Hide clean files ▴" : `Show ${clean.length} clean file${clean.length === 1 ? "" : "s"} ▾`}</div>` : ""}
      ${st.showClean && clean.length ? `<div class="ms-clean-list">${clean.map(cleanRow).join("")}</div>` : ""}
    </div>`;
  }

  // ── installed mods list ──
  function installedRow(id) {
    const rec = st.installed[id] || {};
    const busy = !!st.rescanning[id];
    const lastScan = st.rescanResult[id];
    const verdict = (lastScan && lastScan.verdict) || rec.scan_verdict || "";
    const showDetail = lastScan && ((lastScan.findings || []).length || (lastScan.vt && lastScan.vt.known));
    return `<div class="ms-irow-wrap">
      <div class="ms-irow">
        <div class="ms-irow-main"><div class="ms-irow-name">${esc(rec.name || id)}</div><div class="ms-irow-meta">${esc(rec.champ || "Other")}</div></div>
        ${verdictBadge(verdict)}
        <button class="btn sm" data-rescan="${esc(id)}" ${busy ? "disabled" : ""}>${busy ? "Scanning…" : "Re-scan"}</button>
      </div>
      ${showDetail ? `<div class="ms-irow-detail">${findingsHtml(lastScan.findings)}${vtLine(lastScan)}</div>` : ""}
    </div>`;
  }
  function installedListHtml() {
    const ids = Object.keys(st.installed || {});
    if (!ids.length) return `<div class="ms-empty">No Library mods installed yet.</div>`;
    return `<div class="ms-ilist">${ids.map(installedRow).join("")}</div>`;
  }

  // ── manual single-file scan ──
  function manualResultHtml(r) {
    const scan = r.scan || {};
    return `<div class="ms-manual-result">
      <div class="ms-manual-result-top"><span class="ms-irow-name" title="${esc(r.name || r.file || "")}">${esc(r.name || r.file || "file")}</span>${verdictBadge(scan.verdict)}</div>
      ${findingsHtml(scan.findings)}
      ${vtLine(scan)}
    </div>`;
  }
  function manualScanHtml() {
    return `<div class="ms-manual">
      <div class="ms-manual-row">
        <input class="set-input ms-manual-input" id="msPath" type="text" placeholder="Paste a .fantome path" value="${esc(st.manualPath)}">
        <button class="btn sm primary" id="msScanPath" ${st.manualLoading ? "disabled" : ""}>${st.manualLoading ? "Scanning…" : "Scan file"}</button>
      </div>
      ${st.manualResult ? manualResultHtml(st.manualResult) : ""}
    </div>`;
  }

  function pageHtml() {
    if (st.installed === null) return `<div class="ms-wrap"><div class="muted">Loading ModScan…</div></div>`;
    return `<div class="ms-wrap">
      <div class="ms-head">
        <div><span class="section-label">MODSCAN</span><div class="ms-tag">Malware scanning for downloaded mods — structural checks plus VirusTotal reputation, on every install and on demand.</div></div>
        <button class="btn primary" id="msScanAll" ${st.scanningAll ? "disabled" : ""}>${st.scanningAll ? "Scanning…" : "Scan all mods"}</button>
      </div>
      ${scanAllResultHtml()}
      <div class="glass ms-card">
        <div class="ms-card-title">Installed mods</div>
        ${installedListHtml()}
      </div>
      <div class="glass ms-card">
        <div class="ms-card-title">Scan a file</div>
        ${manualScanHtml()}
      </div>
    </div>`;
  }

  function wire() {
    const all = document.getElementById("msScanAll"); if (all) all.onclick = scanAll;
    root.querySelectorAll("[data-rescan]").forEach((b) => (b.onclick = () => rescan(b.dataset.rescan)));
    const showClean = root.querySelector("[data-showclean]"); if (showClean) showClean.onclick = () => { st.showClean = !st.showClean; paint(); };
    const path = document.getElementById("msPath"); if (path) path.oninput = () => { st.manualPath = path.value; };
    const sc = document.getElementById("msScanPath"); if (sc) sc.onclick = scanPath;
  }

  function paint() {
    if (!root) return;
    root.innerHTML = pageHtml();
    wire();
  }

  window.renderModscan = async function (el) {
    root = el;
    if (st.installed === null) { paint(); await load(); }
    paint();
  };
})();
