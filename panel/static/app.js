// Alpha 分发决策面板 —— 单页应用前端（原生 JS，无框架）
(function () {
  "use strict";
  const I18N = window.I18N;
  const STATE = { user: null, role: null, lang: "zh" };
  const PK = { loaded: false, pokemon: [], abilities: [], moves: [] };

  // ---------------- 基础工具 ----------------
  function t(key) {
    const d = I18N[STATE.lang] || I18N.zh;
    return d[key] !== undefined ? d[key] : (I18N.zh[key] !== undefined ? I18N.zh[key] : key);
  }
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, c =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }
  async function api(path, opts) {
    opts = opts || {};
    const init = { method: opts.method || "GET", credentials: "same-origin", headers: {} };
    if (opts.json) { init.headers["Content-Type"] = "application/json"; init.body = JSON.stringify(opts.json); }
    else if (opts.body) { init.body = opts.body; }
    const r = await fetch(path, init);
    if (r.status === 401) { renderLogin(); throw new Error("unauthorized"); }
    let data = null; try { data = await r.json(); } catch (e) {}
    return { ok: r.ok, status: r.status, data };
  }
  function toast(msg, type) {
    const w = document.getElementById("toast-wrap");
    const el = document.createElement("div");
    el.className = "toast " + (type || "info");
    el.textContent = msg;
    w.appendChild(el);
    setTimeout(() => { el.style.opacity = "0"; setTimeout(() => el.remove(), 300); }, 2400);
  }
  function switchEl(on, onchange) {
    const id = "sw_" + Math.random().toString(36).slice(2, 8);
    setTimeout(() => {
      const el = document.getElementById(id);
      if (el) el.addEventListener("change", e => onchange(e.target.checked));
    }, 0);
    return `<label class="switch"><input type="checkbox" id="${id}" ${on ? "checked" : ""}><span class="slider"></span></label>`;
  }
  function openModal(title, bodyHtml, onOk) {
    const mask = document.createElement("div");
    mask.className = "modal-mask";
    mask.innerHTML = `<div class="modal"><h3>${esc(title)}</h3>${bodyHtml}<div class="row actions" style="margin-top:18px;justify-content:flex-end">
      <button class="btn btn-ghost btn-sm" id="m_cancel">${t("common.cancel")}</button>
      <button class="btn btn-sm" id="m_ok">${t("common.ok")}</button></div></div>`;
    document.body.appendChild(mask);
    mask.querySelector("#m_cancel").onclick = () => mask.remove();
    mask.querySelector("#m_ok").onclick = () => { if (onOk(mask) !== false) mask.remove(); };
    mask.onclick = e => { if (e.target === mask) mask.remove(); };
    return mask;
  }

  // ---------------- 路由 ----------------
  /** 内联 SVG 图标：不依赖系统 emoji 字体，任何环境都能正常显示。 */
  function icon(name) {
    const P = {
      dashboard: '<rect x="3" y="3" width="7.5" height="7.5" rx="1.6"/><rect x="13.5" y="3" width="7.5" height="7.5" rx="1.6"/><rect x="3" y="13.5" width="7.5" height="7.5" rx="1.6"/><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="1.6"/>',
      debug: '<path d="M14.7 6.3a4.5 4.5 0 0 0 5.9 5.9l-8 8a2.5 2.5 0 0 1-3.5-3.5l8-8Z"/><path d="M4 20l3-3"/>',
      scheduler: '<circle cx="12" cy="13" r="8"/><path d="M12 9v4.5l3 2"/><path d="M9 2h6"/>',
      sources: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18Z"/>',
      dispatchers: '<path d="M12 3v3"/><rect x="5" y="6" width="14" height="11" rx="2.5"/><circle cx="9.5" cy="11" r="1.3"/><circle cx="14.5" cy="11" r="1.3"/><path d="M9.5 14.5h5"/><path d="M9 21h6"/>',
      channels: '<path d="M21 4 3 11l7 3 3 7 8-17Z"/><path d="M10 14l4-4"/>',
      logs: '<path d="M6 3h9l5 5v13a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Z"/><path d="M14 3v6h6"/><path d="M8.5 13h7M8.5 17h5"/>',
      system: '<circle cx="12" cy="12" r="3.2"/><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.6 1.6 0 0 0 9 19.4a1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.6 1.6 0 0 0 4.6 9a1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1Z"/>',
      about: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5"/><circle cx="12" cy="7.8" r="0.9" fill="currentColor" stroke="none"/>',
    };
    return `<svg class="ico" viewBox="0 0 24 24" aria-hidden="true">${P[name] || ""}</svg>`;
  }

  const MENU = [
    { group: "menu.group.monitor" },
    { key: "dashboard", icon: "dashboard", route: "#/dashboard" },
    { key: "debug", icon: "debug", route: "#/debug" },
    { key: "scheduler", icon: "scheduler", route: "#/scheduler" },
    { group: "menu.group.source" },
    { key: "sources", icon: "sources", route: "#/sources" },
    { group: "menu.group.dispatch" },
    { key: "dispatchers", icon: "dispatchers", route: "#/dispatchers" },
    { key: "channels", icon: "channels", route: "#/channels" },
    { group: "menu.group.system" },
    { key: "logs", icon: "logs", route: "#/logs" },
    { key: "system", icon: "system", route: "#/system" },
    { key: "about", icon: "about", route: "#/about" },
  ];
  const ROUTES = {
    dashboard: renderDashboard, debug: renderDebug, scheduler: renderScheduler,
    sources: renderSources, dispatchers: renderDispatchers, channels: renderChannels,
    logs: renderLogs, system: renderSystem, about: renderAbout,
  };
  function currentRoute() {
    const h = (location.hash || "").replace("#/", "");
    return ROUTES[h] ? h : "dashboard";
  }
  function route() {
    const name = currentRoute();
    document.querySelectorAll(".menu-item").forEach(el =>
      el.classList.toggle("active", el.dataset.route === name));
    (ROUTES[name] || renderDashboard)();
  }

  // ---------------- 登录 ----------------
  function renderLogin() {
    STATE.user = null;
    const app = document.getElementById("app");
    app.innerHTML = `<div class="login-screen">
      <div class="login-card">
        <div class="login-brand">
          <img class="login-logo" src="/static/logo.png" alt="Alpha" />
          <div class="t">${t("login.title")}</div>
          <div class="s">${t("login.subtitle")}</div>
        </div>
        <div class="form">
          <div class="row">
            <label>${t("login.username")}</label>
            <input id="lg_user" placeholder="${t("login.username_ph")}" autocomplete="username" />
          </div>
          <div class="row">
            <label>${t("login.password")}</label>
            <input id="lg_pass" type="password" placeholder="${t("login.password_ph")}" autocomplete="current-password" />
          </div>
          <button class="btn" id="lg_btn" style="width:100%;justify-content:center">${t("login.submit")}</button>
          <div class="hint" style="text-align:center">${t("login.firstAdminHint")}</div>
        </div>
      </div></div>`;
    const submit = async () => {
      const u = document.getElementById("lg_user").value.trim();
      const p = document.getElementById("lg_pass").value;
      if (!u || !p) { toast(t("common.error"), "err"); return; }
      const btn = document.getElementById("lg_btn"); btn.disabled = true;
      const res = await api("/api/login", { method: "POST", json: { username: u, password: p } });
      if (res.ok && res.data.ok) {
        toast(t("common.ok"), "ok"); boot();
      } else {
        btn.disabled = false;
        toast((res.data && res.data.error) || t("common.error"), "err");
      }
    };
    document.getElementById("lg_btn").onclick = submit;
    document.getElementById("lg_pass").addEventListener("keydown", e => { if (e.key === "Enter") submit(); });
  }

  // ---------------- 主框架 ----------------
  function renderShell() {
    const app = document.getElementById("app");
    let menuHtml = "";
    MENU.forEach(it => {
      if (it.group) menuHtml += `<div class="menu-group-title">${t(it.group)}</div>`;
      else menuHtml += `<div class="menu-item" data-route="${it.key}" onclick="location.hash='${it.route}'">
        <span class="icon">${icon(it.icon)}</span><span>${t("menu." + it.key)}</span></div>`;
    });
    const langBtns = `<button class="btn btn-sm ${STATE.lang === "zh" ? "" : "btn-ghost"}" id="lb_zh">中</button>
      <button class="btn btn-sm ${STATE.lang === "en" ? "" : "btn-ghost"}" id="lb_en">EN</button>`;
    app.innerHTML = `<div class="app-shell">
      <aside class="app-sidebar" id="sidebar">
        <div class="brand"><img class="brand-logo" src="/static/logo.png" alt="Alpha" /><span class="brand-text">Alpha</span></div>
        <nav class="app-menu">${menuHtml}</nav>
      </aside>
      <div class="app-main">
        <header class="app-topbar">
          <button class="icon-btn" id="menu_toggle" title="菜单">☰</button>
          <span class="title" id="pg_title"></span>
          <span class="spacer"></span>
          ${langBtns}
          <span class="tag blue">${esc(STATE.user)}</span>
          <button class="icon-btn" id="logout" title="退出">⏻</button>
        </header>
        <main class="app-content" id="content"></main>
      </div></div>
      <div class="sidebar-mask" id="mask"></div>`;
    document.getElementById("menu_toggle").onclick = () => {
      document.getElementById("sidebar").classList.toggle("mobile-open");
      document.getElementById("mask").classList.toggle("mobile-open");
    };
    document.getElementById("mask").onclick = () => {
      document.getElementById("sidebar").classList.remove("mobile-open");
      document.getElementById("mask").classList.remove("mobile-open");
    };
    document.getElementById("lb_zh").onclick = () => setLang("zh");
    document.getElementById("lb_en").onclick = () => setLang("en");
    document.getElementById("logout").onclick = async () => {
      await api("/api/logout", { method: "POST" }); boot();
    };
    window.addEventListener("hashchange", route);
    route();
  }
  async function setLang(lang) {
    if (lang === STATE.lang) return;
    STATE.lang = lang;
    const res = await api("/api/lang", { method: "POST", json: { lang } });
    if (res.ok) { renderShell(); }
  }

  // ---------------- 仪表盘 ----------------
  async function renderDashboard() {
    document.getElementById("pg_title").textContent = t("dash.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/dashboard");
    if (!res.ok) return;
    const d = res.data;
    const sc = d.scheduler || {};
    const cs = sc.current_slot || {};
    const repTag = sc.reported_this_slot
      ? `<span class="tag green">${t("dash.reported")}</span>`
      : `<span class="tag gray">${t("dash.not_reported")}</span>`;
    const slotHtml = cs.boss
      ? `${esc(cs.slot || "")} · ${esc(cs.boss)} · ${esc(cs.source)} · ${esc(cs.dispatcher || "")} ${repTag}`
      : `<span class="text-weak">-</span>`;
    const lr = sc.last_result || {};
    let logsHtml = "";
    (d.recent_logs || []).forEach(l => {
      logsHtml += `<tr><td class="mono">${esc(l.ts)}</td><td>${esc(l.level)}</td><td>${esc(l.kind)}</td><td>${esc(l.source)}</td><td>${esc(l.message)}</td></tr>`;
    });
    c.innerHTML = `
      <div class="stat-grid">
        <div class="stat-card"><div class="label">${t("dash.sources")}</div><div class="value">${d.sources.enabled}/${d.sources.total}</div></div>
        <div class="stat-card"><div class="label">${t("dash.active_dispatcher")}</div><div class="value" style="font-size:16px">${esc(d.active_dispatcher || "-")}</div></div>
        <div class="stat-card"><div class="label">${t("dash.spawn_recent")}</div><div class="value">${d.spawn_count_200}</div></div>
        <div class="stat-card"><div class="label">${t("dash.total_logs")}</div><div class="value">${d.total_logs}</div></div>
        <div class="stat-card"><div class="label">${t("dash.pokedex")}</div><div class="value" style="font-size:16px">${d.pokedex.pokemon}·${d.pokedex.abilities}·${d.pokedex.moves}</div></div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("dash.scheduler")}</h3></div>
        <div>${t("sched.status")}: <b>${sc.enabled ? t("dash.running") : t("dash.stopped")}</b> ·
          ${t("dash.last_run")}: ${esc(sc.last_run || "-")} ·
          ${t("sched.last_result")}: ${esc(lr.status || "-")} ${lr.boss ? "· " + esc(lr.boss) : ""}</div>
        <div style="margin-top:10px">${t("dash.current_slot")}: ${slotHtml}</div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("dash.recent_logs")}</h3></div>
        <div class="tbl-wrap"><table class="tbl"><thead><tr>
          <th>${t("log.time")}</th><th>${t("log.level")}</th><th>${t("log.type")}</th><th>${t("log.source")}</th><th>${t("log.message")}</th>
        </tr></thead><tbody>${logsHtml || `<tr><td colspan="5" class="empty">${t("dash.empty")}</td></tr>`}</tbody></table></div>
      </div>`;
  }

  // ---------------- 调试 ----------------
  async function ensurePokedex() {
    if (PK.loaded) return PK;
    const res = await api("/api/pokedex");
    if (res.ok) {
      PK.pokemon = res.data.pokemon; PK.abilities = res.data.abilities; PK.moves = res.data.moves;
      PK.loaded = true;
    }
    return PK;
  }
  async function renderDebug() {
    document.getElementById("pg_title").textContent = t("debug.title");
    const c = document.getElementById("content");
    const pk = await ensurePokedex();
    const pkOpts = pk.pokemon.map(p => `<option value="${esc(p.zh)}" data-en="${esc(p.en)}">${esc(p.zh)} / ${esc(p.en)}</option>`).join("");
    const abOpts = pk.abilities.map(a => `<option value="${esc(a.zh)}">${esc(a.zh)} / ${esc(a.en)}</option>`).join("");
    const mvOpts = pk.moves.map(m => `<option value="${esc(m.zh)}">${esc(m.zh)} / ${esc(m.en)}</option>`).join("");
    c.innerHTML = `
      <datalist id="pk_list">${pkOpts}</datalist>
      <datalist id="ab_list">${abOpts}</datalist>
      <datalist id="mv_list">${mvOpts}</datalist>
      <div class="panel">
        <div class="panel-head"><h3>${t("debug.title")}</h3></div>
        <div class="two-col">
          <div class="form">
            <div class="row"><label>${t("debug.pokemon")}</label><input id="d_pk" list="pk_list" placeholder="${t("debug.pokemon")}" /></div>
            <div class="row"><label>${t("debug.ability")}</label><input id="d_ab" list="ab_list" placeholder="${t("debug.ability")}" /></div>
            <div class="row"><label>${t("debug.gender")}</label>
              <select id="d_gender">
                <option value="dual">${t("g.dual")}</option>
                <option value="male">${t("g.male")}</option>
                <option value="female">${t("g.female")}</option>
                <option value="none">${t("g.none")}</option>
              </select></div>
            <div class="row"><label>${t("debug.moves")}</label>
              <input id="d_mv" list="mv_list" placeholder="${t("debug.moves")}" />
              <div class="hint">${t("debug.moves_hint")}</div>
              <div class="chips" id="d_mv_chips"></div></div>
            <div class="row"><label>${t("debug.egg")}</label><input id="d_egg" placeholder="${t("debug.egg")}" /></div>
          </div>
          <div class="form">
            <div class="row"><label>${t("debug.dispatcher")}</label><select id="d_disp"></select></div>
            <div class="row"><label>${t("debug.lang")}</label>
              <select id="d_lang"><option value="zh">中文</option><option value="en">English</option><option value="both">中英双语</option></select></div>
            <div class="row actions"><button class="btn" id="d_run">${t("debug.run")}</button></div>
            <div class="row"><label>${t("debug.resolved")}</label><pre class="report" id="d_resolved" style="min-height:40px"></pre></div>
          </div>
        </div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("debug.report")}</h3></div>
        <pre class="report" id="d_report">${t("dash.empty")}</pre>
      </div>`;

    const moves = [];
    const chipsEl = document.getElementById("d_mv_chips");
    function renderChips() {
      chipsEl.innerHTML = moves.map((m, i) =>
        `<span class="chip">${esc(m)}<button data-i="${i}">×</button></span>`).join("");
      chipsEl.querySelectorAll("button").forEach(b => b.onclick = () => { moves.splice(+b.dataset.i, 1); renderChips(); });
    }
    function addMove(v) {
      v = (v || "").trim();
      if (v && !moves.includes(v)) { moves.push(v); renderChips(); }
    }
    const mvInput = document.getElementById("d_mv");
    mvInput.addEventListener("keydown", e => { if (e.key === "Enter") { e.preventDefault(); addMove(mvInput.value); mvInput.value = ""; } });
    mvInput.addEventListener("blur", () => { if (mvInput.value.trim()) { addMove(mvInput.value); mvInput.value = ""; } });

    const dispSel = document.getElementById("d_disp");
    const dr = await api("/api/dispatchers");
    if (dr.ok) {
      dr.data.forEach(d => {
        const o = document.createElement("option");
        o.value = d.id; o.textContent = d.name + (d.active ? " ★" : "");
        if (d.active) o.selected = true;
        dispSel.appendChild(o);
      });
    }
    document.getElementById("d_run").onclick = async () => {
      const pk = document.getElementById("d_pk").value.trim();
      if (!pk) { toast(t("debug.pokemon"), "err"); return; }
      const payload = {
        pokemon: pk, ability: document.getElementById("d_ab").value.trim(),
        gender: document.getElementById("d_gender").value,
        moves: moves.slice(), egg_groups: document.getElementById("d_egg").value.trim().split(/[\s,，]+/).filter(Boolean),
        dispatcher_id: +dispSel.value, lang: document.getElementById("d_lang").value,
      };
      const btn = document.getElementById("d_run"); btn.disabled = true;
      const res = await api("/debug/run", { method: "POST", json: payload });
      btn.disabled = false;
      if (res.ok && res.data.ok) {
        const r = res.data.resolved;
        document.getElementById("d_resolved").textContent =
          `名称:${r.name} 图鉴:${r.pokemon_id}\n特性:${r.ability}\n技能:${r.moves.join("、")}\n蛋组:${r.egg_groups.join("、")}`;
        document.getElementById("d_report").textContent = res.data.report || "";
        toast(t("common.ok"), "ok");
      } else {
        document.getElementById("d_report").textContent = (res.data && res.data.error) || t("common.error");
        toast((res.data && res.data.error) || t("common.error"), "err");
      }
    };
  }

  // ---------------- 定时 ----------------
  async function renderScheduler() {
    document.getElementById("pg_title").textContent = t("sched.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/scheduler");
    if (!res.ok) return;
    const sc = res.data;
    const cs = sc.current_slot || {};
    c.innerHTML = `
      <div class="panel">
        <div class="panel-head"><h3>${t("sched.title")}</h3></div>
        <div class="form">
          <div class="row">
            <label>${t("sched.enabled")}</label>
            <div id="sw_box">${switchEl(sc.enabled, v => { pending.enabled = v; })}</div>
          </div>
          <div class="row"><label>${t("sched.interval")}</label><input id="s_int" type="number" min="10" value="${sc.interval || 60}" /></div>
          <div class="row actions">
            <button class="btn" id="s_save">${t("sched.save")}</button>
            <button class="btn btn-blue" id="s_debug">${t("sched.debug")}</button>
          </div>
          <div class="hint">${t("sched.debug_hint")}</div>
        </div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("dash.current_slot")}</h3></div>
        ${sc.reported_this_slot ? `<div class="alert success">${t("sched.reported_this_slot")}</div>` : ""}
        ${cs.boss ? `<div>${esc(cs.slot || "")} · <b>${esc(cs.boss)}</b> · 源=${esc(cs.source)} · 决策器=${esc(cs.dispatcher || "")} · ${esc(cs.time || "")}</div>`
          : `<div class="text-weak">-</div>`}
        <div style="margin-top:8px">${t("dash.last_run")}: ${esc(sc.last_run || "-")}</div>
        <div>${t("sched.last_result")}: ${esc((sc.last_result && sc.last_result.status) || "-")} ${sc.last_result && sc.last_result.boss ? "· " + esc(sc.last_result.boss) : ""}</div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("sched.debug_result")}</h3></div>
        <pre class="report" id="s_dbg">${t("dash.empty")}</pre>
      </div>`;
    let pending = { enabled: sc.enabled };
    document.getElementById("s_save").onclick = async () => {
      const interval = +document.getElementById("s_int").value || 60;
      const r = await api("/api/scheduler/set", { method: "POST", json: { enabled: pending.enabled, interval } });
      if (r.ok) toast(t("common.save"), "ok"); else toast(t("common.error"), "err");
    };
    document.getElementById("s_debug").onclick = async () => {
      const btn = document.getElementById("s_debug");
      const el = document.getElementById("s_dbg");
      btn.disabled = true;
      el.textContent = t("sched.debug_running");
      const r = await api("/api/scheduler/debug", { method: "POST" });
      if (!r.ok) { btn.disabled = false; el.textContent = (r.data && r.data.error) || t("common.error"); return; }
      // 后台执行，轮询结果（最多 90s）
      const t0 = Date.now();
      const timer = setInterval(async () => {
        const s = await api("/api/scheduler/debug");
        const st = (s.ok && s.data) || {};
        if (st.running) {
          el.textContent = `${t("sched.debug_running")} ${st.elapsed || 0}s`;
          if (Date.now() - t0 > 90000) { clearInterval(timer); btn.disabled = false; el.textContent = t("sched.debug_timeout"); }
          return;
        }
        clearInterval(timer); btn.disabled = false;
        const d = st.result || {};
        if (d.status === "hit" || d.status === "pushed") el.textContent = d.report || JSON.stringify(d, null, 2);
        else el.textContent = JSON.stringify(d, null, 2);
        toast(t("common.ok"), "ok");
      }, 1000);
    };
  }

  // ---------------- 头目源 ----------------
  async function renderSources() {
    document.getElementById("pg_title").textContent = t("src.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/sources");
    if (!res.ok) return;
    const srcs = res.data.sources || [];
    const adapters = res.data.adapters || [];
    let rows = srcs.map((s, i) => `
      <tr>
        <td><b>${esc(s.name)}</b></td>
        <td class="mono">${esc(s.adapter)}</td>
        <td class="num">${s.priority}</td>
        <td><div data-sw="${i}">${switchEl(s.enabled, v => setSrcEnabled(s.name, v))}</div></td>
        <td class="mono" style="font-size:12px">${esc(s.note || "")}</td>
        <td class="row-actions">
          <button class="btn btn-sm" data-act="edit" data-i="${i}">${t("common.edit")}</button>
          <button class="btn btn-sm btn-ghost" data-act="dl" data-i="${i}">${t("common.download")}</button>
          <button class="btn btn-sm btn-danger" data-act="del" data-i="${i}">${t("common.delete")}</button>
        </td>
      </tr>`).join("");
    c.innerHTML = `
      <div class="panel">
        <div class="panel-head"><h3>${t("src.title")}</h3>
          <span class="spacer"></span>
          <button class="btn btn-sm" id="src_add_btn">${t("src.add")}</button>
          <label class="btn btn-sm btn-blue">${t("src.upload_adapter")}<input type="file" id="src_up" accept=".py" style="display:none"></label>
          <button class="btn btn-sm btn-ghost" id="src_tpl_btn">${t("src.download_template")}</button>
        </div>
        <div class="hint">${t("src.upload_hint")}</div>
        <div class="tbl-wrap" style="margin-top:12px"><table class="tbl"><thead><tr>
          <th>${t("src.name")}</th><th>${t("src.adapter")}</th><th>${t("src.priority")}</th>
          <th>${t("src.status")}</th><th>${t("src.note")}</th><th>${t("common.edit")}</th>
        </tr></thead><tbody>${rows || `<tr><td colspan="6" class="empty">${t("dash.empty")}</td></tr>`}</tbody></table></div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("src.adapter_files")}</h3></div>
        <div class="chips">${adapters.map(a => `<span class="chip">${esc(a)}.py</span>`).join("") || `<span class="text-weak">-</span>`}</div>
      </div>`;

    // 事件委托：不再用内联 onclick（内联只能调全局函数，闭包里的函数浏览器找不到）
    c.querySelectorAll("[data-act]").forEach(btn => {
      btn.onclick = () => {
        const s = srcs[+btn.dataset.i];
        if (!s) return;
        const act = btn.dataset.act;
        if (act === "edit") showEditSource(s, adapters);
        else if (act === "dl") srcDownload(s.adapter);
        else if (act === "del") srcDelete(s.name);
      };
    });
    document.getElementById("src_add_btn").onclick = () => showEditSource(null, adapters);
    document.getElementById("src_tpl_btn").onclick = () => window.open("/api/sources/template", "_blank");
    document.getElementById("src_up").onchange = e => {
      const f = e.target.files[0]; if (!f) return;
      const fd = new FormData(); fd.append("file", f); fd.append("name", f.name.replace(/\.py$/, ""));
      api("/api/sources/upload", { method: "POST", body: fd }).then(r => {
        if (r.ok) { toast(t("common.ok"), "ok"); renderSources(); }
        else toast((r.data && r.data.error) || t("common.error"), "err");
      });
    };
  }
  function setSrcEnabled(name, v) {
    api(`/api/sources/${encodeURIComponent(name)}/enable`, { method: "POST", json: { enabled: v } })
      .then(r => { if (r.ok) toast(t("common.save"), "ok"); else toast(t("common.error"), "err"); });
  }
  function srcDelete(name) {
    if (!confirm(t("common.confirm") + " " + name + "?")) return;
    api(`/api/sources/${encodeURIComponent(name)}`, { method: "DELETE" })
      .then(r => { if (r.ok) { toast(t("common.ok"), "ok"); renderSources(); } else toast(t("common.error"), "err"); });
  }
  function srcDownload(adapter) {
    if (!adapter) return;
    window.open(`/api/sources/${encodeURIComponent(adapter)}/download`, "_blank");
  }
  /** 新增 / 编辑源：同一个弹窗，传 s=null 表示新增。 */
  function showEditSource(s, adapters) {
    const isNew = !s;
    const cur = s || { name: "", adapter: "", priority: 50, enabled: true, options: {} };
    const adapterOpts = (adapters || []).map(a =>
      `<option value="${esc(a)}" ${a === cur.adapter ? "selected" : ""}>${esc(a)}</option>`).join("");
    const body = `
      <div class="form">
        <div class="row"><label>${t("src.add_name")}</label>
          <input id="ed_name" value="${esc(cur.name)}" placeholder="唯一标识，如 alphapedia" /></div>
        <div class="row"><label>${t("src.add_adapter")}</label>
          <select id="ed_adapter">${adapterOpts || `<option value="${esc(cur.adapter)}">${esc(cur.adapter)}</option>`}</select>
          <div class="hint">${t("src.adapter_hint")}</div></div>
        <div class="row"><label>${t("src.add_priority")}</label>
          <input id="ed_pri" type="number" value="${cur.priority}" />
          <div class="hint">${t("src.priority_hint")}</div></div>
        <div class="row"><label>${t("src.add_target")}</label>
          <input id="ed_target" value="${esc((cur.options || {}).target || "")}" placeholder="https://..." /></div>
        <div class="row"><label>${t("src.status")}</label>
          <select id="ed_enabled">
            <option value="1" ${cur.enabled ? "selected" : ""}>${t("common.enable")}</option>
            <option value="0" ${cur.enabled ? "" : "selected"}>${t("common.disable")}</option>
          </select></div>
      </div>`;
    openModal(isNew ? t("src.add") : t("src.edit") + "：" + cur.name, body, mask => {
      const name = mask.querySelector("#ed_name").value.trim();
      const adapter = mask.querySelector("#ed_adapter").value.trim();
      if (!name || !adapter) { toast(t("src.need_name_adapter"), "err"); return false; }
      const payload = {
        name, adapter,
        priority: +mask.querySelector("#ed_pri").value || 50,
        target: mask.querySelector("#ed_target").value.trim(),
        enabled: mask.querySelector("#ed_enabled").value === "1",
      };
      const done = r => {
        if (r.ok) { toast(t("common.save"), "ok"); renderSources(); }
        else toast((r.data && r.data.error) || t("common.error"), "err");
      };
      if (isNew) {
        api("/api/sources", { method: "POST", json: payload }).then(done);
      } else {
        api(`/api/sources/${encodeURIComponent(cur.name)}/edit`, { method: "POST", json: payload }).then(done);
      }
      return true;
    });
  }

  // ---------------- 决策器（分发器） ----------------
  async function renderDispatchers() {
    document.getElementById("pg_title").textContent = t("disp.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/dispatchers");
    if (!res.ok) return;
    const list = res.data;
    let rows = list.map((d, i) => `
      <tr>
        <td><b>${esc(d.name)}</b> ${d.is_builtin ? `<span class="tag gray">${t("disp.builtin")}</span>` : ""}</td>
        <td class="num">${d.priority}</td>
        <td>${switchEl(!!d.enabled, v => api(`/api/dispatchers/${d.id}/enable`, { method: "POST", json: { enabled: v } }).then(() => toast(t("common.save"), "ok")))}</td>
        <td>${d.active ? `<span class="tag yellow">★ ${t("disp.active")}</span>` : `<span class="tag gray">-</span>`}</td>
        <td style="font-size:12px">${esc(d.description || "")}</td>
        <td class="row-actions">
          ${d.active ? "" : `<button class="btn btn-sm btn-blue" data-act="active" data-i="${i}">${t("disp.set_active")}</button>`}
          <button class="btn btn-sm" data-act="edit" data-i="${i}">${t("common.edit")}</button>
          <button class="btn btn-sm btn-ghost" data-act="dl" data-i="${i}">${t("common.download")}</button>
          ${d.is_builtin ? "" : `<button class="btn btn-sm btn-danger" data-act="del" data-i="${i}">${t("common.delete")}</button>`}
        </td>
      </tr>`).join("");
    c.innerHTML = `
      <div class="panel">
        <div class="panel-head"><h3>${t("disp.title")}</h3>
          <span class="spacer"></span>
          <label class="btn btn-sm btn-blue">${t("disp.upload")}<input type="file" id="disp_up" accept=".py" style="display:none"></label>
          <button class="btn btn-sm btn-ghost" id="disp_tpl_btn">${t("disp.download_template")}</button>
        </div>
        <div class="hint">${t("disp.hint")}</div>
        <div class="tbl-wrap" style="margin-top:12px"><table class="tbl"><thead><tr>
          <th>${t("disp.name")}</th><th>${t("disp.priority")}</th><th>${t("common.enable")}</th>
          <th>${t("disp.active")}</th><th>${t("disp.desc")}</th><th>${t("common.edit")}</th>
        </tr></thead><tbody>${rows}</tbody></table></div>
      </div>`;
    c.querySelectorAll("[data-act]").forEach(btn => {
      btn.onclick = () => {
        const d = list[+btn.dataset.i];
        if (!d) return;
        const act = btn.dataset.act;
        if (act === "active") dispActivate(d.id);
        else if (act === "edit") showEditDispatcher(d);
        else if (act === "dl") dispDownload(d.id);
        else if (act === "del") dispDelete(d.id);
      };
    });
    document.getElementById("disp_tpl_btn").onclick = () => window.open("/api/dispatchers/template", "_blank");
    document.getElementById("disp_up").onchange = e => {
      const f = e.target.files[0]; if (!f) return;
      const fd = new FormData(); fd.append("file", f); fd.append("name", f.name.replace(/\.py$/, ""));
      api("/api/dispatchers/upload", { method: "POST", body: fd })
        .then(r => { if (r.ok) { toast(t("common.ok"), "ok"); renderDispatchers(); } else toast((r.data && r.data.error) || t("common.error"), "err"); });
    };
  }
  function dispActivate(id) {
    api(`/api/dispatchers/${id}/activate`, { method: "POST" })
      .then(r => { if (r.ok) { toast(t("common.save"), "ok"); renderDispatchers(); } else toast(t("common.error"), "err"); });
  }
  function dispDelete(id) {
    if (!confirm(t("common.confirm") + "?")) return;
    api(`/api/dispatchers/${id}/delete`, { method: "POST" })
      .then(r => { if (r.ok) { toast(t("common.ok"), "ok"); renderDispatchers(); } else toast(t("common.error"), "err"); });
  }
  function dispDownload(id) { window.open(`/api/dispatchers/${id}/download`, "_blank"); }
  function showEditDispatcher(d) {
    const body = `
      <div class="form">
        <div class="row"><label>${t("disp.name")}</label>
          <input id="dd_name" value="${esc(d.name)}" /></div>
        <div class="row"><label>${t("disp.priority")}</label>
          <input id="dd_pri" type="number" value="${d.priority}" />
          <div class="hint">${t("src.priority_hint")}</div></div>
        <div class="row"><label>${t("disp.desc")}</label>
          <input id="dd_desc" value="${esc(d.description || "")}" /></div>
      </div>`;
    openModal(t("common.edit") + "：" + d.name, body, mask => {
      api(`/api/dispatchers/${d.id}/edit`, {
        method: "POST",
        json: {
          name: mask.querySelector("#dd_name").value.trim() || d.name,
          priority: +mask.querySelector("#dd_pri").value || d.priority,
          description: mask.querySelector("#dd_desc").value.trim(),
        },
      }).then(r => { if (r.ok) { toast(t("common.save"), "ok"); renderDispatchers(); } else toast(t("common.error"), "err"); });
      return true;
    });
  }

  // ---------------- 分发渠道 ----------------
  function chIcon(type) {
    const P = {
      wxpusher: '<path d="M21 11.5a8.4 8.4 0 0 1-9 8.4 9.4 9.4 0 0 1-2.5-.3L4 21l1.5-4A8 8 0 0 1 3 11.5 8.4 8.4 0 0 1 12 3a8.4 8.4 0 0 1 9 8.5Z"/>',
      webhook: '<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>',
      serverchan: '<path d="M3 6.5 12 12l9-5.5"/><rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="M3 17l6-4.5M21 17l-6-4.5"/>',
    };
    return `<svg class="ico" viewBox="0 0 24 24" aria-hidden="true">${P[type] || P.webhook}</svg>`;
  }
  let CH_TYPES = {};
  let CH_LIST = [];

  async function renderChannels() {
    document.getElementById("pg_title").textContent = t("ch.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/channels");
    if (!res.ok) return;
    CH_TYPES = res.data.types || {};
    CH_LIST = res.data.channels || [];
    drawChannels();
  }

  function chSummary(ch) {
    const cfg = ch.config || {};
    if (ch.type === "wxpusher") return t("ch.summary_topic") + ": " + (cfg.topic_ids || "-");
    if (ch.type === "webhook") return cfg.url || "-";
    if (ch.type === "serverchan") return cfg.send_key ? "SendKey " + cfg.send_key : "-";
    return "-";
  }

  function drawChannels() {
    const c = document.getElementById("content");
    const cards = CH_LIST.map((ch, i) => {
      const meta = CH_TYPES[ch.type] || {};
      return `
        <div class="ch-card ${ch.enabled ? "" : "disabled"}">
          <div class="ch-top">
            <div class="ch-icon ${esc(ch.type)}">${chIcon(ch.type)}</div>
            <div class="ch-name">${esc(ch.name)}</div>
            <div data-csw="${i}">${switchEl(!!ch.enabled, v => chSetEnabled(ch.id, v))}</div>
          </div>
          <div>
            <span class="tag ${ch.enabled ? "green" : "gray"}">${esc(meta.label || ch.type)}</span>
            ${ch.enabled ? "" : `<span class="tag gray">${t("common.disable")}</span>`}
          </div>
          <div class="ch-desc">${esc(meta.desc || "")}</div>
          <div class="ch-meta">${esc(chSummary(ch))}</div>
          <div class="ch-actions">
            <button class="btn btn-sm btn-green" data-cact="test" data-i="${i}">${t("ch.test")}</button>
            <button class="btn btn-sm" data-cact="edit" data-i="${i}">${t("common.edit")}</button>
            <button class="btn btn-sm btn-danger" data-cact="del" data-i="${i}">${t("common.delete")}</button>
          </div>
        </div>`;
    }).join("");

    c.innerHTML = `
      <div class="panel">
        <div class="panel-head"><h3>${t("ch.title")}</h3>
          <span class="spacer"></span>
          <button class="btn btn-sm" id="ch_add">${t("ch.add")}</button>
          <button class="btn btn-sm btn-ghost" id="ch_test_all">${t("ch.test_all")}</button>
        </div>
        <div class="hint">${t("ch.hint")}</div>
      </div>
      <div class="ch-grid">${cards || `<div class="panel"><div class="empty">${t("ch.empty")}</div></div>`}</div>`;

    c.querySelectorAll("[data-cact]").forEach(btn => {
      btn.onclick = () => {
        const ch = CH_LIST[+btn.dataset.i];
        if (!ch) return;
        const act = btn.dataset.cact;
        if (act === "test") chTest(ch);
        else if (act === "edit") showEditChannel(ch);
        else if (act === "del") chDelete(ch);
      };
    });
    document.getElementById("ch_add").onclick = () => showEditChannel(null);
    document.getElementById("ch_test_all").onclick = async () => {
      const r = await api("/api/channels/test-all", { method: "POST" });
      if (!r.ok) { toast(t("common.error"), "err"); return; }
      const d = r.data;
      toast(t("ch.test_result") + ": " + (d.sent || 0) + " " + t("ch.ok") + " / " + (d.failed || 0) + " " + t("ch.fail"),
            d.failed ? "err" : "ok");
      renderLogsIfCurrent();
    };
  }

  function renderLogsIfCurrent() { /* 日志页自行刷新，这里无需处理 */ }

  function chSetEnabled(id, v) {
    api(`/api/channels/${id}/enable`, { method: "POST", json: { enabled: v } }).then(r => {
      if (r.ok) { toast(t("common.save"), "ok"); renderChannels(); } else toast(t("common.error"), "err");
    });
  }
  function chDelete(ch) {
    if (!confirm(t("common.confirm") + " " + ch.name + "?")) return;
    api(`/api/channels/${ch.id}`, { method: "DELETE" })
      .then(r => { if (r.ok) { toast(t("common.ok"), "ok"); renderChannels(); } else toast(t("common.error"), "err"); });
  }
  async function chTest(ch) {
    toast(t("ch.testing"), "info");
    const r = await api(`/api/channels/${ch.id}/test`, { method: "POST" });
    if (r.ok && r.data.ok) toast(ch.name + " " + t("ch.test_ok"), "ok");
    else toast(ch.name + " " + t("ch.test_fail") + ": " + ((r.data && r.data.error) || ""), "err");
  }

  /** 新增 / 编辑渠道。ch=null 为新增。 */
  function showEditChannel(ch) {
    const isNew = !ch;
    const cur = ch || { name: "", type: "wxpusher", enabled: true, config: {} };
    const typeOpts = Object.keys(CH_TYPES).map(k =>
      `<option value="${k}" ${k === cur.type ? "selected" : ""}>${esc(CH_TYPES[k].label)}</option>`).join("");

    const fieldsHtml = type => (CH_TYPES[type] || { fields: [] }).fields.map(f => {
      const val = (cur.config || {})[f.key] || "";
      if (f.type === "textarea") {
        return `<div class="row"><label>${esc(f.label)}</label>
          <textarea id="cf_${f.key}" placeholder="${esc(f.placeholder || "")}">${esc(val)}</textarea></div>`;
      }
      return `<div class="row"><label>${esc(f.label)}</label>
        <input id="cf_${f.key}" type="${f.type === "password" ? "password" : "text"}"
          value="${esc(val)}" placeholder="${esc(f.placeholder || "")}" /></div>`;
    }).join("");

    const body = `
      <div class="form">
        <div class="row"><label>${t("ch.name")}</label>
          <input id="ch_name" value="${esc(cur.name)}" placeholder="${t("ch.name_ph")}" /></div>
        <div class="row"><label>${t("ch.type")}</label>
          <select id="ch_type">${typeOpts}</select>
          <div class="hint" id="ch_type_desc">${esc((CH_TYPES[cur.type] || {}).desc || "")}</div></div>
        <div id="ch_fields">${fieldsHtml(cur.type)}</div>
        <div class="row"><label>${t("src.status")}</label>
          <select id="ch_enabled">
            <option value="1" ${cur.enabled ? "selected" : ""}>${t("common.enable")}</option>
            <option value="0" ${cur.enabled ? "" : "selected"}>${t("common.disable")}</option>
          </select></div>
      </div>`;

    const mask = openModal(isNew ? t("ch.add") : t("common.edit") + "：" + cur.name, body, m => {
      const name = m.querySelector("#ch_name").value.trim();
      if (!name) { toast(t("ch.need_name"), "err"); return false; }
      const type = m.querySelector("#ch_type").value;
      const config = {};
      (CH_TYPES[type] || { fields: [] }).fields.forEach(f => {
        const el = m.querySelector("#cf_" + f.key);
        if (el) config[f.key] = el.value.trim();
      });
      const enabled = m.querySelector("#ch_enabled").value === "1";
      const done = r => {
        if (r.ok) { toast(t("common.save"), "ok"); renderChannels(); }
        else toast((r.data && r.data.error) || t("common.error"), "err");
      };
      if (isNew) api("/api/channels", { method: "POST", json: { name, type, config, enabled } }).then(done);
      else api(`/api/channels/${cur.id}/edit`, { method: "POST", json: { name, config, enabled } }).then(done);
      return true;
    });

    // 切换类型时重渲染字段
    const sel = mask.querySelector("#ch_type");
    sel.onchange = () => {
      mask.querySelector("#ch_fields").innerHTML = fieldsHtml(sel.value);
      mask.querySelector("#ch_type_desc").textContent = (CH_TYPES[sel.value] || {}).desc || "";
    };
  }

  // ---------------- 日志 ----------------
  async function renderLogs() {
    document.getElementById("pg_title").textContent = t("log.title");
    const c = document.getElementById("content");
    c.innerHTML = `
      <div class="toolbar">
        <select id="log_filter" style="width:auto">
          <option value="">${t("log.filter_type")}</option>
          <option value="query">query · ${t("log.kind_query")}</option>
          <option value="spawn">spawn · ${t("log.kind_spawn")}</option>
          <option value="scheduler">scheduler</option>
          <option value="channel">channel · ${t("log.kind_channel")}</option>
          <option value="source">source</option><option value="dispatcher">dispatcher</option>
          <option value="debug">debug</option><option value="log">log</option><option value="system">system</option>
        </select>
        <select id="log_src" style="width:auto"><option value="">${t("log.filter_source")}</option></select>
        <button class="btn btn-sm btn-ghost" id="log_refresh">${t("common.refresh")}</button>
        <span class="spacer"></span>
        <label class="inline-sw">${t("log.query_log")}</label>
        <span id="log_sw"></span>
        <label>${t("log.clean_days")}</label><input id="log_days" type="number" value="3" style="width:70px" />
        <button class="btn btn-sm btn-ghost" id="log_clean">${t("log.clean")}</button>
      </div>
      <div class="hint" style="margin-bottom:12px">${t("log.query_hint")}</div>
      <div class="panel"><div class="tbl-wrap"><table class="tbl"><thead><tr>
        <th>${t("log.time")}</th><th>${t("log.level")}</th><th>${t("log.type")}</th>
        <th>${t("log.source")}</th><th>${t("log.message")}</th>
      </tr></thead><tbody id="log_body"><tr><td colspan="5" class="empty">${t("common.loading")}</td></tr></tbody></table></div></div>`;

    let srcFiltered = "";
    const load = async () => {
      const kind = document.getElementById("log_filter").value;
      const res = await api("/api/logs?limit=400"
        + (kind ? "&kind=" + encodeURIComponent(kind) : "")
        + (srcFiltered ? "&source=" + encodeURIComponent(srcFiltered) : ""));
      const body = document.getElementById("log_body");
      if (!res.ok) { body.innerHTML = `<tr><td colspan="5" class="empty">${t("common.error")}</td></tr>`; return; }
      // 填充来源下拉（保留当前选择）
      const sel = document.getElementById("log_src");
      const cur = sel.value;
      const srcs = res.data.sources || [];
      sel.innerHTML = `<option value="">${t("log.filter_source")}</option>`
        + srcs.map(s => `<option value="${esc(s)}" ${s === cur ? "selected" : ""}>${esc(s)}</option>`).join("");
      srcFiltered = cur;

      const lvCls = { error: "red", warning: "yellow", info: "blue", debug: "gray" };
      const logs = res.data.logs || [];
      body.innerHTML = logs.map(l => `
        <tr>
          <td class="mono" style="white-space:nowrap">${esc(l.ts)}</td>
          <td><span class="tag ${lvCls[l.level] || "gray"}">${esc(l.level)}</span></td>
          <td><span class="tag ${l.kind === "query" ? "cyan" : l.kind === "spawn" ? "green" : "gray"}">${esc(l.kind)}</span></td>
          <td class="mono" style="font-size:12px">${esc(l.source)}</td>
          <td style="font-size:13px">${esc(l.message)}</td>
        </tr>`).join("")
        || `<tr><td colspan="5" class="empty">${t("dash.empty")}</td></tr>`;
      return res.data;
    };

    const first = await load();
    // 源查询日志开关
    const swBox = document.getElementById("log_sw");
    swBox.innerHTML = switchEl(first && first.query_log, v => {
      api("/api/logs/state", { method: "POST", json: { enabled: v } })
        .then(r => { if (r.ok) { toast(t("common.save"), "ok"); load(); } else toast(t("common.error"), "err"); });
    });

    document.getElementById("log_filter").onchange = () => { load(); };
    document.getElementById("log_src").onchange = e => { srcFiltered = e.target.value; load(); };
    document.getElementById("log_refresh").onclick = () => load();
    document.getElementById("log_clean").onclick = async () => {
      const days = +document.getElementById("log_days").value || 3;
      const r = await api("/api/logs/clean", { method: "POST", json: { days } });
      if (r.ok) { toast(t("common.ok") + " (" + (r.data.deleted || 0) + ")", "ok"); load(); } else toast(t("common.error"), "err");
    };
  }

  // ---------------- 系统配置 ----------------
  async function renderSystem() {
    document.getElementById("pg_title").textContent = t("sys.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/system");
    if (!res.ok) return;
    const d = res.data;
    const tzOpts = d.timezones.map(z => `<option value="${z}" ${z === d.timezone ? "selected" : ""}>${z}</option>`).join("");
    const pushOpts = ["zh", "en", "both"].map(l => `<option value="${l}" ${l === d.push_lang ? "selected" : ""}>${t("sys.lang." + l)}</option>`).join("");
    const panelOpts = ["zh", "en"].map(l => `<option value="${l}" ${l === d.panel_lang ? "selected" : ""}>${t("sys.lang." + l)}</option>`).join("");
    const envRows = (d.envs || []).map(e => `
      <div class="row">
        <label>${esc(e.label)}
          ${e.set ? `<span class="tag green">${t("sys.env_set")}</span>` : `<span class="tag gray">${t("sys.env_unset")}</span>`}</label>
        <input id="env_${esc(e.key)}" data-envkey="${esc(e.key)}" type="text"
          placeholder="${e.set ? esc(e.preview) : t("sys.env_ph")}" />
        <div class="hint">${esc(e.desc)}　${t("sys.env_hint")}</div>
      </div>`).join("");

    c.innerHTML = `
      <div class="panel">
        <div class="panel-head"><h3>${t("sys.title")}</h3></div>
        <div class="form" style="max-width:460px">
          <div class="row"><label>${t("sys.timezone")}</label><select id="sy_tz">${tzOpts}</select></div>
          <div class="row"><label>${t("sys.push_lang")}</label><select id="sy_push">${pushOpts}</select></div>
          <div class="row"><label>${t("sys.panel_lang")}</label><select id="sy_panel">${panelOpts}</select></div>
          <div class="row actions"><button class="btn" id="sy_save">${t("sys.save")}</button></div>
        </div>
      </div>
      <div class="panel">
        <div class="panel-head"><h3>${t("sys.env")}</h3></div>
        <div class="hint" style="margin-bottom:12px">${t("sys.env_desc")}</div>
        <div class="form" style="max-width:460px">${envRows}</div>
      </div>`;
    document.getElementById("sy_save").onclick = async () => {
      const envs = {};
      document.querySelectorAll("[data-envkey]").forEach(el => {
        const v = el.value.trim();
        if (v) envs[el.dataset.envkey] = v;   // 留空表示不改
      });
      const payload = {
        timezone: document.getElementById("sy_tz").value,
        push_lang: document.getElementById("sy_push").value,
        panel_lang: document.getElementById("sy_panel").value,
        envs,
      };
      const r = await api("/api/system/save", { method: "POST", json: payload });
      if (r.ok) { STATE.lang = payload.panel_lang; toast(t("common.save"), "ok"); renderShell(); }
      else toast(t("common.error"), "err");
    };
  }

  // ---------------- 关于 ----------------
  async function renderAbout() {
    document.getElementById("pg_title").textContent = t("about.title");
    const c = document.getElementById("content");
    c.innerHTML = `<div class="empty">${t("common.loading")}</div>`;
    const res = await api("/api/about");
    if (!res.ok) return;
    const d = res.data;
    // 后端每个字段都给了 {zh,en}，这里按当前面板语言取
    const L = v => (v && typeof v === "object" && !Array.isArray(v))
      ? (v[STATE.lang] || v.zh || "") : (v || "");
    const proj = L(d.project) || {};
    const ex = L(d.examples) || {};
    const thanks = (L(d.thanks) || []).map(t2 => `<li><b>${esc(t2.name)}</b> — ${esc(t2.note)}</li>`).join("");
    // <ol> 自己会编号，别再手写序号，否则会出现「1. 1. xxx」
    const flow = (proj.flow || []).map(f =>
      `<li style="margin-bottom:10px"><b>${esc(f.name)}</b> — ${esc(f.note)}</li>`).join("");
    const exBlock = (key) => {
      const e = ex[key] || {};
      const steps = (e.steps || []).map(s =>
        `<li style="margin-bottom:8px">${esc(s)}</li>`).join("");
      return `<div class="panel">
        <div class="panel-head"><h3>${esc(e.title || "")}</h3></div>
        <ol style="line-height:1.9; padding-left:20px; margin:0 0 14px">${steps}</ol>
        <pre class="report">${esc(e.code || "")}</pre>
      </div>`;
    };
    const TABS = [
      ["project", t("about.tab.project")],
      ["thanks", t("about.thanks")],
      ["examples", t("about.tab.examples")],
    ];
    const tabHtml = TABS.map(([k, label], i) =>
      `<button class="tab" data-abouttab="${k}" ${i === 0 ? 'data-on="1"' : ""}>${esc(label)}</button>`).join("");
    const head = `<div class="panel">
        <div class="panel-head"><h3>${esc(L(d.title))} <span class="tag gray">v${esc(d.version)}</span></h3></div>
        <p style="line-height:1.8">${esc(L(d.desc))}</p>
        <div class="row"><label>${t("about.built_with")}</label><div>${esc(L(d.built_with))}</div></div>
        <div class="row"><label>${t("about.git")}</label><div><a href="${esc(d.git)}" target="_blank" rel="noreferrer">${esc(d.git)}</a></div></div>
        <div class="row"><label>${t("about.credits")}</label><div>${esc(L(d.credits))}</div></div>
      </div>`;
    const panes = {
      project: `<div class="panel">
          <div class="panel-head"><h3>${t("about.tab.project")}</h3></div>
          <p style="line-height:1.9">${esc(proj.intro || "")}</p>
          <ol style="line-height:1.9; padding-left:20px; margin:14px 0 0">${flow}</ol>
        </div>`,
      thanks: `<div class="panel">
          <div class="panel-head"><h3>${t("about.thanks")}</h3></div>
          <ul style="line-height:2; padding-left:20px">${thanks}</ul>
        </div>`,
      examples: exBlock("source") + exBlock("dispatcher"),
    };
    c.innerHTML = `${head}
      <div class="tabs">${tabHtml}</div>
      <div id="about_pane">${panes.project}</div>`;
    c.querySelectorAll("[data-abouttab]").forEach(btn => {
      btn.onclick = () => {
        const k = btn.dataset.abouttab;
        c.querySelectorAll("[data-abouttab]").forEach(b2 => {
          if (b2 === btn) b2.dataset.on = "1"; else delete b2.dataset.on;
        });
        c.querySelector("#about_pane").innerHTML = panes[k] || "";
      };
    });
  }

  // ---------------- 启动 ----------------
  async function boot() {
    const res = await api("/api/me");
    if (res.ok && res.data.authenticated) {
      STATE.user = res.data.user; STATE.role = res.data.role;
      STATE.lang = res.data.lang || "zh";
      renderShell();
    } else {
      renderLogin();
    }
  }
  boot();
})();
