"use strict";

function load() {
  fetch("/api/dispatchers").then(r => r.json()).then(rows => render(rows));
}

function render(rows) {
  const body = document.getElementById("disp-body");
  body.innerHTML = "";
  rows.forEach(r => {
    const tr = document.createElement("tr");

    const status = r.enabled
      ? '<span class="tag on">启用</span>'
      : '<span class="tag off">关闭</span>';
    const active = r.active ? '<span class="tag star">★ 生效</span>' : "";
    const builtin = r.is_builtin ? "是" : "否";

    tr.innerHTML = `
      <td>${r.name}</td>
      <td><code>${r.filename}</code></td>
      <td>${r.priority}</td>
      <td>${status}</td>
      <td>${active}</td>
      <td>${builtin}</td>
      <td>${r.description || ""}</td>
      <td class="ops"></td>`;

    const ops = tr.querySelector(".ops");
    const btnEnable = document.createElement("button");
    btnEnable.className = "mini ghost";
    btnEnable.textContent = r.enabled ? "关闭" : "启用";
    btnEnable.onclick = () => toggleEnable(r.id, !r.enabled);
    ops.appendChild(btnEnable);

    if (!r.active) {
      const btnActive = document.createElement("button");
      btnActive.className = "mini";
      btnActive.textContent = "设为生效";
      btnActive.onclick = () => activate(r.id);
      ops.appendChild(btnActive);
    }

    const btnDl = document.createElement("a");
    btnDl.className = "mini ghost";
    btnDl.textContent = "下载";
    btnDl.href = `/api/dispatchers/${r.id}/download`;
    ops.appendChild(btnDl);

    if (!r.is_builtin) {
      const btnDel = document.createElement("button");
      btnDel.className = "mini danger";
      btnDel.textContent = "删除";
      btnDel.onclick = () => del(r.id, r.name);
      ops.appendChild(btnDel);
    }

    body.appendChild(tr);
  });
}

function toggleEnable(id, enabled) {
  fetch(`/api/dispatchers/${id}/enable`, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  }).then(() => load());
}

function activate(id) {
  fetch(`/api/dispatchers/${id}/activate`, { method: "POST" }).then(() => load());
}

function del(id, name) {
  if (!confirm(`确认删除分发器「${name}」？该操作不可恢复。`)) return;
  fetch(`/api/dispatchers/${id}/delete`, { method: "POST" }).then(() => load());
}

function upload(e) {
  e.preventDefault();
  const status = document.getElementById("up-status");
  status.textContent = "上传中…"; status.className = "status";
  const fd = new FormData();
  fd.append("name", document.getElementById("up-name").value);
  fd.append("file", document.getElementById("up-file").files[0]);
  fetch("/api/dispatchers/upload", { method: "POST", body: fd })
    .then(r => r.json()).then(res => {
      if (!res.ok) { status.textContent = res.error; status.className = "status err"; return; }
      status.textContent = "✓ 已上传"; status.className = "status ok";
      document.getElementById("upload-form").reset();
      load();
    }).catch(err => { status.textContent = "出错：" + err; status.className = "status err"; });
}

document.addEventListener("DOMContentLoaded", () => {
  load();
  document.getElementById("upload-form").addEventListener("submit", upload);
});
