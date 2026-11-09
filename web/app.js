(() => {
  'use strict';

  const SPINNER = '<div class="empty">Loading live world…</div>';
  const DEFAULT_API = 'http://127.0.0.1:8080';

  function storedApi() {
    const saved = localStorage.getItem('fm-web-api-base');
    if (saved && !['http://127.0.0.1:8080', 'http://localhost:8080'].includes(saved.trim())) return saved.trim().replace(/\/$/, '');
    return WINDOW_API_BASE();
  }
  function WINDOW_API_BASE() {
    const q = new URLSearchParams(location.search).get('api');
    if (q) return q.replace(/\/$/, '');
    const cfg = window.FM_API_BASE;
    if (cfg) return cfg.replace(/\/$/, '');
    return location.origin && location.origin !== 'null' ? location.origin : DEFAULT_API;
  }
  let API = storedApi();
  let state = {
    overview: null, leagues: [], fixtures: null, players: { players: [], total: 0 },
    page: 'home', mode: localStorage.getItem('fm-mode') || 'simulate',
    leagueIndex: 0, leagueTab: 'table', leagueData: null,
    matchId: 0, matchTab: 'facts', matchData: null, matchLeagueData: null,
    playerHandle: '', playerData: null,
    playerQuery: '', sort: 'goals', scope: 'all', matchQuery: '',
    homeWeek: 0, homeMatches: [], homeFixtures: [],
    managerTeam: Number(localStorage.getItem('fm-manager-team') || 0), manager: null, managerClubs: [],
    clockDays: 0, lastNewsWeek: -1, news: [], newsIndex: 0,
    auto: { running: false, inFlight: false, timer: null, speed: 'week', dayOffset: 0 }
  };

  const $ = (id) => document.getElementById(id);
  const fmt = (n) => new Intl.NumberFormat().format(n ?? 0);
  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const number = (value) => Number(value ?? 0).toLocaleString(undefined, { maximumFractionDigits: 2 });
  const hash = (s) => { let h = 0; for (const ch of String(s)) h = (h * 31 + ch.codePointAt(0)) >>> 0; return h; };
  const initials = (name) => String(name || '?').trim().split(/\s+/).slice(0, 2).map((x) => x[0]).join('').toUpperCase();
  const hue = (name) => hash(name) % 360;
  const crest = (name, size = 26) => `<span class="crest" style="width:${size}px;height:${size}px;background:hsl(${hue(name)} 52% 68%)">${esc(initials(name))}</span>`;
  const ratingClass = (r) => r >= 7.5 ? 'green' : r >= 6.5 ? 'gold' : 'red';

  function setStatus(text, ok = true) {
    const el = $('api-status');
    if (!el) return;
    el.textContent = text;
    el.style.color = ok ? 'var(--green)' : 'var(--red)';
  }
  function toast(message, error = false) {
    const el = $('toast');
    el.textContent = message;
    el.className = error ? 'error show' : 'show';
    clearTimeout(el._timer);
    el._timer = setTimeout(() => el.className = '', 3400);
  }
  async function api(path, options = {}) {
    const res = await fetch(`${API}${path}`, {
      method: options.method || 'GET',
      headers: { 'Content-Type': 'application/json' },
      body: options.body ? JSON.stringify(options.body) : undefined,
      signal: AbortSignal.timeout(options.timeout || 180000),
    });
    const text = await res.text();
    let json;
    try { json = JSON.parse(text); } catch { json = { raw: text }; }
    if (!res.ok) throw new Error(json.error || json.raw || `HTTP ${res.status}`);
    return json;
  }

  function seasonDate(days) { const d = new Date(2026, 7, 1); d.setDate(d.getDate() + (days || 0)); return d; }
  function fmtDate(days) { return new Intl.DateTimeFormat('en-GB', { weekday: 'short', day: '2-digit', month: 'short', year: 'numeric' }).format(seasonDate(days)); }
  function clockTime(days) { const hour = 9 + ((days * 37 + state.auto.dayOffset * 11) % 7); return `${String(hour).padStart(2, '0')}:00`; }
  function updateClock() {
    const w = state.overview?.world || {};
    const week = w.week ?? 0, total = w.total_rounds ?? 38;
    state.clockDays = Math.max(state.clockDays || 0, week * 7);
    const d = seasonDate(state.clockDays);
    const setText = (id, text) => { const el = $(id); if (el) el.textContent = text; };
    setText('sim-year', String(d.getFullYear()));
    setText('sim-date', fmtDate(state.clockDays));
    setText('sim-week', `${week} / ${total}`);
    setText('sim-clock', clockTime(state.clockDays));
    setText('world-week', `Week ${week}`);
    setText('world-meta', `${fmtDate(state.clockDays)} · ${fmt(w.matches_played ?? 0)} matches played`);
  }
  function addNews(text) {
    if (!text || state.news.some((n) => n.text === text)) return;
    state.news.unshift({ date: fmtDate(state.clockDays), text });
    state.news = state.news.slice(0, 24);
    state.newsIndex = 0;
    renderNewsRail();
  }
  function renderNewsRail() {
    const list = $('news-list');
    if (!list) return;
    const nd = $('news-date');
    if (nd) nd.textContent = fmtDate(state.clockDays);
    if (!state.news.length) {
      list.innerHTML = '<div class="news-item"><div class="news-thumb">CA</div><div><strong>Opening weekend pending</strong><span>Simulate a round to populate the wire.</span></div></div>';
      return;
    }
    list.innerHTML = state.news.slice(0, 5).map((n) => `<div class="news-item"><div class="news-thumb" style="background:hsl(${hue(n.text)} 38% 28%)">${esc(initials(n.text.match(/^[A-Z][a-z]+/)?.[0] || 'CA'))}</div><div><strong>${esc(n.text)}</strong><span>${esc(n.date)} · Crouton Alpha wire</span></div></div>`).join('');
  }
  function appendNews(w) {
    const week = w?.week ?? 0;
    if (week === state.lastNewsWeek) return;
    state.lastNewsWeek = week;
    if (week === 0) { addNews('The 2026 season is ready: 81 leagues and 655 clubs waiting for the opening round.'); return; }
    const top = state.overview?.top_scorers?.[0];
    if (top) addNews(`${top.name} leads the golden race in ${top.league || 'the world'} with ${top.goals} goals.`);
    (state.overview?.recent_matches || []).slice(0, 4).forEach((m) => addNews(`${m.home} ${m.home_goals}–${m.away_goals} ${m.away} · ${m.league}`));
    (state.leagues || []).slice(0, 3).forEach((l) => { if (l.leader && l.leader !== '—') addNews(`${l.name}: ${l.leader} lead the table.`); });
  }
  setInterval(() => { if (state.news.length) { state.newsIndex = (state.newsIndex + 1) % state.news.length; renderNewsRail(); } }, 4200);

  function renderRails() {
    const tl = $('top-leagues');
    if (tl) {
      const picked = state.leagues.slice(0, 9);
      tl.innerHTML = picked.map((l) => `<button data-league="${l.id}"><span class="league-dot" style="background:hsl(${hue(l.name)} 55% 68%)">${esc(initials(l.name))}</span><span>${esc(l.name)}</span></button>`).join('');
    }
    updateClock();
    renderNewsRail();
  }

  function groupedMatches(matches) {
    const map = new Map();
    for (const m of matches || []) {
      if (!map.has(m.league_idx)) map.set(m.league_idx, { name: m.league, items: [] });
      map.get(m.league_idx).items.push(m);
    }
    return [...map.entries()].sort((a, b) => a[0] - b[0]);
  }
  function matchRow(m) {
    return `<button class="match-row" data-match="${m.id}">
      <span class="status">FT</span>
      <span class="home"><span>${esc(m.home)}</span>${crest(m.home)}</span>
      <span class="score">${m.home_goals} - ${m.away_goals}</span>
      <span class="away">${crest(m.away)}<span>${esc(m.away)}</span></span>
    </button>`;
  }
  function fixtureRow(f) {
    return `<div class="match-row" style="cursor:default">
      <span class="status">vs</span>
      <span class="home"><span>${esc(f.home)}</span>${crest(f.home)}</span>
      <span class="score">–</span>
      <span class="away">${crest(f.away)}<span>${esc(f.away)}</span></span>
    </div>`;
  }

  async function renderHome() {
    const w = state.overview?.world || {};
    const week = state.homeWeek || w.week || 0;
    let html = `<div class="page-head">
      <div><span class="pill">${week ? 'Latest results' : 'Upcoming fixtures'}</span><h1 style="margin:10px 0 4px">${esc(fmtDate(week * 7))}</h1><p class="small" style="margin:0">${week ? `Round ${week} across all leagues.` : 'Round 1 is scheduled. Press Play or Step week.'}</p></div>
      <div class="date-nav"><button id="date-prev" aria-label="Previous week">‹</button><strong>${esc(fmtDate(week * 7))}</strong><button id="date-next" aria-label="Next week">›</button></div>
    </div>`;
    if (week === 0) {
      const fixtures = state.fixtures?.fixtures || [];
      const groups = groupedMatches(fixtures.map((f, i) => ({ ...f, id: `f${i}`, home_goals: null, away_goals: null })));
      for (const [, g] of groups.slice(0, 12)) {
        html += `<div class="league-group"><button class="league-group-head" data-league="${g.items[0].league_idx}"><span class="league-dot" style="background:hsl(${hue(g.name)} 55% 68%)">${esc(initials(g.name))}</span><strong>${esc(g.name)}</strong><span class="chev">›</span></button>${g.items.map(fixtureRow).join('')}</div>`;
      }
      if (!fixtures.length) html += `<div class="empty">No fixtures left this season.</div>`;
    } else {
      if (!state.homeMatches.length || state.homeMatchesWeek !== week) {
        try { const data = await api(`/api/matches?week=${week}`); state.homeMatches = data.matches || []; state.homeMatchesWeek = week; } catch { state.homeMatches = state.overview?.recent_matches || []; }
      }
      const groups = groupedMatches(state.homeMatches);
      for (const [, g] of groups) {
        html += `<div class="league-group"><button class="league-group-head" data-league="${g.items[0].league_idx}"><span class="league-dot" style="background:hsl(${hue(g.name)} 55% 68%)">${esc(initials(g.name))}</span><strong>${esc(g.name)}</strong><span class="chev">›</span></button>${g.items.map(matchRow).join('')}</div>`;
      }
      if (!state.homeMatches.length) html += `<div class="empty">No matches recorded for this week yet.</div>`;
    }
    $('app').innerHTML = html;
    $('date-prev')?.addEventListener('click', () => { state.homeWeek = Math.max(0, (state.homeWeek || w.week || 0) - 1); renderHome(); });
    $('date-next')?.addEventListener('click', () => { state.homeWeek = Math.min(w.total_rounds ?? 38, (state.homeWeek || w.week || 0) + 1); renderHome(); });
  }

  function renderLeagues() {
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Leagues</h1><p class="small" style="margin:4px 0 0">Open any competition to see the live table, fixtures, matches and player stats.</p></div><span class="pill">${state.leagues.length} competitions</span></div>
    <div class="grid cols-4">${state.leagues.map((l) => `<button class="card league-card" data-league="${l.id}"><div class="pill">${esc(l.confederation || '')}</div><h3 style="margin-top:10px">${esc(l.name)}</h3><p>${l.teams} clubs · ${l.rounds} rounds<br>Leader: <strong>${esc(l.leader || '—')}</strong></p></button>`).join('')}</div>`;
  }

  function leagueTable(table) {
    if (!table?.length) return '<div class="empty">No table rows yet.</div>';
    return `<div class="table-wrap"><table><thead><tr><th>#</th><th>Club</th><th>P</th><th>W</th><th>D</th><th>L</th><th>GF</th><th>GA</th><th>GD</th><th>Pts</th></tr></thead><tbody>
      ${table.map((r) => `<tr><td class="numeric">${r.position}</td><td><span style="display:flex;gap:8px;align-items:center">${crest(r.name, 22)}<strong>${esc(r.name)}</strong></span><div class="small">${esc(r.nation)}</div></td><td class="numeric">${r.table.played}</td><td class="numeric">${r.table.won}</td><td class="numeric">${r.table.drawn}</td><td class="numeric">${r.table.lost}</td><td class="numeric">${r.table.gf}</td><td class="numeric">${r.table.ga}</td><td class="numeric">${r.table.gd}</td><td class="numeric"><strong>${r.table.pts}</strong></td></tr>`).join('')}
    </tbody></table></div>`;
  }

  async function renderLeagueDetail(index = state.leagueIndex) {
    state.leagueIndex = index;
    state.page = 'league';
    $('app').innerHTML = SPINNER;
    const data = await api(`/api/league/${index}`);
    state.leagueData = data;
    const fixtures = (state.fixtures?.fixtures || []).filter((f) => f.league_idx === index);
    const tabs = [['table','Table'],['fixtures','Fixtures'],['matches','Matches'],['players','Player stats'],['stats','Team stats']];
    let body = '';
    if (state.leagueTab === 'table') body = leagueTable(data.table || []);
    else if (state.leagueTab === 'fixtures') body = fixtures.length ? fixtures.map(fixtureRow).join('') : '<div class="empty">No fixtures remain for this league.</div>';
    else if (state.leagueTab === 'matches') {
      try { const md = await api(`/api/matches?league=${index}`); body = (md.matches || []).length ? md.matches.map(matchRow).join('') : '<div class="empty">No played matches yet.</div>'; } catch { body = '<div class="empty">Match history unavailable.</div>'; }
    } else if (state.leagueTab === 'players') {
      body = (data.scorers || []).length ? data.scorers.map((p) => `<div class="row"><span><button class="linklike" data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)} · ${p.apps} apps</div></span><strong>${p.goals} G · ${p.assists} A</strong></div>`).join('') : '<div class="empty">No player data yet.</div>';
    } else {
      body = `<div class="grid cols-3">
        <div class="card"><h3>Top rated</h3>${(await api(`/api/players?scope=world&sort=rating&league=${encodeURIComponent(data.league || '')}&limit=5`).catch(() => ({players:[]}))).players.map((p) => `<div class="row"><span><button class="linklike" data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)}</div></span><span class="badge green">${number(p.rating)}</span></div>`).join('') || '<p>No rated players yet.</p>'}</div>
        <div class="card"><h3>Top scorers</h3>${(data.scorers || []).slice(0,5).map((p) => `<div class="row"><span><button class="linklike" data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)}</div></span><span class="badge">${p.goals}</span></div>`).join('') || '<p>No scorers yet.</p>'}</div>
        <div class="card"><h3>Most assists</h3>${(await api(`/api/players?scope=world&sort=assists&league=${encodeURIComponent(data.league || '')}&limit=5`).catch(() => ({players:[]}))).players.map((p) => `<div class="row"><span><button class="linklike" data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)}</div></span><span class="badge">${p.assists}</span></div>`).join('') || '<p>No assists yet.</p>'}</div>
      </div>`;
    }
    $('app').innerHTML = `<div class="page-head"><div><button class="back-btn" data-page="leagues">‹ Leagues</button><div class="pill" style="margin-top:10px">${esc(data.confederation || '')} · ${esc(data.league || '')}</div><h1 style="margin:8px 0 4px">${esc(data.league || 'League')}</h1><p class="small" style="margin:0">${data.week || 0} weeks played · ${esc(fmtDate((data.week || 0) * 7))}</p></div><div style="display:flex;gap:8px"><button id="follow-league">Follow</button><button id="sync-calendar" class="secondary">Sync to calendar</button></div></div>
    <div class="tabs">${tabs.map(([id, label]) => `<button data-league-tab="${id}" class="${state.leagueTab === id ? 'active' : ''}">${label}</button>`).join('')}</div>${body}`;
    $('follow-league')?.addEventListener('click', () => toast('League added to your followed list.'));
    $('sync-calendar')?.addEventListener('click', () => toast('Calendar sync prepared for this league.'));
  }

  function renderFixtures() {
    const fixtures = state.fixtures?.fixtures || [];
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Fixtures</h1><p class="small" style="margin:4px 0 0">Next round-robin slate across all leagues.</p></div><span class="pill">Week ${state.fixtures?.week ?? (state.overview?.world?.week ?? 0) + 1}</span></div>
    <div class="card"><div class="toolbar"><input id="fixture-league-filter" aria-label="Filter fixtures by league" placeholder="Filter league"/><button id="load-fixtures">Apply</button></div>
    ${fixtures.length ? fixtures.map(fixtureRow).join('') : '<div class="empty">No fixtures left this season.</div>'}</div>`;
    $('load-fixtures')?.addEventListener('click', () => {
      const q = $('fixture-league-filter').value.toLowerCase();
      document.querySelectorAll('.match-row').forEach((el) => { el.style.display = !q || el.textContent.toLowerCase().includes(q) ? 'grid' : 'none'; });
    });
  }

  function renderMatches() {
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Match centre</h1><p class="small" style="margin:4px 0 0">Every played result, with deep post-match microdata.</p></div></div>
    <div class="card"><div class="toolbar">
      <select id="match-week" aria-label="Match week"><option value="">All weeks</option>${Array.from({ length: Math.max(1, state.overview?.world?.week || 1) }, (_, i) => `<option value="${i + 1}" ${i + 1 === (state.overview?.world?.week || 0) ? 'selected' : ''}>Week ${i + 1}</option>`).join('')}</select>
      <input id="match-q" aria-label="Search matches" placeholder="Search club or competition…"/>
      <button id="load-matches">Load matches</button>
      <button id="load-current-week" class="secondary">Show current week</button>
    </div><div id="match-results">${state.matchesList?.length ? state.matchesList.map(matchRow).join('') : '<div class="empty">Load a week to inspect match detail.</div>'}</div></div>`;
    $('load-matches')?.addEventListener('click', loadMatches);
    $('load-current-week')?.addEventListener('click', () => { $('match-week').value = String(state.overview?.world?.week || ''); loadMatches(); });
  }

  async function loadMatches() {
    const week = $('match-week')?.value || '';
    const q = ($('match-q')?.value || '').toLowerCase();
    const path = week ? `/api/matches?week=${encodeURIComponent(week)}` : '/api/matches';
    const data = await api(path);
    state.matchesList = (data.matches || []).filter((m) => !q || `${m.home} ${m.away} ${m.league}`.toLowerCase().includes(q));
    const el = $('match-results');
    if (el) el.innerHTML = state.matchesList.length ? state.matchesList.map(matchRow).join('') : '<div class="empty">No matches match.</div>';
  }

  function statBar(label, a, b, aLabel = '', bLabel = '') { return `<div class="stat-bar"><div class="nums"><span>${aLabel || number(a)}</span><span>${label}</span><span>${bLabel || number(b)}</span></div><div class="bar-track"><i style="width:${Math.max(0, Math.min(100, a))}%"></i><i style="width:${Math.max(0, Math.min(100, b))}%"></i></div></div>`; }
  function eventRows(m) {
    const rows = (m.events || []).map((e) => `<div class="event"><span class="minute">${e.minute}'</span><span><strong>${esc(e.player)}${e.assist ? ` <span class="small">assist ${esc(e.assist)}</span>` : ''}</strong><span>${esc(e.type)} · ${e.side === 'home' ? esc(m.match.home) : esc(m.match.away)}</span></span></div>`);
    return rows.join('') || '<div class="empty">No goal/event notes recorded.</div>';
  }
  function pitchSide(stats, homeSide) {
    const order = { GK: 0, DEF: 1, MID: 2, FWD: 3 };
    const cols = { GK: homeSide ? 10 : 90, DEF: homeSide ? 26 : 74, MID: homeSide ? 52 : 48, FWD: homeSide ? 78 : 22 };
    const byPos = new Map();
    for (const s of stats) { if (!byPos.has(s.position)) byPos.set(s.position, []); byPos.get(s.position).push(s); }
    return `<div class="pitch"><span class="circle"></span>${Object.entries(cols).map(([pos, x]) => {
      const players = (byPos.get(pos) || []).sort((a, b) => a.rating - b.rating);
      const n = players.length;
      return players.map((s, i) => {
        const y = n <= 1 ? 50 : 16 + (i / (n - 1)) * 68;
        return `<div class="pnode" style="left:${x}%;top:${y}%"><span class="rating" style="background:${s.rating >= 7.5 ? '#0d5c35' : s.rating >= 6.5 ? '#5a481b' : '#5c2525'}">${number(s.rating)}</span><span class="ball">${esc(initials(s.name))}</span><span class="name">${esc(s.name)}</span></div>`;
      }).join('');
    }).join('')}</div>`;
  }

  async function renderMatch(id = state.matchId) {
    state.matchId = id;
    state.page = 'match';
    $('app').innerHTML = SPINNER;
    const m = await api(`/api/match/${id}`);
    state.matchData = m;
    const mm = m.match;
    const scorers = (e) => (m.events || []).filter((x) => x.side === e && x.type === 'goal').map((x) => `${x.player} ${x.minute}'`).join('<br>');
    const tabs = [['facts','Facts'],['events','Events'],['lineup','Lineup'],['stats','Stats'],['table','Table']];
    let body = '';
    if (state.matchTab === 'facts') {
      body = `<div class="grid cols-2">
        <div class="card"><h3>Match facts</h3><div class="row-list">
          <div class="row"><span>Date</span><strong>${esc(fmtDate((mm.week || 1) * 7))}</strong></div>
          <div class="row"><span>Round</span><strong>${mm.round}</strong></div>
          <div class="row"><span>Expected goals</span><strong>${number(mm.xg?.[0])} – ${number(mm.xg?.[1])}</strong></div>
          <div class="row"><span>Shots</span><strong>${mm.shots?.join(' – ')}</strong></div>
          <div class="row"><span>Possession</span><strong>${mm.possession}% home</strong></div>
          <div class="row"><span>Corners</span><strong>${mm.corners?.join(' – ')}</strong></div>
        </div></div>
        <div class="card"><h3>Top stats</h3><div class="stat-bars">${statBar('Ball possession', mm.possession, 100 - mm.possession)}${statBar('Expected goals (xG)', mm.xg?.[0] * 50, mm.xg?.[1] * 50)}${statBar('Total shots', mm.shots?.[0] * 4, mm.shots?.[1] * 4)}</div></div>
      </div>`;
    } else if (state.matchTab === 'events') body = `<div class="card"><h2>Events</h2><div class="events">${eventRows(m)}</div></div>`;
    else if (state.matchTab === 'lineup') body = `<div class="grid cols-2"><div class="card"><h3>${esc(mm.home)} · ${esc(mm.home)}</h3>${pitchSide(m.home_lineup || [], true)}</div><div class="card"><h3>${esc(mm.away)} · ${esc(mm.away)}</h3>${pitchSide(m.away_lineup || [], false)}</div></div>`;
    else if (state.matchTab === 'stats') {
      const rows = (stats) => (stats || []).map((s) => `<tr><td><strong>${esc(s.name)}</strong><div class="small">${esc(s.nation)} · ${esc(s.position)}</div></td><td class="numeric">${number(s.rating)}</td><td class="numeric">${s.minutes}</td><td class="numeric">${s.goals}</td><td class="numeric">${s.assists}</td><td class="numeric">${s.shots}</td><td class="numeric">${number(s.xg)}</td><td class="numeric">${s.key_passes}</td><td class="numeric">${esc(s.passes)}</td><td class="numeric">${esc(s.dribbles)}</td><td class="numeric">${s.tackles}</td><td class="numeric">${s.saves}</td></tr>`).join('');
      body = `<div class="grid cols-2"><div class="card"><h3>${esc(mm.home)} stat lines</h3><div class="table-wrap"><table><thead><tr><th>Player</th><th>Rtg</th><th>Min</th><th>G</th><th>A</th><th>Sh</th><th>xG</th><th>KP</th><th>Passes</th><th>Dribbles</th><th>Tkl</th><th>Saves</th></tr></thead><tbody>${rows(m.home_lineup)}</tbody></table></div></div><div class="card"><h3>${esc(mm.away)} stat lines</h3><div class="table-wrap"><table><thead><tr><th>Player</th><th>Rtg</th><th>Min</th><th>G</th><th>A</th><th>Sh</th><th>xG</th><th>KP</th><th>Passes</th><th>Dribbles</th><th>Tkl</th><th>Saves</th></tr></thead><tbody>${rows(m.away_lineup)}</tbody></table></div></div></div>`;
    } else {
      if (!state.matchLeagueData) { try { state.matchLeagueData = await api(`/api/league/${mm.league_idx}`); } catch { state.matchLeagueData = { table: [] }; } }
      body = leagueTable(state.matchLeagueData.table || []);
    }
    $('app').innerHTML = `<div class="page-head"><div><button class="back-btn" data-page="matches">‹ Matches</button><div class="pill" style="margin-top:10px">${esc(mm.league)} · Round ${mm.round}</div><h1 style="margin:8px 0 4px">${esc(mm.home)} ${mm.home_goals}–${mm.away_goals} ${esc(mm.away)}</h1><p class="small" style="margin:0">${esc(fmtDate((mm.week || 1) * 7))} · Simulated stadium</p></div><button id="follow-match" class="secondary">Follow</button></div>
    <div class="card match-hero"><div class="score-line"><div class="team-name home">${esc(mm.home)} ${crest(mm.home, 34)}</div><div class="big-score">${mm.home_goals} - ${mm.away_goals}<div class="small" style="font-size:12px;font-weight:650">Full time</div></div><div class="team-name away">${crest(mm.away, 34)} ${esc(mm.away)}</div></div><div class="scorers"><div>${scorers('home') || 'No home goals'}</div><div class="away">${scorers('away') || 'No away goals'}</div></div></div>
    <div class="tabs">${tabs.map(([id, label]) => `<button data-match-tab="${id}" class="${state.matchTab === id ? 'active' : ''}">${label}</button>`).join('')}</div>${body}`;
    $('follow-match')?.addEventListener('click', () => toast('Match added to your followed list.'));
  }

  function playerCard(p) {
    const handle = handleOf(p);
    const label = p.source === 'database-player' ? 'Source database' : p.source === 'live-database' ? 'Live + source profile' : 'Generated world player';
    return `<button class="player-tile card" data-player="${handle}">
      <span class="pill">${label}</span>
      <strong style="margin-top:9px">${esc(p.name)}</strong>
      <span>${esc(p.club)} · ${esc(p.nation)}</span>
      <span style="margin-top:9px"><span class="badge">OVR ${p.ovr}</span> <span class="badge">G ${p.goals} · A ${p.assists}</span></span>
      <span style="margin-top:8px">Rating ${number(p.rating)} · POT ${p.pot} · Age ${p.age}${p.database_id ? ` · ID ${p.database_id}` : ''}${p.in_active_world === false ? ' · source-only' : ''}</span>
      <span>${esc(p.league || '')}</span>
    </button>`;
  }
  function handleOf(p) { return p?.handle || (p?.world_id != null ? `wp:${p.world_id}` : p?.id); }

  function renderPlayers() {
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Player laboratory</h1><p class="small" style="margin:4px 0 0">Search every source-database prospect plus every generated world player.</p></div><span class="pill">${fmt(state.players.total || 0)} results</span></div>
    <div class="card"><div class="toolbar">
      <input id="player-q" aria-label="Search players" placeholder="Search name, nation, club, league or ID…" value="${esc(state.playerQuery)}"/>
      <select id="player-scope" aria-label="Player scope"><option value="all" ${state.scope === 'all' ? 'selected' : ''}>All players</option><option value="database" ${state.scope === 'database' ? 'selected' : ''}>Full database</option><option value="world" ${state.scope === 'world' ? 'selected' : ''}>Live world only</option></select>
      <select id="player-sort" aria-label="Player sort"><option value="goals" ${state.sort === 'goals' ? 'selected' : ''}>Goals</option><option value="assists" ${state.sort === 'assists' ? 'selected' : ''}>Assists</option><option value="rating" ${state.sort === 'rating' ? 'selected' : ''}>Average rating</option><option value="ovr" ${state.sort === 'ovr' ? 'selected' : ''}>Overall</option><option value="future" ${state.sort === 'future' ? 'selected' : ''}>Potential</option><option value="created" ${state.sort === 'created' ? 'selected' : ''}>Chances created</option><option value="kp" ${state.sort === 'kp' ? 'selected' : ''}>Key passes</option><option value="apps" ${state.sort === 'apps' ? 'selected' : ''}>Appearances</option><option value="name" ${state.sort === 'name' ? 'selected' : ''}>Name</option></select>
      <button id="load-players">Search players</button>
    </div><div class="player-grid">${(state.players.players || []).map(playerCard).join('') || '<div class="empty">No players match.</div>'}</div></div>`;
    $('load-players')?.addEventListener('click', loadPlayers);
    $('player-q')?.addEventListener('keydown', (e) => { if (e.key === 'Enter') loadPlayers(); });
    $('player-sort')?.addEventListener('change', loadPlayers);
    $('player-scope')?.addEventListener('change', loadPlayers);
  }
  async function loadPlayers() {
    state.playerQuery = $('player-q')?.value || '';
    state.sort = $('player-sort')?.value || 'goals';
    state.scope = $('player-scope')?.value || 'all';
    const qs = new URLSearchParams({ q: state.playerQuery, sort: state.sort, scope: state.scope, limit: '250' });
    state.players = await api(`/api/players?${qs}`);
    renderPlayers();
  }

  function radarSvg(attrs) {
    const keys = ['pace','shooting','passing','dribbling','tackling','stamina'];
    const labels = ['Pace','Shooting','Passing','Dribbling','Defending','Engine'];
    const vals = keys.map((k) => Math.max(1, Math.min(100, Number(attrs?.[k] || 1))));
    const cx = 130, cy = 120, r = 78;
    const pt = (i, v) => { const a = (-90 + i * 60) * Math.PI / 180; const rr = r * v / 100; return `${cx + Math.cos(a) * rr},${cy + Math.sin(a) * rr}`; };
    const grid = [25, 50, 75, 100].map((s) => `<polygon points="${vals.map((_, i) => pt(i, s)).join(' ')}" fill="none" stroke="#34343a"/>`).join('');
    const labelsHtml = vals.map((v, i) => { const a = (-90 + i * 60) * Math.PI / 180; const x = cx + Math.cos(a) * (r + 24), y = cy + Math.sin(a) * (r + 16); return `<text x="${x}" y="${y}" fill="#b8b8bd" font-size="11" text-anchor="middle">${labels[i]} ${v}</text>`; }).join('');
    return `<svg class="radar" viewBox="0 0 260 240">${grid}<polygon points="${vals.map((v, i) => pt(i, v)).join(' ')}" fill="rgba(255,90,120,.35)" stroke="#ff5a78" stroke-width="2"/>${labelsHtml}</svg>`;
  }
  function positionMarker(pos) {
    const map = { GK: [50, 88], DEF: [50, 64], MID: [50, 44], FWD: [50, 22] };
    const [x, y] = map[pos] || map.MID;
    return `<div class="mini-pitch"><div class="marker" style="left:${x}%;top:${y}%"><i>${esc(pos || 'MID')}</i><span class="small">${esc(pos || '')}</span></div></div>`;
  }

  async function renderPlayer(handle = state.playerHandle) {
    state.playerHandle = handle;
    state.page = 'player';
    $('app').innerHTML = SPINNER;
    const p = await api(`/api/player/${encodeURIComponent(handle)}`);
    state.playerData = p;
    const live = p.live || p;
    const profile = p.profile || {};
    const source = p.profile ? `<div class="card"><h3>Source database dossier</h3><div class="row-list">
      <div class="row"><span>Database ID</span><strong>${profile.database_id ?? p.database_id ?? '—'}</strong></div>
      <div class="row"><span>Source club / league</span><strong style="text-align:right">${esc(profile.source_club || 'Unattached')}<br><span class="small">${esc(profile.source_league || 'No league listed')}</span></strong></div>
      <div class="row"><span>Original rating / potential</span><strong>${profile.overall ?? '—'} / ${profile.potential ?? '—'} (card ${profile.seen_ovr ?? '—'})</strong></div>
      <div class="row"><span>Market value</span><strong>${fmt(profile.value ?? 0)}</strong></div>
      <div class="row"><span>Wage / contract</span><strong>${fmt(profile.wage ?? 0)} · ${profile.contract_years ?? 0} yrs</strong></div>
      <div class="row"><span>Foot / weak foot / skill</span><strong>${esc(profile.foot || '—')} · ${profile.weak_foot ?? '—'} · ${profile.skill_moves ?? '—'}</strong></div>
      <div class="row"><span>Personality</span><strong>${esc(profile.personality || '—')}</strong></div>
      <div class="row"><span>Height / traits</span><strong style="text-align:right">${profile.height_cm ? `${profile.height_cm} cm` : 'Unknown'}<br><span class="small">${esc(Array.isArray(profile.traits) ? profile.traits.join(', ') : 'None listed')}</span></strong></div>
    </div></div>` : '<div class="card"><h3>Source database dossier</h3><p>No original CSV dossier — this profile was generated inside the simulation world.</p></div>';
    $('app').innerHTML = `<div class="page-head"><div><button class="back-btn" data-page="players">‹ Player laboratory</button><div class="pill" style="margin-top:10px">${esc(live.nation)} · ${esc(live.club)} · ${p.in_active_world === false ? 'Source-only' : 'Live world'}</div></div><button id="follow-player" class="secondary">Follow</button></div>
    <div class="card player-hero"><div class="player-head"><div class="avatar">${esc(initials(live.name))}</div><div><h2>${esc(live.name)}</h2><div class="small">${esc(live.club)} · ${esc(live.league || '')}</div><div style="margin-top:8px"><span class="badge green">OVR ${live.ovr}</span> <span class="badge">POT ${live.pot}</span> <span class="badge">${esc(live.position)}</span></div></div></div>
      <div class="hero-stats">
        <div class="stat"><span class="kpi-label">Age</span><strong>${live.age}</strong><span class="small">${esc(live.position)} · ${esc(live.nation)}</span></div>
        <div class="stat"><span class="kpi-label">Season</span><strong>${live.apps} apps · ${live.goals} G</strong><span class="small">${live.assists} assists · rating ${number(live.rating)}</span></div>
        <div class="stat"><span class="kpi-label">Career</span><strong>${fmt(live.career?.apps ?? 0)} apps</strong><span class="small">${fmt(live.career?.goals ?? 0)} goals · ${fmt(live.career?.assists ?? 0)} assists</span></div>
      </div></div>
    <div class="grid cols-2" style="margin-top:14px">
      <div class="card"><h3>Position</h3>${positionMarker(live.position)}<div class="row-list"><div class="row"><span>Primary</span><strong>${esc(live.position)}</strong></div><div class="row"><span>Age</span><strong>${live.age}</strong></div><div class="row"><span>Club</span><strong>${esc(live.club)}</strong></div></div></div>
      <div class="card"><h3>Player traits</h3>${radarSvg(live.attrs || {})}</div>
    </div>
    <div class="grid cols-2" style="margin-top:14px">
      <div class="card"><h3>Season microdata</h3><div class="row-list">
        <div class="row"><span>Shots</span><strong>${live.shots} (${live.sot} on target)</strong></div>
        <div class="row"><span>Big chances created</span><strong>${live.big_chances}</strong></div>
        <div class="row"><span>Passes</span><strong>${esc(live.passes)}</strong></div>
        <div class="row"><span>Dribbles</span><strong>${esc(live.dribbles)}</strong></div>
        <div class="row"><span>Tackles + interceptions</span><strong>${live.tackles + live.interceptions}</strong></div>
        <div class="row"><span>Saves / clean sheets</span><strong>${live.saves} / ${live.clean_sheets}</strong></div>
        <div class="row"><span>Fatigue / morale</span><strong>${live.fatigue} / ${live.morale}</strong></div>
      </div></div>
      <div class="card"><h3>Attributes</h3>${Object.entries(live.attrs || {}).map(([k, v]) => `<div class="attr-row"><span style="text-transform:capitalize">${esc(k.replaceAll('_',' '))}</span><span class="attr-bar"><i style="width:${Math.min(100, Number(v) * 1.01)}%"></i></span><strong>${v}</strong></div>`).join('')}</div>
    </div>
    <div class="grid cols-2" style="margin-top:14px">
      <div class="card"><h3>Career</h3><div class="row-list"><div class="row"><span>Apps</span><strong>${fmt(live.career?.apps ?? 0)}</strong></div><div class="row"><span>Goals</span><strong>${fmt(live.career?.goals ?? 0)}</strong></div><div class="row"><span>Assists</span><strong>${fmt(live.career?.assists ?? 0)}</strong></div><div class="row"><span>Key passes</span><strong>${fmt(live.career?.kp ?? 0)}</strong></div><div class="row"><span>Ballon d'Ors</span><strong>${live.career?.ballons ?? 0}</strong></div><div class="row"><span>Club path</span><strong style="max-width:65%;text-align:right">${esc((live.career?.path || []).join(' → ') || 'No move yet')}</strong></div></div></div>
      ${source}
    </div>`;
    $('follow-player')?.addEventListener('click', () => toast('Player added to your followed list.'));
  }

  function renderCalendar() {
    const w = state.overview?.world || {};
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Season calendar</h1><p class="small" style="margin:4px 0 0">${fmtDate(state.clockDays)} · ${w.remaining_rounds ?? 0} domestic rounds remain.</p></div><span class="pill">Week ${w.week ?? 0} / ${w.total_rounds ?? 38}</span></div>
    <div class="grid cols-3">
      <div class="card"><h3>Current spoke</h3><p style="font-size:30px;line-height:1;margin:0 0 8px">Week ${w.week ?? 0}</p><p>${fmtDate(state.clockDays)} · ${w.remaining_rounds ?? 0} domestic rounds remain across the world’s main leagues.</p></div>
      <div class="card"><h3>World Cup</h3><p>Qualification is shaped by the integrated federation pools. India must still earn it over decades.</p></div>
      <div class="card"><h3>Continentals</h3><p>Euros, Copa América, AFCON, Gold Cup and AFC Asian Cup rotate through the calendar.</p></div>
    </div>
    <div class="card" style="margin-top:14px"><h2>Advance the calendar</h2><p>Use Play/Pause with Per day, Per week or Per month, or step manually. Every round updates tables, player motives, fatigue, morale and the news wire.</p><div class="toolbar"><button id="calendar-sim">Step week</button><button id="calendar-sim5" class="secondary">Step 5</button><button id="calendar-play" class="ghost">${state.auto.running ? '⏸ Pause' : '▶ Play'}</button></div></div>`;
    $('calendar-sim')?.addEventListener('click', () => simulate(1));
    $('calendar-sim5')?.addEventListener('click', () => simulate(5));
    $('calendar-play')?.addEventListener('click', () => setPlaying(!state.auto.running));
  }

  async function renderManager() {
    state.page = 'manager';
    if (!state.managerClubs.length) {
      try { const data = await api('/api/manager/clubs'); state.managerClubs = data.clubs || []; } catch { state.managerClubs = []; }
    }
    const teamId = state.managerTeam || 0;
    try { state.manager = await api(`/api/manager/team?team=${teamId}`); } catch { state.manager = null; }
    const m = state.manager;
    if (!m || m.error) { $('app').innerHTML = `<div class="empty">Manager mode needs a backend with manager endpoints. ${esc(m?.error || '')}</div>`; return; }
    const t = m.team;
    const options = state.managerClubs.map((c) => `<option value="${c.id}" ${c.id === teamId ? 'selected' : ''}>${esc(c.name)} — ${esc(c.league)}</option>`).join('');
    const squadRows = (m.players || []).map((p) => {
      const mo = p.motives || {};
      const motive = (label, v) => `<div class="motive"><span>${label}</span><span class="attr-bar"><i style="width:${Math.round(Number(v || 0) * 100)}%"></i></span><strong>${Math.round(Number(v || 0) * 100)}</strong></div>`;
      return `<tr><td><strong>${esc(p.name)}</strong><div class="small">${esc(p.position)} · age ${p.age} · ${esc(p.in_active_world === false ? 'source-only' : 'live')}</div></td><td class="numeric">${p.ovr} / ${p.pot}</td><td class="numeric">${p.apps} / ${p.goals} / ${p.assists}</td><td class="numeric">${number(p.rating)}</td><td><span class="status-dot ${mo.unhappy ? 'bad' : 'ok'}"></span>${p.morale}<div class="small">${mo.unhappy ? 'unhappy' : 'content'}${mo.requested ? ' · requested out' : ''}</div></td><td class="numeric">${p.fatigue}</td><td style="white-space:normal;min-width:190px">${motive('Ambition', mo.ambition)}${motive('Loyalty', mo.loyalty)}${motive('Greed', mo.greed)}${motive('Pro', mo.professionalism)}</td><td><div class="action-cell"><button data-manager-player-action="boost" data-player="${p.world_id}">Boost</button><button data-manager-player-action="promise" data-player="${p.world_id}">Promise</button><button data-manager-player-action="train" data-player="${p.world_id}">Train</button></div></td></tr>`;
    }).join('');
    $('app').innerHTML = `<div class="page-head"><div><h1 style="margin:0">Manager mode</h1><p class="small" style="margin:4px 0 0">Pick a club, manage morale/fatigue, promise players, set shape, then simulate.</p></div><span class="pill">${esc(t.league)} · Rank ${m.rank ?? '—'}</span></div>
    <div class="card manager-head"><div><h2 style="margin:0 0 6px">${esc(t.name)}</h2><div class="small">${esc(t.nation)} · rep ${t.reputation} · strength ${number(t.strength)} · shape ${m.shape?.def}-${m.shape?.mid}-${m.shape?.fwd}</div><div class="small">Budget ${fmt(m.finance?.budget ?? 0)} · Cash ${fmt(m.finance?.cash ?? 0)} · Wealth ${m.finance?.wealth ?? '—'}</div></div><div class="toolbar" style="margin:0"><select id="manager-club" aria-label="Select club">${options}</select></div></div>
    <div class="toolbar">
      <button data-manager-action="team_talk">Team talk</button>
      <button data-manager-action="rest" class="secondary">Rest squad</button>
      <select id="manager-shape" aria-label="Formation shape"><option value="4-4-2">4-4-2</option><option value="4-3-3">4-3-3</option><option value="4-2-3-1">4-2-3-1</option><option value="3-5-2">3-5-2</option><option value="3-4-3">3-4-3</option><option value="5-4-1">5-4-1</option></select><button id="apply-shape" class="ghost">Apply shape</button>
      <button id="manager-sim" class="secondary">Step week</button>
    </div>
    <div class="grid cols-2">
      <div class="card"><h3>Mini table</h3>${leagueTable((m.mini_table || []).map((r, i) => ({ ...r, position: (m.rank ? Math.max(1, m.rank - 3) + i : i + 1) })))}</div>
      <div class="card"><h3>Next fixtures</h3>${(m.next_fixtures || []).map((f) => `<div class="row"><span>${esc(f.home)} vs ${esc(f.away)}</span><span class="pill">Week ${f.week}</span></div>`).join('') || '<div class="empty">No fixtures remain.</div>'}</div>
    </div>
    <div class="card" style="margin-top:14px"><h2>Squad & motives</h2><div class="squad-table-wrap"><table class="squad-table"><thead><tr><th>Player</th><th>OVR/POT</th><th>Apps/G/A</th><th>Rtg</th><th>Morale</th><th>Fatigue</th><th>Motives</th><th>Actions</th></tr></thead><tbody>${squadRows}</tbody></table></div></div>`;
    $('manager-club')?.addEventListener('change', async (e) => { state.managerTeam = Number(e.target.value); localStorage.setItem('fm-manager-team', String(state.managerTeam)); await renderManager(); });
    $('apply-shape')?.addEventListener('click', async () => { const v = $('manager-shape').value.split('-').map(Number); await managerAction('shape', null, { def: v[0], mid: v[1], fwd: v[2] }); });
    $('manager-sim')?.addEventListener('click', () => simulate(1));
  }

  async function managerAction(action, playerId, extra = {}) {
    try {
      const res = await api('/api/manager/action', { method: 'POST', body: { team: state.managerTeam, action, player: playerId, ...extra } });
      if (res.error) { toast(res.error, true); return; }
      state.manager = res;
      toast(`${action} applied.`);
      renderManager();
    } catch (e) { toast(e.message, true); }
  }

  function setMode(mode) {
    state.mode = mode;
    localStorage.setItem('fm-mode', mode);
    $('mode-sim')?.classList.toggle('active', mode === 'simulate');
    $('mode-manager')?.classList.toggle('active', mode === 'manager');
    setPage(mode === 'manager' ? 'manager' : 'home');
  }
  function setPage(page) {
    state.page = page;
    document.querySelectorAll('[data-page]').forEach((b) => b.classList.toggle('active', b.dataset.page === page));
    if (page === 'home') renderHome();
    else if (page === 'leagues') renderLeagues();
    else if (page === 'league') renderLeagueDetail(state.leagueIndex);
    else if (page === 'fixtures') renderFixtures();
    else if (page === 'matches') renderMatches();
    else if (page === 'match') renderMatch(state.matchId);
    else if (page === 'players') renderPlayers();
    else if (page === 'player') renderPlayer(state.playerHandle);
    else if (page === 'calendar') renderCalendar();
    else if (page === 'manager') renderManager();
  }

  async function refreshAll() {
    const [overview, leagues, fixtures] = await Promise.all([api('/api/overview'), api('/api/leagues'), api('/api/fixtures')]);
    state.overview = overview;
    state.leagues = leagues.leagues || [];
    state.fixtures = fixtures;
    const w = overview.world || {};
    state.homeWeek = w.week || 0;
    state.homeMatches = [];
    state.homeMatchesWeek = -1;
    updateClock();
    setStatus(`Connected · API ${API}`, true);
    appendNews(w);
    renderRails();
    setPage(state.page || 'home');
  }

  async function simulate(weeks = 1) {
    if (state.auto.inFlight) return;
    state.auto.inFlight = true;
    const sw = $('simulate-week'), pp = $('play-pause');
    if (sw) sw.disabled = true;
    if (pp) pp.disabled = true;
    try {
      const res = await api('/api/simulate/week', { method: 'POST', body: { weeks }, timeout: 300000 });
      toast(`Simulated ${res.matches_played} matches — now week ${res.week}.`);
      await refreshAll();
      if (state.page === 'matches') loadMatches();
    } catch (e) { toast(e.message, true); }
    finally { state.auto.inFlight = false; if (sw) sw.disabled = false; if (pp) pp.disabled = false; }
  }
  function setPlaying(running) {
    state.auto.running = running;
    const pp = $('play-pause');
    if (pp) { pp.textContent = running ? '⏸ Pause' : '▶ Play'; pp.setAttribute('aria-pressed', String(running)); }
    if (state.auto.timer) { clearInterval(state.auto.timer); state.auto.timer = null; }
    if (!running) return;
    state.auto.timer = setInterval(() => {
      if (state.auto.inFlight) return;
      const speed = state.auto.speed || 'week';
      const w = state.overview?.world || {};
      if ((w.remaining_rounds ?? 0) <= 0) { setPlaying(false); return; }
      if (speed === 'day') {
        state.auto.dayOffset += 1;
        if (state.auto.dayOffset % 7 === 0) { state.clockDays = (w.week + 1) * 7; simulate(1); }
        else { state.clockDays += 1; updateClock(); }
      } else if (speed === 'month') {
        const before = seasonDate(state.clockDays);
        let weeks = 1;
        while (weeks < 5 && seasonDate(state.clockDays + weeks * 7).getMonth() === before.getMonth()) weeks += 1;
        simulate(Math.min(weeks, w.remaining_rounds ?? weeks));
      } else simulate(1);
    }, 850);
  }

  async function resetWorld() {
    if (!confirm('Reset to a fresh 2026 world? Unsaved weekly progress will be replaced.')) return;
    try {
      setPlaying(false);
      await api('/api/world/reset', { method: 'POST', body: { seed: Date.now() }, timeout: 300000 });
      state.clockDays = 0; state.auto.dayOffset = 0; state.lastNewsWeek = -1; state.news = []; state.newsIndex = 0; renderNewsRail();
      toast('Fresh world built.');
      await refreshAll();
    } catch (e) { toast(e.message, true); }
  }

  document.addEventListener('click', async (e) => {
    const pageBtn = e.target.closest('[data-page]');
    if (pageBtn) { setPage(pageBtn.dataset.page); return; }
    const league = e.target.closest('[data-league]');
    if (league) { setPage('league'); await renderLeagueDetail(Number(league.dataset.league)); return; }
    const match = e.target.closest('[data-match]');
    if (match) { state.matchTab = 'facts'; state.matchLeagueData = null; setPage('match'); await renderMatch(Number(match.dataset.match)); return; }
    const player = e.target.closest('[data-player]');
    if (player) { setPage('player'); await renderPlayer(player.dataset.player); return; }
    const mtab = e.target.closest('[data-match-tab]');
    if (mtab) { state.matchTab = mtab.dataset.matchTab; renderMatch(state.matchId); return; }
    const ltab = e.target.closest('[data-league-tab]');
    if (ltab) { state.leagueTab = ltab.dataset.leagueTab; renderLeagueDetail(state.leagueIndex); return; }
    const mact = e.target.closest('[data-manager-action]');
    if (mact) { managerAction(mact.dataset.managerAction, null); return; }
    const mpact = e.target.closest('[data-manager-player-action]');
    if (mpact) { managerAction(mpact.dataset.managerPlayerAction, Number(mpact.dataset.player)); return; }
    const btn = e.target.closest('button');
    if (!btn) return;
    if (btn.id === 'mode-sim') setMode('simulate');
    if (btn.id === 'mode-manager') setMode('manager');
    if (btn.id === 'play-pause') setPlaying(!state.auto.running);
    if (btn.id === 'simulate-week') simulate(1);
    if (btn.id === 'simulate-five') simulate(5);
    if (btn.id === 'reset-world') resetWorld();
    if (btn.id === 'all-leagues') setPage('leagues');
    if (btn.id === 'refresh') { try { await refreshAll(); if (state.page === 'players') await loadPlayers(); toast('Dashboard refreshed.'); } catch (err) { toast(err.message, true); } }
  });
  document.addEventListener('change', (e) => {
    if (e.target.id === 'auto-speed') state.auto.speed = e.target.value;
    if (e.target.id === 'player-sort') loadPlayers();
    if (e.target.id === 'player-scope') loadPlayers();
  });
  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); $('global-search')?.focus(); }
    if (e.key === 'Enter' && e.target.id === 'global-search') { state.playerQuery = e.target.value; setMode('simulate'); setPage('players'); loadPlayers(); }
    if (e.key === 'Enter' && e.target.id === 'player-q') loadPlayers();
  });
  $('save-api')?.addEventListener('click', () => {
    const v = $('api-base').value.trim().replace(/\/$/, '');
    if (v) { API = v; localStorage.setItem('fm-web-api-base', v); toast(`API endpoint set to ${v}`); refreshAll().catch((e) => toast(e.message, true)); }
  });

  $('api-base').value = API;
  $('mode-sim')?.classList.toggle('active', state.mode !== 'manager');
  $('mode-manager')?.classList.toggle('active', state.mode === 'manager');
  updateClock();
  renderNewsRail();
  api('/api/health').then(async () => { await refreshAll(); await loadPlayers(); }).catch((e) => { setStatus(`Could not connect — ${e.message}`, false); $('app').innerHTML = `<div class="alert"><strong>Simulation API is unreachable.</strong><p>The frontend is loaded, but the Rust backend did not answer at <code>${esc(API)}</code>. Start it with <code>cargo run --release -- serve</code>, check that the tunnel/process is still running, or enter a different endpoint in the left rail.</p><p><a href="${esc(API)}" target="_blank" rel="noreferrer">Open backend dashboard</a> · <a href="/api/health" target="_blank" rel="noreferrer">Health check</a></p><p>${esc(e.message)}</p></div>`; });
})();
