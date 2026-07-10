// Shared UI helpers. `esc` is the app's sole XSS defense for LCU/opponent-
// controlled strings — single copy here so a fix can never drift between pages.
window.ChudShared = (() => {
  const TAURI = window.__TAURI__?.core;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  // Returns null on missing backend (browser preview) AND on a real invoke
  // failure — callers that must distinguish check `hasBackend`.
  const invoke = async (cmd, args) => {
    if (!TAURI) return null;
    try { return await TAURI.invoke(cmd, args); }
    catch (e) { console.warn(cmd, e); return null; }
  };
  return { esc, invoke, hasBackend: !!TAURI };
})();
