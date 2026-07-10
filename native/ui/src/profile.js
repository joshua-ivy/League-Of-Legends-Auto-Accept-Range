// Profile screen — op.gg-style player profile fed by the LCU (get_profile).
// Self-contained; main.js routes the "profile" page here via window.renderProfile().
// Art is proxied from the client's own asset server via http://lcu.localhost/...
// (numeric ids; see the `lcu` URI scheme in lib.rs). In a plain browser those
// images 404 and fall back to a letter tile — layout still renders from mock.

(function () {
  const esc = window.ChudShared.esc;
  const inv = window.ChudShared.invoke;

  // LCU asset proxy URLs (numeric ids).
  const asset = (kind, id, ext = "png") => (id ? `http://lcu.localhost/${kind}/${id}.${ext}` : null);
  const champImg = (id) => asset("champion-icons", id);
  const ppIcon = (id) => asset("profile-icons", id, "jpg");
  // Items/spells have no /v1/{kind}/{id}.png endpoint; the backend resolves each
  // one's real iconPath. Serve it via the proxy, which accepts full asset paths.
  const assetPath = (p) => (p ? `http://lcu.localhost${p}` : null);

  const TIERS = {
    IRON: { c: "#7C6F68" }, BRONZE: { c: "#A9714B" }, SILVER: { c: "#9FB0BC" }, GOLD: { c: "#E0A44B" },
    PLATINUM: { c: "#3FB6A6" }, EMERALD: { c: "#2BD980" }, DIAMOND: { c: "#6AA0F0" }, MASTER: { c: "#B768E6" },
    GRANDMASTER: { c: "#E8556B" }, CHALLENGER: { c: "#62D0E8" }, UNRANKED: { c: "#5B5A56" },
  };
  const tierColor = (t) => (TIERS[t] || TIERS.UNRANKED).c;
  const cap = (s) => (s ? s.charAt(0) + s.slice(1).toLowerCase() : "");
  const wrColor = (wr) => (wr >= 50 ? "var(--teal)" : "var(--red)");

  // ── small components ──────────────────────────────────────────────────────
  function champSq(id, name, mastery) {
    const url = champImg(id);
    const letter = esc(String(name || id || "?")[0] || "?");
    return `<div class="champ-sq">
      ${url ? `<img src="${url}" alt="" onerror="this.style.display='none';this.parentElement.querySelector('.fallback').style.display='flex'">` : ""}
      <span class="fallback" style="display:${url ? "none" : "flex"}">${letter}</span>
      ${mastery ? `<span class="mast">M${mastery}</span>` : ""}
    </div>`;
  }
  function itemSlot(url, trinket) {
    return `<div class="islot${trinket ? " trinket" : ""}">${url ? `<img src="${url}" alt="" onerror="this.style.display='none'">` : ""}</div>`;
  }
  function ring(size, stroke, pct, color) {
    const r = (size - stroke) / 2, c = 2 * Math.PI * r;
    return `<svg width="${size}" height="${size}">
      <circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="var(--line-strong)" stroke-width="${stroke}"/>
      <circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="${color}" stroke-width="${stroke}" stroke-linecap="round" stroke-dasharray="${c}" stroke-dashoffset="${c * (1 - pct / 100)}" transform="rotate(-90 ${size / 2} ${size / 2})"/>
    </svg>`;
  }
  function emblem(tier, division, size) {
    const col = tierColor(tier), gid = "eg" + tier;
    return `<div class="emblem" style="width:${size}px;height:${size}px">
      <svg width="${size}" height="${size}" viewBox="0 0 72 72">
        <defs><linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${col}" stop-opacity="0.95"/><stop offset="1" stop-color="${col}" stop-opacity="0.55"/></linearGradient></defs>
        <polygon points="36,4 64,20 64,52 36,68 8,52 8,20" fill="url(#${gid})" stroke="#0a121a" stroke-width="2"/>
        <polygon points="36,4 64,20 64,52 36,68 8,52 8,20" fill="none" stroke="#F0E6D2" stroke-opacity="0.5" stroke-width="1.5"/>
        <polygon points="36,12 57,24 57,48 36,60 15,48 15,24" fill="none" stroke="#0a121a" stroke-opacity="0.35" stroke-width="1.4"/>
      </svg>
      ${division ? `<span class="etier">${esc(division)}</span>` : ""}
    </div>`;
  }
  const tick = `<svg viewBox="0 0 18 18" fill="none" stroke="var(--gold)" stroke-width="1.4"><path d="M1 1 L8 1 M1 1 L1 8"/></svg>`;

  // ── banner ──────────────────────────────────────────────────────────────--
  function banner(p) {
    const s = p.summoner, solo = (p.ranked || []).find((r) => r.queue === "RANKED_SOLO_5x5") || (p.ranked || [])[0];
    const xpPct = s.xpUntilNextLevel ? Math.round((s.xpSinceLastLevel / s.xpUntilNextLevel) * 100) : 0;
    const av = { online: ["var(--green)", "var(--green-fill)"], inGame: ["var(--amber)", "var(--amber-fill)"], away: ["var(--text-secondary)", "var(--bg-inset)"], offline: ["var(--text-secondary)", "var(--bg-inset)"] }[s.availability] || ["var(--text-secondary)", "var(--bg-inset)"];
    const T = solo ? tierColor(solo.tier) : tierColor("UNRANKED");
    return `<div class="forge profile-banner-wrap">
      <span class="fcorner tl">${tick}</span><span class="fcorner tr">${tick}</span><span class="fcorner bl">${tick}</span><span class="fcorner br">${tick}</span>
      <div class="pf-banner">
        <div class="pf-avatar">
          <div class="hexmask">${ppIcon(s.profileIconId) ? `<img src="${ppIcon(s.profileIconId)}" alt="" onerror="this.style.display='none'">` : ""}</div>
          <svg class="hexring" viewBox="0 0 110 110" fill="none"><polygon points="55,4 102,29 102,81 55,106 8,81 8,29" stroke="var(--gold)" stroke-width="2" opacity="0.85"/></svg>
          <span class="lvl">${esc(s.summonerLevel)}</span>
        </div>
        <div class="pf-id">
          <div class="pf-name"><span class="gn">${esc(s.gameName)}</span><span class="tag">#${esc(s.tagLine)}</span></div>
          <div class="pf-status-row">
            <span class="chip" style="color:${av[0]};background:${av[1]};border-color:${av[0]}66">
              <span class="slight on" style="width:6px;height:6px;background:${av[0]};color:${av[0]}"></span>${esc(s.statusMessage)}
            </span>
            ${solo && solo.hotStreak ? `<span class="hotstreak">▲ Hot streak</span>` : ""}
          </div>
          <div class="pf-xp">
            <span class="mono" style="font-size:10px;color:var(--text-muted);min-width:30px">Lv ${esc(s.summonerLevel)}</span>
            <div class="bar"><i style="width:${xpPct}%"></i></div>
            <span class="xptxt">${Number(s.xpSinceLastLevel).toLocaleString()} / ${Number(s.xpUntilNextLevel).toLocaleString()} XP</span>
          </div>
        </div>
        ${solo ? `<div class="pf-rank-emblem">
          ${emblem(solo.tier, solo.division, 76)}
          <div class="rank-meta">
            <span class="rk-tier" style="color:${T}">${esc(cap(solo.tier))} ${esc(solo.division)}</span>
            <span class="rk-lp">${esc(solo.lp)} LP · Ranked Solo/Duo</span>
            ${solo.percentile ? `<span class="rk-pct">${esc(solo.percentile)} of region</span>` : ""}
          </div>
        </div>` : ""}
      </div>
    </div>`;
  }

  function rankCard(p) {
    const rows = (p.ranked || []).map((q) => {
      const total = q.wins + q.losses, wr = total ? Math.round((q.wins / total) * 100) : 0, T = tierColor(q.tier);
      return `<div class="qrow">
        ${emblem(q.tier, q.division, 48)}
        <div>
          <div class="qname">${esc(q.label)}</div>
          <div class="qtier" style="color:${T}">${esc(cap(q.tier))} ${esc(q.division)}</div>
          <div class="qlp">${esc(q.lp)} LP</div>
          ${q.series ? `<div class="series-dots" style="margin-top:6px">${q.series.progress.split("").map((r) => `<i class="${r === "W" ? "w" : r === "L" ? "l" : ""}"></i>`).join("")}</div>` : ""}
        </div>
        <div class="qwl">
          <div class="wring">${ring(50, 5, wr, wrColor(wr))}<span class="wtxt">${wr}%</span></div>
          <div class="wl" style="margin-top:6px"><b class="w">${q.wins}W</b> <b class="l">${q.losses}L</b></div>
        </div>
      </div>`;
    }).join("") || `<div class="dim" style="padding:10px 0">No ranked games this season.</div>`;
    return `<div class="hx"><div class="pf-card-head"><span class="section-label">Ranked</span><span class="mono" style="font-size:10px;color:var(--text-muted)">Current season</span></div>${rows}</div>`;
  }

  function champPool(p) {
    const rows = (p.champPool || []).map((c) => {
      const wr = c.games ? Math.round((c.wins / c.games) * 100) : 0;
      const kda = ((c.k + c.a) / Math.max(1, c.d)).toFixed(2);
      return `<div class="champ-row">
        ${champSq(c.id, c.name, c.mastery)}
        <div class="champ-info"><div class="cn">${esc(c.name)}</div><div class="cm">${c.games} games · ${c.cs} cs/min</div></div>
        <div class="champ-stat"><div class="ckda">${kda} KDA</div><div class="cwr" style="color:${wrColor(wr)}">${wr}% · ${c.wins}W ${c.games - c.wins}L</div></div>
      </div>`;
    }).join("") || `<div class="dim" style="padding:10px 0">No recent games to summarize.</div>`;
    return `<div class="hx"><div class="pf-card-head"><span class="section-label">Champion Pool</span><span class="mono" style="font-size:10px;color:var(--text-muted)">Recent games</span></div>${rows}</div>`;
  }

  function perfSummary(p) {
    const pf = p.perf, wr = pf.games ? Math.round((pf.wins / pf.games) * 100) : 0;
    const kda = ((pf.k + pf.a) / Math.max(0.1, pf.d)).toFixed(2);
    const roles = (pf.roles || []).map((r) => `<div class="role-row"><span class="rl">${esc(r.label)}</span><div class="rbar"><i style="width:${r.pct}%"></i></div><span class="rpct">${r.pct}%</span></div>`).join("");
    return `<div class="hx"><div class="pf-card-head"><span class="section-label">Recent Performance</span><span class="mono" style="font-size:10px;color:var(--text-muted)">Last ${pf.games} games</span></div>
      <div class="perf-grid">
        <div class="perf-ring">${ring(128, 9, wr, wrColor(wr))}<div class="pr-center"><div><div class="pr-wr">${wr}%</div><div class="pr-wl">${pf.wins}W · ${pf.losses}L</div><div class="pr-cap">Win rate</div></div></div></div>
        <div class="col" style="gap:14px">
          <div class="perf-metrics">
            <div class="pm"><span class="pm-label">KDA</span><span class="pm-val gold">${kda}</span><span class="pm-sub kda-big"><span class="kk">${pf.k}</span> / <span class="dd">${pf.d}</span> / <span class="aa">${pf.a}</span> avg</span></div>
            <div class="pm"><span class="pm-label">Kill participation</span><span class="pm-val teal">${pf.killP}%</span><span class="pm-sub">team fight presence</span></div>
            <div class="pm"><span class="pm-label">CS / min</span><span class="pm-val">${pf.csMin}</span><span class="pm-sub">farm efficiency</span></div>
            <div class="pm"><span class="pm-label">Vision score</span><span class="pm-val">${pf.vision}</span><span class="pm-sub">avg per game</span></div>
          </div>
          <div class="roles">${roles}</div>
        </div>
      </div></div>`;
  }

  function scoreboard(team, side) {
    return `<div class="team-col ${side}">
      <div class="team-head">${side === "ally" ? "Allies" : "Enemies"}</div>
      ${(team || []).map((pl) => `<div class="sb-row${pl.isMe ? " me" : ""}">${champSq(pl.champ, pl.champName)}<span class="sb-name">${esc(pl.name)}</span><span class="sb-kda">${pl.k}/${pl.d}/${pl.a}</span></div>`).join("")}
    </div>`;
  }

  function matchRow(m) {
    const kda = ((m.k + m.a) / Math.max(1, m.d)).toFixed(2);
    const icons = m.itemIcons || [];
    const items6 = [0, 1, 2, 3, 4, 5].map((n) => itemSlot(assetPath(icons[n]))).join("");
    const trinket = itemSlot(assetPath(icons[6]), true);
    const spellIcons = m.spellIcons || [];
    const spells = (m.spells || []).map((sp, n) => { const u = assetPath(spellIcons[n]); return `<span class="sp">${u ? `<img src="${u}" alt="" onerror="this.style.display='none'">` : ""}</span>`; }).join("");
    const lp = m.lpDelta == null ? "" : `<span class="rlp ${m.lpDelta >= 0 ? "up" : "dn"}">${m.lpDelta >= 0 ? "+" : ""}${m.lpDelta} LP</span>`;
    return `<div class="match ${m.result}" data-mid="${m.id}">
      <div class="match-main">
        <div class="m-result ${m.result}"><span class="rlabel">${m.result === "win" ? "Victory" : "Defeat"}</span><span class="rqueue">${esc(m.queueShort)}</span><span class="rtime">${esc(m.length)} · ${esc(m.timeAgo)}</span>${lp}</div>
        <div class="m-champ">${champSq(m.champ, m.champName)}<span class="clvl">${m.lvl}</span><div class="spells">${spells}</div></div>
        <div class="m-kda"><div class="kline"><span class="kd-k">${m.k}</span><span class="sl">/</span><span class="kd-d">${m.d}</span><span class="sl">/</span><span class="kd-a">${m.a}</span></div><div class="kratio"><span style="color:var(--text-secondary)">${kda} KDA</span>${m.mvp ? `<span class="mvp">MVP</span>` : ""}</div></div>
        <div class="m-stats"><div><b>${m.cs}</b> CS (${m.csMin})</div><div><b>${m.killP}%</b> KP</div><div><b>${(m.dmg / 1000).toFixed(1)}k</b> dmg · ${m.vision} vis</div></div>
        <div class="m-items">${items6}${trinket}</div>
        <div class="m-expand"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg></div>
      </div>
      <div class="m-detail">${scoreboard(m.team && m.team.ally, "ally")}${scoreboard(m.team && m.team.enemy, "enemy")}</div>
    </div>`;
  }

  let filter = "all";
  function matchListHtml(p) {
    const all = p.matches || [];
    const wins = all.filter((m) => m.result === "win").length;
    const shown = all.filter((m) => (filter === "all" ? true : filter === "win" ? m.result === "win" : m.result === "loss"));
    const tab = (id, label, n) => `<button class="tab${filter === id ? " on" : ""}" data-filter="${id}">${label} <span class="tab-n">${n}</span></button>`;
    return `<div class="col" style="gap:12px">
      <div class="pf-filters"><span class="section-label">Match History</span>${tab("all", "All", all.length)}${tab("win", "Wins", wins)}${tab("loss", "Losses", all.length - wins)}<span class="spacer"></span><span class="mono" style="font-size:10.5px;color:var(--text-muted)">click a match to expand</span></div>
      <div class="match-list">${shown.map(matchRow).join("") || `<div class="dim" style="padding:14px">No matches found.</div>`}</div>
    </div>`;
  }

  function profileHtml(p) {
    return `<div class="content-inner profile fade-in">
      <div class="pf-sync"><span class="slight on" style="width:7px;height:7px;background:var(--teal);color:var(--teal)"></span>Synced from League Client<span class="dotsep">·</span><span class="endp">${esc(p.endpoint || "/lol-summoner/v1/current-summoner")}</span><span class="dotsep">·</span>${esc(p.summoner.syncedAgo || "now")} ago<span class="refresh" id="pfRefresh"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 12a8 8 0 1 0-2.6 5.9"/><path d="M20 6v5h-5"/></svg>Refresh</span></div>
      ${banner(p)}
      <div class="pf-layout">
        <div class="pf-col">${rankCard(p)}${champPool(p)}</div>
        <div class="pf-col">${perfSummary(p)}<div id="pfMatches">${matchListHtml(p)}</div></div>
      </div>
    </div>`;
  }

  function emptyState() {
    return `<div class="content-inner profile fade-in"><div class="hx" style="margin-top:14px;text-align:center;padding:48px 24px">
      <div style="font-family:var(--font-display);font-size:18px;color:var(--gold-bright)">No profile yet</div>
      <div class="dim" style="margin-top:6px">Start the League client and log in — your summoner, rank, and match history will appear here.</div>
      <div style="margin-top:16px"><span class="refresh" id="pfEmptyRefresh" style="display:inline-flex;align-items:center;gap:6px;cursor:pointer;color:var(--text-secondary);padding:5px 11px;border-radius:var(--r-sm);border:1px solid var(--glass-border)"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 12a8 8 0 1 0-2.6 5.9"/><path d="M20 6v5h-5"/></svg>Check again</span></div>
    </div></div>`;
  }

  let current = null;
  function wire(el, p) {
    const ref = el.querySelector("#pfRefresh");
    if (ref) ref.onclick = () => render(el, true);
    el.querySelectorAll(".match-main").forEach((mm) => (mm.onclick = () => mm.parentElement.classList.toggle("open")));
    el.querySelectorAll(".pf-filters .tab").forEach((t) => (t.onclick = () => {
      filter = t.dataset.filter;
      const box = el.querySelector("#pfMatches");
      if (box && current) { box.innerHTML = matchListHtml(current); wire(el, current); }
    }));
  }

  async function render(el, force) {
    if (!current || force || current.clientOnline === false) {
      const data = await inv("get_profile");
      // Real backend: an offline/failed fetch stays an offline state (retried
      // on every visit); mock data is for the browser preview only.
      current = data || (window.ChudShared.hasBackend ? { clientOnline: false } : MOCK_PROFILE);
    }
    if (!current || current.clientOnline === false) {
      el.innerHTML = emptyState();
      const r = el.querySelector("#pfEmptyRefresh");
      if (r) r.onclick = () => render(el, true);
      return;
    }
    el.innerHTML = profileHtml(current);
    wire(el, current);
  }

  // Compact mock so the screen renders in a plain browser (no Tauri/live client).
  const MOCK_PROFILE = {
    clientOnline: true, endpoint: "/lol-summoner/v1/current-summoner",
    summoner: { gameName: "Andi", tagLine: "NA1", summonerLevel: 312, profileIconId: 5212, xpSinceLastLevel: 1840, xpUntilNextLevel: 2640, availability: "inGame", statusMessage: "In Game · Ranked Solo/Duo", syncedAgo: "2s" },
    ranked: [
      { queue: "RANKED_SOLO_5x5", label: "Ranked Solo/Duo", tier: "GOLD", division: "II", lp: 67, wins: 142, losses: 121, hotStreak: true, series: null, percentile: "Top 14%" },
      { queue: "RANKED_FLEX_SR", label: "Ranked Flex", tier: "SILVER", division: "I", lp: 34, wins: 48, losses: 51, hotStreak: false, series: { progress: "WLN" }, percentile: "" },
    ],
    champPool: [
      { id: 202, name: "Jhin", games: 64, wins: 38, k: 7.1, d: 4.2, a: 9.3, cs: 7.8, mastery: 7 },
      { id: 51, name: "Caitlyn", games: 41, wins: 24, k: 6.4, d: 4.8, a: 7.1, cs: 8.3, mastery: 7 },
      { id: 145, name: "Kai'Sa", games: 33, wins: 17, k: 8.2, d: 5.1, a: 8.0, cs: 7.6, mastery: 6 },
    ],
    perf: { games: 20, wins: 13, losses: 7, k: 7.2, d: 4.7, a: 8.6, killP: 58, csMin: 7.8, vision: 24, dmgMin: 712, roles: [{ label: "ADC", pct: 80 }, { label: "Mid", pct: 12 }, { label: "Support", pct: 8 }] },
    matches: [
      { id: 1, result: "win", queue: "Ranked Solo/Duo", queueShort: "Solo/Duo", champ: 202, champName: "Jhin", role: "ADC", lvl: 16, lpDelta: null, timeAgo: "14m ago", length: "31:24", k: 11, d: 3, a: 14, cs: 248, csMin: 7.9, killP: 64, dmg: 28400, vision: 22, spells: [4, 7], items: [3031, 3094, 3006, 3036, 3072, 3046, 3340], mvp: true, team: { ally: [{ champ: 202, champName: "Jhin", name: "Andi", isMe: true, k: 11, d: 3, a: 14 }, { champ: 412, champName: "Thresh", name: "OnlyVisupport", k: 1, d: 5, a: 21 }], enemy: [{ champ: 51, champName: "Caitlyn", name: "adcdiff", k: 6, d: 8, a: 5 }] } },
      { id: 2, result: "loss", queue: "Ranked Solo/Duo", queueShort: "Solo/Duo", champ: 51, champName: "Caitlyn", role: "ADC", lvl: 14, lpDelta: null, timeAgo: "2h ago", length: "34:48", k: 5, d: 9, a: 6, cs: 263, csMin: 7.6, killP: 41, dmg: 21800, vision: 17, spells: [4, 7], items: [6672, 3006, 3031, 1038, 0, 0, 3363], mvp: false, team: { ally: [{ champ: 51, champName: "Caitlyn", name: "Andi", isMe: true, k: 5, d: 9, a: 6 }], enemy: [{ champ: 222, champName: "Jinx", name: "getexcited", k: 12, d: 3, a: 9 }] } },
    ],
  };

  window.renderProfile = (el) => render(el, false);
})();
