// Hydragent Control UI — main app
// Vanilla JS, no build step. Talks to the same Event Bus that
// adapters/websocket_adapter.py talks to, but with token auth and
// device pairing added in front.

(function () {
  "use strict";

  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => Array.from(document.querySelectorAll(sel));

  // -------------------------------------------------------------------
  // State
  // -------------------------------------------------------------------
  const state = {
    cfg: null,
    token: localStorage.getItem("hydra.token") || "",
    password: sessionStorage.getItem("hydra.password") || "",
    deviceId: localStorage.getItem("hydra.deviceId") || (crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).slice(2)),
    pageId: localStorage.getItem("hydra.pageId") || ("ctrl-" + Math.random().toString(36).slice(2, 10)),
    ws: null,
    inflight: null,
    adminToken: localStorage.getItem("hydra.adminToken") || "",
  };
  localStorage.setItem("hydra.deviceId", state.deviceId);
  localStorage.setItem("hydra.pageId", state.pageId);

  const ADMIN_METHODS = {
    "memory.list":     { params: ["q", "limit"],                 invoke: (p) => adminRpc("memory.list", p) },
    "memory.search":   { params: ["q"],                         invoke: (p) => adminRpc("memory.search", p) },
    "memory.delete":   { params: ["id"],                        invoke: (p) => adminRpc("memory.delete", p) },
    "memory.clear":    { params: ["confirm"],                   invoke: (p) => adminRpc("memory.clear", p) },
    "page.list":       { params: [],                            invoke: (p) => adminRpc("page.list", p) },
    "page.get_summary":{ params: ["page_id"],                   invoke: (p) => adminRpc("page.get_summary", p) },
    "page.update_summary": { params: ["page_id", "summary"],    invoke: (p) => adminRpc("page.update_summary", p) },
    "page.compact":    { params: ["page_id"],                   invoke: (p) => adminRpc("page.compact", p) },
    "library.list_nodes": { params: [],                         invoke: (p) => adminRpc("library.list_nodes", p) },
    "library.search":  { params: ["q"],                         invoke: (p) => adminRpc("library.search", p) },
    "config.read":     { params: ["key"],                       invoke: (p) => adminRpc("config.read", p) },
    "config.write":    { params: ["key", "value"],              invoke: (p) => adminRpc("config.write", p) },
    "skill.list":      { params: [],                            invoke: (p) => adminRpc("skill.list", p) },
    "skill.search":    { params: ["q"],                         invoke: (p) => adminRpc("skill.search", p) },
    "skill.run":       { params: ["name", "args"],              invoke: (p) => adminRpc("skill.run", p) },
    "dream.run":       { params: ["intent"],                    invoke: (p) => adminRpc("dream.run", p) },
    "dream.status":    { params: [],                            invoke: (p) => adminRpc("dream.status", p) },
    "cron.list":       { params: [],                            invoke: (p) => adminRpc("cron.list", p) },
    "cron.run":        { params: ["name"],                      invoke: (p) => adminRpc("cron.run", p) },
    "cron.add":        { params: ["name", "cron", "intent"],    invoke: (p) => adminRpc("cron.add", p) },
    "cron.remove":     { params: ["name"],                      invoke: (p) => adminRpc("cron.remove", p) },
    "session.list":    { params: [],                            invoke: (p) => adminRpc("session.list", p) },
  };

  // -------------------------------------------------------------------
  // Boot
  // -------------------------------------------------------------------
  async function boot() {
    await HydraI18n.init();
    await loadConfig();
    populateThemeSelect();
    wireUI();
    connect();
    hydrateActiveTab();
  }

  async function loadConfig() {
    const basePath = (location.pathname || "/").replace(/[^/]*$/, "");
    const url = `${basePath || "/"}control-ui-config.json`;
    try {
      const res = await fetch(url, { headers: authHeaders() });
      if (!res.ok) throw new Error("status " + res.status);
      state.cfg = await res.json();
    } catch (e) {
      console.warn("config load failed, using defaults", e);
      state.cfg = {
        websocketUrl: `ws://${location.host}/ws`,
        authMode: "token",
        features: { pwa: true, webPush: true, i18n: true, themes: true },
        themes: ["hydra-dark", "hydra-light", "abyss", "aurora"],
        vapidPublicKey: "",
      };
    }
    applyTheme(localStorage.getItem("hydra.theme") || "hydra-dark");
  }

  function authHeaders(extra) {
    const h = Object.assign({}, extra || {});
    if (state.cfg?.authMode === "password" && state.password) {
      h["Authorization"] = "Basic " + btoa("hydragent:" + state.password);
    } else if (state.token) {
      h["Authorization"] = "Bearer " + state.token;
    }
    h["X-Hydra-Device-Id"] = state.deviceId;
    return h;
  }

  // -------------------------------------------------------------------
  // UI
  // -------------------------------------------------------------------
  function wireUI() {
    // Tabs
    $$("#tabs .tab").forEach((btn) => {
      btn.addEventListener("click", () => activateTab(btn.dataset.tab));
    });
    // Theme + lang
    $("#theme-select").addEventListener("change", (e) => {
      applyTheme(e.target.value);
      localStorage.setItem("hydra.theme", e.target.value);
    });
    $("#lang-select").value = HydraI18n.locale;
    $("#lang-select").addEventListener("change", (e) => HydraI18n.setLocale(e.target.value));
    HydraI18n.onChange((loc) => { $("#lang-select").value = loc; });
    // Chat
    $("#chat-form").addEventListener("submit", onSend);
    $("#chat-input").addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); onSend(e); }
      autosize();
    });
    $("#chat-clear").addEventListener("click", () => { $("#chat-stream").innerHTML = ""; });
    $("#chat-new").addEventListener("click", () => {
      state.pageId = "ctrl-" + Math.random().toString(36).slice(2, 10);
      localStorage.setItem("hydra.pageId", state.pageId);
      if (state.ws && state.ws.readyState === WebSocket.OPEN) {
        state.ws.send(JSON.stringify({ set_page_id: state.pageId }));
      }
      $("#chat-stream").innerHTML = "";
      addSystemMsg("New page: " + state.pageId);
    });
    // Memory
    $("#memory-refresh").addEventListener("click", () => refreshMemory());
    $("#memory-search").addEventListener("input", debounce(refreshMemory, 200));
    $("#memory-clear-all").addEventListener("click", clearAllMemory);
    // Library
    $("#library-refresh").addEventListener("click", () => refreshLibrary());
    $("#library-search").addEventListener("input", debounce(refreshLibrary, 200));
    // Skills
    $("#skill-refresh").addEventListener("click", () => refreshSkills());
    $("#skill-search").addEventListener("input", debounce(refreshSkills, 200));
    // Dreams
    $("#dream-status").addEventListener("click", () => dream("dream.status"));
    $("#dream-run").addEventListener("click", () => dream("dream.run", { intent: "consolidate recent memories" }));
    // Cron
    $("#cron-refresh").addEventListener("click", () => refreshCron());
    $("#cron-add").addEventListener("click", addCronJob);
    // Pages
    $("#page-refresh").addEventListener("click", () => refreshPages());
    $("#page-compact").addEventListener("click", () => adminRpc("page.compact", { page_id: state.pageId }).then((r) => addSystemMsg("compact: " + JSON.stringify(r))));
    // Config
    $("#config-reload").addEventListener("click", () => loadConfig().then(renderConfig));
    // Auth banner
    $("#auth-submit").addEventListener("click", submitAuth);
    $("#auth-dismiss").addEventListener("click", () => $("#auth-banner").setAttribute("hidden", ""));
    $("#auth-token").addEventListener("keydown", (e) => { if (e.key === "Enter") submitAuth(e); });
    // PWA
    window.addEventListener("beforeinstallprompt", (e) => {
      e.preventDefault();
      $("#install-pwa").hidden = false;
      $("#install-pwa").onclick = () => e.prompt();
    });
  }

  function populateThemeSelect() {
    const sel = $("#theme-select");
    sel.innerHTML = "";
    (state.cfg?.themes || ["hydra-dark", "hydra-light", "abyss", "aurora"]).forEach((t) => {
      const opt = document.createElement("option");
      opt.value = t; opt.textContent = t; sel.appendChild(opt);
    });
    sel.value = localStorage.getItem("hydra.theme") || "hydra-dark";
  }

  function applyTheme(name) {
    document.documentElement.setAttribute("data-theme", name);
  }

  function activateTab(name) {
    $$("#tabs .tab").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
    $$(".panel").forEach((p) => p.hidden = p.dataset.panel !== name);
    hydrateActiveTab(name);
  }

  function hydrateActiveTab(name) {
    if (name === "memory") refreshMemory();
    else if (name === "library") refreshLibrary();
    else if (name === "skills") refreshSkills();
    else if (name === "cron") refreshCron();
    else if (name === "pages") refreshPages();
    else if (name === "config") renderConfig();
  }

  function debounce(fn, ms) {
    let t = null; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
  }

  function autosize() {
    const el = $("#chat-input");
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 240) + "px";
  }

  // -------------------------------------------------------------------
  // WebSocket
  // -------------------------------------------------------------------
  function setConn(state_) {
    const el = $("#conn-status");
    el.dataset.state = state_;
    el.querySelector(".label").textContent = state_;
  }

  function connect() {
    if (!state.cfg) return;
    setConn("connecting");
    const url = new URL(state.cfg.websocketUrl, location.href);
    // Token can also be in the URL for browsers that strip Authorization
    // headers on the WebSocket handshake.
    if (state.cfg.authMode === "token" && state.token) {
      url.searchParams.set("token", state.token);
    } else if (state.cfg.authMode === "password" && state.password) {
      // Browsers can't set Authorization on WS; the password is sent via
      // the existing HTTP basic-auth header. If the runtime config says
      // password mode, the adapter's auth_middleware will accept basic
      // auth only on the upgrade.
    }
    const ws = new WebSocket(url.toString());
    state.ws = ws;
    ws.addEventListener("open", () => {
      setConn("connected");
      ws.send(JSON.stringify({ set_page_id: state.pageId }));
    });
    ws.addEventListener("close", (e) => {
      setConn("offline");
      const reason = e.code === 1006 ? "" : "(" + e.code + ")";
      addSystemMsg("disconnected " + reason);
      // Pairing-required or auth-required: surface the banner.
      if (e.code === 409) { showAuthBanner("pairing required"); }
      else if (e.code === 401) { showAuthBanner("auth required"); }
      setTimeout(connect, 2000);
    });
    ws.addEventListener("error", () => setConn("error"));
    ws.addEventListener("message", onWsMessage);
  }

  function onWsMessage(ev) {
    let msg; try { msg = JSON.parse(ev.data); } catch { return; }
    switch (msg.type) {
      case "hello":
        console.info("hello", msg);
        break;
      case "token":
        if (state.inflight) state.inflight.text += (msg.token || "");
        break;
      case "status":
        if (state.inflight) state.inflight.placeholder = "thinking… " + msg.status;
        break;
      case "permission_request": {
        askPermission(msg).then((decision) => {
          // The Rust kernel listens for `permission.respond` over the bus;
          // the websocket adapter translates `auto_approve_set` etc. from
          // inbound browser messages. For now we just toast — the
          // permission flow is a v0.2 add-on.
          console.info("permission decision", decision, "for", msg);
        });
        break;
      }
      case "result":
        if (state.inflight) {
          state.inflight.text = msg.content || state.inflight.text;
          state.inflight = null;
        }
        $("#chat-send").hidden = false;
        $("#chat-cancel").hidden = true;
        autosize();
        break;
      case "push":
        if (msg.page_id && msg.page_id !== state.pageId) return;
        addMsg("assistant", msg.content || "");
        break;
      case "error":
        if (state.inflight) { state.inflight.text += "\n\n⚠️ " + (msg.message || ""); state.inflight = null; }
        addSystemMsg("error: " + (msg.message || ""));
        $("#chat-send").hidden = false;
        $("#chat-cancel").hidden = true;
        break;
    }
  }

  function onSend(e) {
    e?.preventDefault?.();
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
      addSystemMsg("not connected");
      return;
    }
    const ta = $("#chat-input");
    const content = ta.value.trim();
    if (!content) return;
    addMsg("user", content);
    state.inflight = addMsg("assistant", "");
    state.inflight.placeholder = "thinking…";
    state.ws.send(JSON.stringify({ content, page_id: state.pageId }));
    ta.value = "";
    autosize();
    $("#chat-send").hidden = true;
    $("#chat-cancel").hidden = false;
    $("#chat-cancel").onclick = () => {
      if (state.ws && state.ws.readyState === WebSocket.OPEN) {
        state.ws.close();
      }
    };
  }

  function addMsg(role, text) {
    const div = document.createElement("div");
    div.className = "msg " + role;
    div.textContent = text;
    const stream = $("#chat-stream");
    stream.appendChild(div);
    stream.scrollTop = stream.scrollHeight;
    return div;
  }
  function addSystemMsg(text) { addMsg("system", text); }

  function showAuthBanner(reason) {
    const banner = $("#auth-banner");
    banner.querySelector(".msg").textContent = "🔒 " + reason;
    banner.removeAttribute("hidden");
  }

  function submitAuth(e) {
    e?.preventDefault?.();
    const v = $("#auth-token").value;
    if (!v) return;
    if (state.cfg?.authMode === "password") {
      state.password = v;
      sessionStorage.setItem("hydra.password", state.password);
    } else {
      state.token = v;
      localStorage.setItem("hydra.token", state.token);
    }
    $("#auth-banner").setAttribute("hidden", "");
    if (state.ws) try { state.ws.close(); } catch {}
    connect();
  }

  // -------------------------------------------------------------------
  // Permission modal
  // -------------------------------------------------------------------
  function askPermission(req) {
    return new Promise((resolve) => {
      const modal = $("#perm-modal");
      $("#perm-summary").textContent = `${req.tier || "Permission"} — ${req.tool_id || req.request_id || ""}\n${req.summary || ""}`;
      const handler = (e) => {
        modal.removeEventListener("close", handler);
        resolve(modal.returnValue || "deny");
      };
      modal.addEventListener("close", handler);
      modal.showModal();
    });
  }

  // -------------------------------------------------------------------
  // Admin RPC
  // -------------------------------------------------------------------
  async function adminRpc(method, params) {
    const token = state.adminToken || prompt("Admin token (HYDRA_ADMIN_RPC_TOKEN):");
    if (!token) throw new Error("admin token required");
    state.adminToken = token;
    localStorage.setItem("hydra.adminToken", token);
    const basePath = (location.pathname || "/").replace(/[^/]*$/, "");
    const url = `${basePath || "/"}api/v1/admin/rpc`;
    const res = await fetch(url, {
      method: "POST",
      headers: Object.assign(authHeaders({ "Content-Type": "application/json" }), {
        "Authorization": "Bearer " + token,
      }),
      body: JSON.stringify({ method, params: params || {} }),
    });
    if (!res.ok) throw new Error("admin rpc " + res.status + ": " + (await res.text()));
    return res.json();
  }

  // -------------------------------------------------------------------
  // Panel renderers
  // -------------------------------------------------------------------
  async function refreshMemory() {
    const list = $("#memory-list");
    list.setAttribute("aria-busy", "true");
    list.innerHTML = "";
    try {
      const q = $("#memory-search").value.trim();
      const res = await adminRpc(q ? "memory.search" : "memory.list", { q, limit: 50 });
      const items = res?.result?.items || res?.result || [];
      if (!items.length) { list.innerHTML = "<li class='muted'>No memories.</li>"; return; }
      for (const it of items) {
        const li = document.createElement("li");
        li.innerHTML = `
          <div>
            <div class="title">${escapeHtml(it.title || it.id || "memory")}</div>
            <div class="excerpt">${escapeHtml((it.text || it.content || "").slice(0, 240))}</div>
          </div>
          <div class="actions">
            <button class="btn ghost" data-action="del" data-id="${escapeHtml(it.id)}">Delete</button>
          </div>
        `;
        li.querySelector('[data-action="del"]').onclick = async () => {
          if (!confirm("Delete this memory?")) return;
          try { await adminRpc("memory.delete", { id: it.id }); li.remove(); }
          catch (e) { alert("delete failed: " + e.message); }
        };
        list.appendChild(li);
      }
    } catch (e) { list.innerHTML = "<li class='muted'>" + escapeHtml(e.message) + "</li>"; }
    finally { list.removeAttribute("aria-busy"); }
  }

  async function clearAllMemory() {
    if (!confirm("Wipe ALL memory? This cannot be undone.")) return;
    try { await adminRpc("memory.clear", { confirm: true }); refreshMemory(); }
    catch (e) { alert("clear failed: " + e.message); }
  }

  async function refreshLibrary() {
    const list = $("#library-list");
    list.setAttribute("aria-busy", "true");
    list.innerHTML = "";
    try {
      const q = $("#library-search").value.trim();
      const res = await adminRpc(q ? "library.search" : "library.list_nodes", { q });
      const items = res?.result?.items || res?.result || [];
      if (!items.length) { list.innerHTML = "<li class='muted'>Library empty.</li>"; return; }
      for (const it of items) {
        const li = document.createElement("li");
        li.innerHTML = `
          <div>
            <div class="title">${escapeHtml(it.title || it.name || it.id)}</div>
            <div class="excerpt">${escapeHtml((it.summary || it.text || "").slice(0, 240))}</div>
          </div>
          <div class="meta">${escapeHtml(it.kind || "")}</div>
        `;
        list.appendChild(li);
      }
    } catch (e) { list.innerHTML = "<li class='muted'>" + escapeHtml(e.message) + "</li>"; }
    finally { list.removeAttribute("aria-busy"); }
  }

  async function refreshSkills() {
    const list = $("#skill-list");
    list.setAttribute("aria-busy", "true");
    list.innerHTML = "";
    try {
      const q = $("#skill-search").value.trim();
      const res = await adminRpc(q ? "skill.search" : "skill.list", { q });
      const items = res?.result?.items || res?.result || [];
      if (!items.length) { list.innerHTML = "<li class='muted'>No skills installed.</li>"; return; }
      for (const it of items) {
        const li = document.createElement("li");
        li.innerHTML = `
          <div>
            <div class="title">${escapeHtml(it.name || it.id)}</div>
            <div class="excerpt">${escapeHtml((it.description || "").slice(0, 240))}</div>
          </div>
          <div class="actions">
            <button class="btn" data-action="run" data-name="${escapeHtml(it.name)}">Run</button>
          </div>
        `;
        li.querySelector('[data-action="run"]').onclick = async () => {
          const args = prompt("Args (JSON):", "{}");
          try { await adminRpc("skill.run", { name: it.name, args: args || "{}" }); refreshSkills(); }
          catch (e) { alert("run failed: " + e.message); }
        };
        list.appendChild(li);
      }
    } catch (e) { list.innerHTML = "<li class='muted'>" + escapeHtml(e.message) + "</li>"; }
    finally { list.removeAttribute("aria-busy"); }
  }

  async function dream(method, params) {
    const out = $("#dream-output");
    out.textContent = "running…";
    try {
      const res = await adminRpc(method, params);
      out.textContent = JSON.stringify(res?.result || res, null, 2);
    } catch (e) { out.textContent = "error: " + e.message; }
  }

  async function refreshCron() {
    const tbody = $("#cron-table tbody");
    tbody.innerHTML = "";
    try {
      const res = await adminRpc("cron.list", {});
      const items = res?.result?.items || res?.result || [];
      for (const j of items) {
        const tr = document.createElement("tr");
        tr.innerHTML = `
          <td>${escapeHtml(j.name)}</td>
          <td><code>${escapeHtml(j.cron)}</code></td>
          <td>${escapeHtml(j.last_run || "")}</td>
          <td>${escapeHtml(j.next_run || "")}</td>
          <td><button class="btn danger" data-name="${escapeHtml(j.name)}">Delete</button></td>
        `;
        tr.querySelector("button").onclick = async () => {
          if (!confirm("Delete cron " + j.name + "?")) return;
          try { await adminRpc("cron.remove", { name: j.name }); refreshCron(); }
          catch (e) { alert("delete failed: " + e.message); }
        };
        tbody.appendChild(tr);
      }
    } catch (e) {
      const tr = document.createElement("tr");
      tr.innerHTML = `<td colspan="5" class="muted">${escapeHtml(e.message)}</td>`;
      tbody.appendChild(tr);
    }
  }

  async function addCronJob() {
    const name = prompt("Name:"); if (!name) return;
    const cron = prompt("Cron expression (e.g. 0 */6 * * *):"); if (!cron) return;
    const intent = prompt("Intent:"); if (!intent) return;
    try { await adminRpc("cron.add", { name, cron, intent }); refreshCron(); }
    catch (e) { alert("add failed: " + e.message); }
  }

  async function refreshPages() {
    const list = $("#page-list");
    list.innerHTML = "";
    try {
      const res = await adminRpc("page.list", {});
      const items = res?.result?.items || res?.result || [];
      for (const p of items) {
        const li = document.createElement("li");
        li.innerHTML = `
          <div>
            <div class="title">${escapeHtml(p.page_id || p.id)}</div>
            <div class="excerpt">${escapeHtml((p.summary || "").slice(0, 240))}</div>
          </div>
          <div class="meta">${escapeHtml(p.updated || "")}</div>
        `;
        li.onclick = () => {
          state.pageId = p.page_id || p.id;
          localStorage.setItem("hydra.pageId", state.pageId);
          if (state.ws && state.ws.readyState === WebSocket.OPEN) {
            state.ws.send(JSON.stringify({ set_page_id: state.pageId }));
          }
          activateTab("chat");
        };
        list.appendChild(li);
      }
    } catch (e) { list.innerHTML = "<li class='muted'>" + escapeHtml(e.message) + "</li>"; }
  }

  function renderConfig() {
    $("#config-output").textContent = JSON.stringify(state.cfg, null, 2);
  }

  // -------------------------------------------------------------------
  // Utils
  // -------------------------------------------------------------------
  function escapeHtml(s) {
    if (s == null) return "";
    return String(s).replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[c]));
  }

  // -------------------------------------------------------------------
  // Go
  // -------------------------------------------------------------------
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
