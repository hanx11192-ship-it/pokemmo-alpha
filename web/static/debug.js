"use strict";

let PK = null;            // 图鉴清单
let moves = [];           // 已选技能（中文名）

function norm(s) { return (s || "").trim().toLowerCase(); }

function findPokemon(q) {
  if (!PK) return null;
  q = norm(q);
  return PK.pokemon.find(p => norm(p.zh) === q || norm(p.en) === q) || null;
}
function findAbility(q) {
  if (!PK) return null;
  q = norm(q);
  return PK.abilities.find(a => norm(a.zh) === q || norm(a.en) === q) || null;
}

function fillDatalists() {
  const pl = document.getElementById("pokemon-list");
  const al = document.getElementById("ability-list");
  const ml = document.getElementById("move-list");
  PK.pokemon.forEach(p => {
    [p.zh, p.en].forEach(n => { if (n) pl.appendChild(new Option(n)); });
  });
  PK.abilities.forEach(a => {
    [a.zh, a.en].forEach(n => { if (n) al.appendChild(new Option(n)); });
  });
  PK.moves.forEach(m => {
    [m.zh, m.en].forEach(n => { if (n) ml.appendChild(new Option(n)); });
  });
}

function refreshDispatchers() {
  fetch("/api/dispatchers").then(r => r.json()).then(rows => {
    const sel = document.getElementById("dispatcher");
    sel.innerHTML = "";
    rows.filter(r => r.enabled).forEach(r => {
      const o = new Option(`${r.name}${r.active ? "（当前生效）" : ""}`, r.id);
      sel.appendChild(o);
    });
    if (!sel.options.length) sel.appendChild(new Option("（无可用分发器）", ""));
  });
}

function onPokemonChange() {
  const p = findPokemon(document.getElementById("pokemon").value);
  if (!p) return;
  if (p.ha) document.getElementById("ability").value = p.ha;
  if (p.eg && p.eg.length) document.getElementById("egg_groups").value = p.eg.join(" / ");
}

function renderChips() {
  const box = document.getElementById("move-chips");
  box.innerHTML = "";
  moves.forEach((m, i) => {
    const c = document.createElement("span");
    c.className = "chip";
    c.textContent = m;
    const x = document.createElement("button");
    x.type = "button"; x.textContent = "×";
    x.onclick = () => { moves.splice(i, 1); renderChips(); };
    c.appendChild(x);
    box.appendChild(c);
  });
}

function addMove() {
  const inp = document.getElementById("move-input");
  let v = inp.value.trim();
  if (!v) return;
  v = v.replace(/[，,]$/, "");
  const hit = PK ? (PK.moves.find(m => norm(m.zh) === norm(v) || norm(m.en) === norm(v))) : null;
  const name = hit ? (hit.zh || hit.en) : v;   // 图鉴没有则原样保留
  if (name && !moves.includes(name) && moves.length < 4) moves.push(name);
  inp.value = "";
  renderChips();
}

function submitDebug(e) {
  e.preventDefault();
  const status = document.getElementById("status");
  status.textContent = "运行中…"; status.className = "status";

  const payload = {
    pokemon: document.getElementById("pokemon").value.trim(),
    ability: document.getElementById("ability").value.trim(),
    gender: document.getElementById("gender").value,
    egg_groups: document.getElementById("egg_groups").value.split("/").map(s => s.trim()).filter(Boolean),
    moves: moves,
    dispatcher_id: document.getElementById("dispatcher").value || null,
    lang: document.getElementById("lang").value,
  };

  fetch("/debug/run", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload),
  }).then(r => r.json()).then(res => {
    if (!res.ok) {
      status.textContent = res.error || "失败"; status.className = "status err";
      document.getElementById("report").textContent = "—";
      document.getElementById("resolved").textContent = "";
      return;
    }
    status.textContent = `✓ 由「${res.dispatcher}」生成`; status.className = "status ok";
    document.getElementById("report").textContent = res.report;
    const r = res.resolved;
    document.getElementById("resolved").innerHTML =
      `<strong>解析结果：</strong> ${r.name}（id ${r.pokemon_id}） · 特性 ${r.ability}（id ${r.ability_id}）` +
      ` · 技能 ${r.moves.join("、")}（id ${r.move_ids.join("、")}） · 蛋组 ${r.egg_groups.join("、") || "—"}`;
  }).catch(err => {
    status.textContent = "请求出错：" + err; status.className = "status err";
  });
}

document.addEventListener("DOMContentLoaded", () => {
  fetch("/api/pokedex").then(r => r.json()).then(data => {
    PK = data; fillDatalists();
    document.getElementById("pokemon").addEventListener("change", onPokemonChange);
    document.getElementById("move-input").addEventListener("keydown", e => {
      if (e.key === "Enter" || e.key === ",") { e.preventDefault(); addMove(); }
    });
    document.getElementById("debug-form").addEventListener("submit", submitDebug);
    refreshDispatchers();
  });
});
