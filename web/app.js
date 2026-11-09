(() => {
  'use strict';

  const SPINNER = '<span class="pill">Loading live world…</span>';
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
  let state = { overview: null, leagues: [], players: { players: [], total: 0 }, page: 'overview', leagueIndex: 0, playerQuery: '', sort: 'goals', scope: 'all', matchQuery: '' };

  const $ = (id) => document.getElementById(id);
  const fmt = (n) => new Intl.NumberFormat().format(n ?? 0);
  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const number = (value) => Number(value ?? 0).toLocaleString(undefined, { maximumFractionDigits: 2 });

  function setStatus(text, ok = true) {
    const el = $('api-status');
    el.textContent = text;
    el.style.color = ok ? 'var(--mint)' : 'var(--rose)';
  }
  function toast(message, error = false) {
    const el = $('toast');
    el.textContent = message;
    el.className = error ? 'error show' : 'show';
    clearTimeout(el._timer);
    el._timer = setTimeout(() => el.className = '', 3400);
  }
  async function api(path, options = {}) {
    const url = `${API}${path}`;
    const res = await fetch(url, {
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

  function kpi(label, value, note) {
    return `<div class="metric"><span class="kpi-label">${esc(label)}</span><strong>${esc(value)}</strong><span>${esc(note || '')}</span></div>`;
  }
  function scoreboard(matches, limit = 8) {
    if (!matches?.length) return '<div class="empty">No matches yet — simulate a week.</div>';
    return `<div class="row-list">${matches.slice(0, limit).map((m) => `
      <div class="row match-card" data-match="${m.id}">
        <span><span class="pill">${esc(m.league)}</span><br><strong>${esc(m.home)} ${esc(m.home_goals)}–${esc(m.away_goals)} ${esc(m.away)}</strong><div class="match-sub">xG ${number(m.xg?.[0])} / ${number(m.xg?.[1])} · shots ${esc(m.shots?.join('–'))} · possession ${esc(m.possession)}%</div></span>
        <span class="pill good">Match ${m.id}</span>
      </div>`).join('')}</div>`;
  }
  function playerCard(p) {
    const handle = esc(p.handle || p.id);
    const sourceLabel = p.source === 'database-player' ? 'Source database' : p.source === 'live-database' ? 'Live + source profile' : 'Generated world player';
    const sourceDetail = p.database_id ? ` · ID ${p.database_id}` : '';
    const status = p.in_active_world === false ? ' · not in active world' : '';
    return `<button class="player-tile card" data-player="${handle}">
      <span class="pill">${sourceLabel}</span>
      <strong style="margin-top:9px">${esc(p.name)}</strong>
      <span>${esc(p.club)} · ${esc(p.nation)}</span>
      <span style="margin-top:9px"><span class="badge">OVR ${p.ovr}</span> <span class="badge">G ${p.goals} · A ${p.assists}</span></span>
      <span style="margin-top:8px">Avg rating ${number(p.rating)} · POT ${p.pot} · Age ${p.age}${sourceDetail}${status}</span>
      <span>${esc(p.league || '')}</span>
    </button>`;
  }

  function renderOverview() {
    const w = state.overview?.world || {};
    const top = state.overview?.top_scorers || [];
    $('view-title').textContent = 'World overview';
    $('view-subtitle').textContent = 'Live status of the 2026 living world.';
    $('app').innerHTML = `
      <div class="hero">
        <div><div class="pill">Live world · Week ${w.week ?? 0}</div><h2>Season control room: advance fixtures, open matches, audit every player.</h2><p>The Rust simulation supplies every table, stat line and career record shown here. Use the controls in the left rail to move the entire world forward, then drill into the result you care about.</p></div>
        <div class="hero-metrics">
          ${kpi('Players', fmt(w.players), 'world pool')}
          ${kpi('Database players', fmt(w.database_players), `${fmt(w.database_clubs)} source clubs`)}
          ${kpi('Teams', fmt(w.teams), 'active clubs')}
          ${kpi('Matches played', fmt(w.matches_played), `round ${w.week ?? 0} complete`)}
        </div>
      </div>
      <div class="grid cols-3">
        <div class="card"><h3>World control</h3><p>Advance the same shared calendar across every domestic league. Every played match retains its start XI, event timeline and per-player stat line.</p><div class="actions"><button id="hero-sim">Simulate week</button><button id="hero-sim5" class="secondary">Simulate 5 weeks</button></div></div>
        <div class="card"><h3>World DNA</h3><div class="row-list">
          <div class="row"><span>Players</span><strong>${fmt(w.players)}</strong></div>
          <div class="row"><span>Active clubs</span><strong>${fmt(w.teams)}</strong></div>
          <div class="row"><span>Domestic leagues</span><strong>${fmt(w.leagues)}</strong></div>
          <div class="row"><span>Season seed</span><strong>${esc(w.seed ?? '')}</strong></div>
        </div></div>
        <div class="card"><h3>Top scorers</h3>${top.length ? `<div class="row-list">${top.slice(0, 6).map((p, i) => `<div class="row"><span>${i+1}. <button class="linklike" data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)} · ${esc(p.league || '')}</div></span><strong>${p.goals} goals</strong></div>`).join('')}</div>` : '<p>No scorer data yet.</p>'}</div>
      </div>
      <div class="grid cols-2"><div class="card"><h2>Latest match signals</h2>${scoreboard(state.overview?.recent_matches || [], 8)}</div>
      <div class="card"><h2>How to inspect the world</h2><p>Open any match to see the 22 starter lines, expected goals, shot mix, possession and events. Open any player to see attributes, season microdata, career totals and the source-database dossier.</p><button data-page="fixtures">Open fixtures</button> <button data-page="players">Open player laboratory</button></div></div>`;
    bindDynamic();
  }

  function renderLeagues() {
    $('view-title').textContent = 'Leagues';
    $('view-subtitle').textContent = 'Every domestic competition in the live world.';
    $('app').innerHTML = `<div class="grid cols-4">${state.leagues.map((l) => `
      <button class="card league-card" data-league="${l.id}">
        <div class="pill">${esc(l.confederation || '')}</div>
        <h3 style="margin-top:10px">${esc(l.name)}</h3>
        <p>${l.teams} clubs · ${l.rounds} rounds<br>Leader: <strong>${esc(l.leader || '—')}</strong></p>
      </button>`).join('')}</div>`;
    bindDynamic();
  }

  async function renderLeagueDetail(index) {
    state.page = 'leagues';
    document.querySelectorAll('nav button').forEach((b) => b.classList.toggle('active', b.dataset.page === 'leagues'));
    $('view-title').textContent = 'League detail';
    $('app').innerHTML = SPINNER;
    const data = await api(`/api/league/${index}`);
    const table = data.table || [];
    const scorers = data.scorers || [];
    $('view-title').textContent = data.league || 'League';
    $('view-subtitle').textContent = `${data.week || 0} weeks played · ${data.confederation || ''}`;
    $('app').innerHTML = `
      <div class="grid cols-3">
        <div class="card" style="grid-column:span 2"><h2>League table</h2><div class="table-wrap"><table><thead><tr><th>#</th><th>Club</th><th>P</th><th>W</th><th>D</th><th>L</th><th>GF</th><th>GA</th><th>GD</th><th>Pts</th></tr></thead><tbody>
        ${table.map((r) => `<tr><td class="numeric">${r.position}</td><td><strong>${esc(r.name)}</strong><div class="small">${esc(r.nation)}</div></td><td class="numeric">${r.table.played}</td><td class="numeric">${r.table.won}</td><td class="numeric">${r.table.drawn}</td><td class="numeric">${r.table.lost}</td><td class="numeric">${r.table.gf}</td><td class="numeric">${r.table.ga}</td><td class="numeric">${r.table.gd}</td><td class="numeric"><strong>${r.table.pts}</strong></td></tr>`).join('')}
        </tbody></table></div></div>
        <div class="card"><h3>Scoring leaders</h3>${scorers.slice(0, 12).map((p) => `<div class="row"><span><button data-player="${esc(p.handle || p.id)}">${esc(p.name)}</button><div class="small">${esc(p.club)} · ${p.apps} apps</div></span><strong>${p.goals} G · ${p.assists} A</strong></div>`).join('') || '<p>No league scorer data.</p>'}</div>
      </div>
      <div class="grid cols-2"><div class="card"><h2>Next fixtures</h2>${(data.next_fixtures || []).map((f) => `<div class="row"><span>${esc(f.home)} vs ${esc(f.away)}</span><span class="pill">scheduled</span></div>`).join('') || '<p>No fixtures remain.</p>'}</div>
      <div class="card"><h2>Recent league matches</h2>${scoreboard(data.recent || [], 14)}</div></div>`;
    bindDynamic();
  }

  function renderFixtures() {
    $('view-title').textContent = 'Fixtures & league calendar';
    $('view-subtitle').textContent = 'The round-robin calendar across all competitions.';
    $('app').innerHTML = `<div class="card"><div class="toolbar"><input id="fixture-league-filter" placeholder="Filter league"/><button id="load-fixtures">Apply</button></div><div class="grid cols-2">${state.fixtures?.fixtures?.map((f) => `<div class="card match-card"><div class="pill">${esc(f.league)}</div><h3 style="margin:12px 0 4px">${esc(f.home)} vs ${esc(f.away)}</h3><span class="match-sub">Week ${esc(f.week)} · round-robin domestic league</span></div>`).join('') || '<div class="empty">No fixtures left this season.</div>'}</div></div>`;
    bindDynamic();
  }

  function renderMatches() {
    $('view-title').textContent = 'Match centre';
    $('view-subtitle').textContent = 'Every played result, with deep post-match microdata.';
    $('app').innerHTML = `<div class="card"><div class="toolbar">
      <select id="match-week"><option value="">All weeks</option>${Array.from({length: Math.max(1, state.overview?.world?.week || 1)}, (_,i)=>`<option value="${i+1}">Week ${i+1}</option>`).join('')}</select>
      <input id="match-q" placeholder="Search club or competition…"/>
      <button id="load-matches">Load matches</button>
      <button id="load-current-week">Show current week</button>
    </div><div id="match-results">${scoreboard(state.matchesList || [], 100)}</div></div>`;
    bindDynamic();
  }

  function renderPlayers() {
    $('view-title').textContent = 'Player laboratory';
    $('view-subtitle').textContent = 'Every source-database prospect plus every generated world player, with live in-season detail where one exists.';
    const p = state.players;
    const census = p.database_total ? `${fmt(p.database_total)} database profiles · ${fmt(p.database_only_total || 0)} source-only · ${fmt(p.database_active_total || 0)} live in the world` : `${fmt(p.total || 0)} results`;
    $('app').innerHTML = `<div class="card"><div class="toolbar">
      <input id="player-q" placeholder="Search name, nation, club, league or ID…" value="${esc(state.playerQuery)}"/>
      <select id="player-scope"><option value="all" ${state.scope === 'all' ? 'selected' : ''}>All players</option><option value="database" ${state.scope === 'database' ? 'selected' : ''}>Full database</option><option value="world" ${state.scope === 'world' ? 'selected' : ''}>Live world only</option></select>
      <select id="player-sort"><option value="goals" ${state.sort === 'goals' ? 'selected' : ''}>Goals</option><option value="assists" ${state.sort === 'assists' ? 'selected' : ''}>Assists</option><option value="rating" ${state.sort === 'rating' ? 'selected' : ''}>Average rating</option><option value="ovr" ${state.sort === 'ovr' ? 'selected' : ''}>Overall</option><option value="future" ${state.sort === 'future' ? 'selected' : ''}>Potential</option><option value="created" ${state.sort === 'created' ? 'selected' : ''}>Chances created</option><option value="kp" ${state.sort === 'kp' ? 'selected' : ''}>Key passes</option><option value="apps" ${state.sort === 'apps' ? 'selected' : ''}>Appearances</option><option value="name" ${state.sort === 'name' ? 'selected' : ''}>Name</option></select>
      <button id="load-players">Search players</button>
      <span class="pill">${fmt(p.total || 0)} results · ${census}</span>
    </div><div class="player-grid">${(p.players || []).map(playerCard).join('') || '<div class="empty">No players match.</div>'}</div></div>`;
    bindDynamic();
  }

  function renderCalendar() {
    $('view-title').textContent = 'Season calendar';
    $('view-subtitle').textContent = 'Rhythm of the season: domestic rounds, continental tournaments and the international calendar.';
    const w = state.overview?.world || {};
    $('app').innerHTML = `<div class="grid cols-3">
      <div class="card"><h3>Current spoke</h3><p style="font-size:32px;line-height:1">Week ${w.week ?? 0}</p><p>${w.remaining_rounds ?? 0} domestic rounds remain across the world’s main leagues.</p></div>
      <div class="card"><h3>World Cup</h3><p>Qualification shaped by the new integrated federation pools. India must still earn it over decades.</p></div>
      <div class="card"><h3>Continentals</h3><p>Euros, Copa América, AFCON, Gold Cup and AFC Asian Cup now rotate through the calendar.</p></div>
    </div>
    <div class="card" style="margin-top:18px"><h2>Why one click advances everything</h2><p>Simulation steps a full world round from every league at once, applying fatigue, morale, player development, career records, league tables and fixture rotation. Every domestic result is stored with detailed 22-player match outcomes for inspection.</p><div class="toolbar"><button id="calendar-sim">Simulate week</button><button id="calendar-sim5" class="secondary">Simulate 5 weeks</button></div></div>`;
    bindDynamic();
  }

  function setPage(page) {
    state.page = page;
    document.querySelectorAll('nav button').forEach((b) => b.classList.toggle('active', b.dataset.page === page));
    if (page === 'overview') renderOverview();
    if (page === 'leagues') renderLeagues();
    if (page === 'fixtures') renderFixtures();
    if (page === 'matches') renderMatches();
    if (page === 'players') renderPlayers();
    if (page === 'calendar') renderCalendar();
  }

  async function refreshAll() {
    const [overview, leagues, fixtures] = await Promise.all([
      api('/api/overview'),
      api('/api/leagues'),
      api('/api/fixtures'),
    ]);
    state.overview = overview;
    state.leagues = leagues.leagues || [];
    state.fixtures = fixtures;
    const w = overview.world || {};
    $('world-week').textContent = `Week ${w.week ?? 0}`;
    $('world-meta').textContent = `${fmt(w.teams)} clubs · ${fmt(w.leagues)} leagues · ${fmt(w.matches_played)} matches played`;
    setStatus(`Connected · API ${API}`, true);
    setPage(state.page || 'overview');
  }

  async function simulate(weeks = 1) {
    if ($('simulate-week')) $('simulate-week').disabled = true;
    try {
      const res = await api('/api/simulate/week', { method: 'POST', body: { weeks }, timeout: 300000 });
      toast(`Simulated ${res.matches_played} matches — now week ${res.week}.`);
      await refreshAll();
      if (state.page === 'matches') loadMatches();
    } catch (e) { toast(e.message, true); }
    finally { const b = $('simulate-week'); if (b) b.disabled = false; }
  }

  async function resetWorld() {
    if (!confirm('Reset to a fresh 2026 world? Unsaved weekly progress will be replaced.')) return;
    try {
      await api('/api/world/reset', { method: 'POST', body: { seed: Date.now() }, timeout: 300000 });
      toast('Fresh world built.');
      await refreshAll();
    } catch (e) { toast(e.message, true); }
  }

  async function loadMatches() {
    const week = $('match-week')?.value || '';
    const q = ($('match-q')?.value || state.matchQuery || '').toLowerCase();
    const path = week ? `/api/matches?week=${encodeURIComponent(week)}` : '/api/matches';
    const data = await api(path);
    const list = (data.matches || []).filter((m) => !q || `${m.home} ${m.away} ${m.league}`.toLowerCase().includes(q));
    state.matchesList = list;
    const el = $('match-results');
    if (el) el.innerHTML = scoreboard(list, 120);
    bindDynamic();
  }

  async function loadPlayers() {
    state.playerQuery = $('player-q')?.value || '';
    state.sort = $('player-sort')?.value || 'goals';
    state.scope = $('player-scope')?.value || 'all';
    const qs = new URLSearchParams({ q: state.playerQuery, sort: state.sort, scope: state.scope, limit: '250' });
    state.players = await api(`/api/players?${qs}`);
    renderPlayers();
  }

  async function openMatch(id) {
    const m = await api(`/api/match/${id}`);
    const rows = (a, team) => `<tr><td><strong>${esc(a.name)}</strong><div class="small">${esc(a.nation)} · ${esc(a.position)}</div></td><td class="numeric">${a.rating?.toFixed(2) ?? ''}</td><td class="numeric">${a.minutes}</td><td class="numeric">${a.goals}</td><td class="numeric">${a.assists}</td><td class="numeric">${a.shots}</td><td class="numeric">${a.on_target}</td><td class="numeric">${a.xg?.toFixed(2) ?? ''}</td><td class="numeric">${a.key_passes}</td><td class="numeric">${a.passes}</td><td class="numeric">${a.dribbles}</td><td class="numeric">${a.tackles}</td><td class="numeric">${a.interceptions}</td><td class="numeric">${a.saves}</td></tr>`;
    openDrawer(`
      <div class="pill">${esc(m.match.league)} · Week ${esc(m.match.week)}</div>
      <h2 style="margin:12px 0">${esc(m.match.home)} ${m.match.home_goals}–${m.match.away_goals} ${esc(m.match.away)}</h2>
      <div class="grid cols-3">
        ${kpi('Expected goals', `${number(m.match.xg?.[0])}–${number(m.match.xg?.[1])}`, 'chance quality')}
        ${kpi('Shots', `${m.match.shots?.join(' – ')}`, 'total attempts')}
        ${kpi('Possession', `${m.match.possession}%`, 'home ball share')}
      </div>
      <h3>Events</h3>
      <div>${(m.events || []).map((e) => `<div class="event"><span class="minute">${e.minute}'</span><span><strong>${esc(e.player)}</strong> ${esc(e.type)} ${e.assist ? `· assisted by ${esc(e.assist)}` : ''}</span></div>`).join('') || '<p>No goal/event notes.</p>'}</div>
      <div class="detail-grid"><div class="card"><h3>${esc(m.match.home)} stat lines</h3><div class="table-wrap"><table><thead><tr><th>Player</th><th>Rating</th><th>Min</th><th>G</th><th>A</th><th>Sh</th><th>OnT</th><th>xG</th><th>KP</th><th>Passes</th><th>Dribbles</th><th>Tkl</th><th>INT</th><th>Saves</th></tr></thead><tbody>${(m.home_lineup || []).map((a) => rows(a)).join('')}</tbody></table></div></div>
      <div class="card"><h3>${esc(m.match.away)} stat lines</h3><div class="table-wrap"><table><thead><tr><th>Player</th><th>Rating</th><th>Min</th><th>G</th><th>A</th><th>Sh</th><th>OnT</th><th>xG</th><th>KP</th><th>Passes</th><th>Drib</th><th>Tkl</th><th>INT</th><th>Saves</th></tr></thead><tbody>${(m.away_lineup || []).map((a) => rows(a)).join('')}</tbody></table></div></div></div>
    `);
  }

  async function openPlayer(id) {
    const raw = String(id || '');
    const endpoint = raw.includes(':') ? `/api/player/${encodeURIComponent(raw)}` : `/api/player/${raw}`;
    const p = await api(endpoint);
    const live = p.live || p;
    const profile = p.profile || {};
    const profileTraits = Array.isArray(profile.traits) && profile.traits.length ? profile.traits.join(', ') : 'None listed';
    const sourceProfile = p.profile ? `<div class="card"><h3>Source database dossier</h3><div class="row-list">
      <div class="row"><span>Database ID</span><strong>${profile.database_id ?? p.database_id ?? '—'}</strong></div>
      <div class="row"><span>Source club / league</span><strong style="text-align:right">${esc(profile.source_club || 'Unattached')}<br><span class="small">${esc(profile.source_league || 'No league listed')}</span></strong></div>
      <div class="row"><span>Original rating / potential</span><strong>${profile.overall ?? '—'} / ${profile.potential ?? '—'} (card ${profile.seen_ovr ?? '—'})</strong></div>
      <div class="row"><span>Market value</span><strong>${fmt(profile.value ?? 0)}</strong></div>
      <div class="row"><span>Wage / contract</span><strong>${fmt(profile.wage ?? 0)} · ${profile.contract_years ?? 0} yrs</strong></div>
      <div class="row"><span>Foot / weak foot / skill</span><strong>${esc(profile.foot || '—')} · ${profile.weak_foot ?? '—'} · ${profile.skill_moves ?? '—'}</strong></div>
      <div class="row"><span>Personality</span><strong>${esc(profile.personality || '—')}</strong></div>
      <div class="row"><span>Height / traits</span><strong style="text-align:right">${profile.height_cm ? `${profile.height_cm} cm` : 'Unknown'}<br><span class="small">${esc(profileTraits)}</span></strong></div>
      <div class="row"><span>Fitness / sharpness / condition</span><strong>${profile.fitness ?? '—'} · ${profile.sharpness ?? '—'} · ${profile.condition ?? '—'}</strong></div>
      <div class="row"><span>Scout knowledge / happiness</span><strong>${profile.scout_knowledge ?? '—'} · ${profile.happiness ?? '—'}</strong></div>
    </div></div>` : `<div class="card"><h3>Source database dossier</h3><p>This player is generated directly in the simulation world and has no original CSV dossier.</p></div>`;
    openDrawer(`
      <div class="pill">${esc(live.nation)} · ${esc(live.club)} · ${p.in_active_world === false ? 'Source-only' : 'Live world'}</div>
      <h2 style="margin:12px 0">${esc(live.name)}</h2>
      <div class="grid cols-4">
        ${kpi('Current overall', live.ovr, `potential ${live.pot}`)}
        ${kpi('Age', live.age, `${esc(live.position)} · ${esc(live.league)}`)}
        ${kpi('Apps / goals', `${live.apps} / ${live.goals}`, `${live.assists} assists`)}
        ${kpi('Average rating', number(live.rating), `${live.key_passes} key passes`)}
      </div>
      <div class="detail-grid">
        <div class="card"><h3>Attributes</h3>${Object.entries(live.attrs || {}).map(([k, v]) => `<div class="row"><span style="text-transform:capitalize">${esc(k.replaceAll('_',' '))}</span><strong>${v}</strong></div><div class="bar"><i style="width:${Math.min(100, Number(v) * 1.01)}%"></i></div>`).join('')}</div>
        <div><div class="card"><h3>Season microdata</h3><div class="row-list">
          <div class="row"><span>Shots</span><strong>${live.shots} (${live.sot} on target)</strong></div>
          <div class="row"><span>Big chances created</span><strong>${live.big_chances}</strong></div>
          <div class="row"><span>Passes</span><strong>${esc(live.passes)}</strong></div>
          <div class="row"><span>Dribbles</span><strong>${esc(live.dribbles)}</strong></div>
          <div class="row"><span>Tackles + interceptions</span><strong>${live.tackles + live.interceptions}</strong></div>
          <div class="row"><span>Saves / clean sheets</span><strong>${live.saves} / ${live.clean_sheets}</strong></div>
          <div class="row"><span>Fatigue / morale</span><strong>${live.fatigue} / ${live.morale}</strong></div>
          <div class="row"><span>International output</span><strong>${live.intl_goals ?? 0} goals · ${live.intl_assists ?? 0} assists</strong></div>
        </div></div>
        <div class="card"><h3>Career</h3><div class="row-list">
          <div class="row"><span>Apps</span><strong>${fmt(live.career?.apps ?? 0)}</strong></div>
          <div class="row"><span>Goals</span><strong>${fmt(live.career?.goals ?? 0)}</strong></div>
          <div class="row"><span>Assists</span><strong>${fmt(live.career?.assists ?? 0)}</strong></div>
          <div class="row"><span>Key passes</span><strong>${fmt(live.career?.kp ?? 0)}</strong></div>
          <div class="row"><span>Ballon d'Ors</span><strong>${live.career?.ballons ?? 0}</strong></div>
          <div class="row"><span>Club path</span><strong style="max-width:65%;text-align:right">${esc((live.career?.path || []).join(' → ') || 'No move yet')}</strong></div>
        </div></div>
        ${sourceProfile}
      </div>`);
  }

  function openDrawer(html) {
    const old = document.querySelector('.detail-drawer,.scrim'); if (old) old.remove();
    document.body.insertAdjacentHTML('beforeend', `<div class="scrim"></div><aside class="detail-drawer">${html}<button class="close-detail">Close</button></aside>`);
    requestAnimationFrame(() => { document.querySelector('.scrim').classList.add('show'); document.querySelector('.detail-drawer').classList.add('open'); });
    document.querySelector('.close-detail').addEventListener('click', closeDrawer);
    document.querySelector('.scrim').addEventListener('click', closeDrawer);
  }
  function closeDrawer() { document.querySelector('.detail-drawer')?.remove(); document.querySelector('.scrim')?.remove(); }

  function bindDynamic() {
    document.querySelectorAll('[data-match]').forEach((el) => el.addEventListener('click', () => openMatch(el.dataset.match)));
    document.querySelectorAll('[data-player]').forEach((el) => el.addEventListener('click', () => openPlayer(el.dataset.player)));
    document.querySelectorAll('[data-page]').forEach((el) => el.addEventListener('click', () => setPage(el.dataset.page)));
    $('simulate-week')?.addEventListener('click', () => simulate(1));
    $('simulate-five')?.addEventListener('click', () => simulate(5));
    $('hero-sim')?.addEventListener('click', () => simulate(1));
    $('hero-sim5')?.addEventListener('click', () => simulate(5));
    $('calendar-sim')?.addEventListener('click', () => simulate(1));
    $('calendar-sim5')?.addEventListener('click', () => simulate(5));
    $('reset-world')?.addEventListener('click', resetWorld);
    $('load-fixtures')?.addEventListener('click', () => {
      const q = $('fixture-league-filter').value.toLowerCase();
      document.querySelectorAll('.match-card').forEach((el) => { el.style.display = !q || el.textContent.toLowerCase().includes(q) ? 'block' : 'none'; });
    });
    $('load-matches')?.addEventListener('click', loadMatches);
    $('load-current-week')?.addEventListener('click', loadMatches);
    $('load-players')?.addEventListener('click', loadPlayers);
    $('player-q')?.addEventListener('keydown', (e) => { if (e.key === 'Enter') loadPlayers(); });
    $('player-sort')?.addEventListener('change', loadPlayers);
    $('player-scope')?.addEventListener('change', loadPlayers);
  }

  document.querySelectorAll('nav button').forEach((b) => b.addEventListener('click', () => setPage(b.dataset.page)));
  $('simulate-week').addEventListener('click', () => simulate(1));
  $('simulate-five').addEventListener('click', () => simulate(5));
  $('reset-world').addEventListener('click', resetWorld);
  $('refresh').addEventListener('click', async () => { try { await refreshAll(); if (state.page === 'players') await loadPlayers(); if (state.page === 'matches') await loadMatches(); toast('Dashboard refreshed.'); } catch (e) { toast(e.message, true); } });
  $('global-search').addEventListener('keydown', async (e) => { if (e.key === 'Enter') { state.playerQuery = e.target.value; state.page='players'; await loadPlayers(); } });
  $('save-api').addEventListener('click', () => {
    const v = $('api-base').value.trim().replace(/\/$/, '');
    if (v) { API = v; localStorage.setItem('fm-web-api-base', v); toast(`API endpoint set to ${v}`); refreshAll().catch((e) => toast(e.message, true)); }
  });

  $('api-base').value = API;
  api('/api/health').then(async () => { await refreshAll(); await loadPlayers(); }).catch((e) => { setStatus(`Could not connect — ${e.message}`, false); $('app').innerHTML = `<div class="alert"><strong>Simulation API is unreachable.</strong><p>The frontend is loaded, but the Rust backend did not answer at <code>${esc(API)}</code>. Start it with <code>cargo run --release -- serve</code> from the project root, check that the tunnel/process is still running, or enter a different endpoint in the left rail.</p><p><a href="${esc(API)}" target="_blank" rel="noreferrer">Open backend dashboard</a> · <a href="/api/health" target="_blank" rel="noreferrer">Health check</a></p><p>${esc(e.message)}</p></div>`; });
})();
