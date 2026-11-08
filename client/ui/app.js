// goodbyedc arayüzü
// Yazı/sinyal: Worker'a WebSocket (bu dosya)
// Ses/ekran:   Rust tarafı (invoke ile çağrılır)

const tauri = window.__TAURI__;
const invoke = tauri ? tauri.core.invoke : async () => { throw new Error("Tauri dışında çalışıyor"); };

const $ = (id) => document.getElementById(id);

// Sunucu adresi sabit: herkes aynı Worker'a bağlanır.
const BASE = "https://goodbyedc.my-turn-server.workers.dev";

// İkonlar (Lucide, ISC lisansı). currentColor ile boyanır.
const ICONS = {
  mic: '<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/>',
  micOff: '<path d="M2 2l20 20"/><path d="M18.89 13.23A7.12 7.12 0 0 0 19 12v-2"/><path d="M5 10v2a7 7 0 0 0 12 5"/><path d="M15 9.34V5a3 3 0 0 0-5.68-1.33"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12"/><path d="M12 19v3"/>',
  headphones: '<path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"/>',
  headphonesOff: '<path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"/><path d="M2 2l20 20"/>',
  logOut: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="M16 17l5-5-5-5"/><path d="M21 12H9"/>',
  monitor: '<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>',
  window: '<rect width="20" height="16" x="2" y="4" rx="2"/><path d="M2 8h20"/><path d="M6 4v4"/><path d="M10 4v4"/>',
  phoneOff: '<path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91"/><path d="M22 2 2 22"/>',
  volume: '<path d="M11 5 6 9H2v6h4l5 4V5Z"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>',
  hash: '<path d="M4 9h16"/><path d="M4 15h16"/><path d="M10 3 8 21"/><path d="M16 3l-2 18"/>',
  x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',
  winMin: '<path d="M5 12h14"/>',
  winMax: '<rect width="14" height="14" x="5" y="5" rx="1"/>',
  winRestore: '<rect width="11" height="11" x="5" y="8" rx="1"/><path d="M9 8V6a1 1 0 0 1 1-1h7a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-2"/>',
  refresh: '<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>',
};

function icon(name, size = 20) {
  return `<svg class="ic" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[name]}</svg>`;
}

document.querySelectorAll("[data-icon]").forEach((el) => (el.innerHTML = icon(el.dataset.icon)));

// ------------------------------------------------------------- pencere
// macOS: sistem düğmeleri (kırmızı/sarı/yeşil) kenar çubuğunun sol üstüne yerleşir.
// Windows/Linux: pencere çerçevesiz açılır, düğmeleri biz çizeriz.
// Tarayıcıda (geliştirme) ikisi de yok.
const isMac = /Mac/i.test(navigator.platform || navigator.userAgent);
document.documentElement.dataset.os = !tauri ? "browser" : isMac ? "mac" : "other";

if (tauri && !isMac) {
  const win = tauri.window.getCurrentWindow();
  $("win-min").onclick = () => win.minimize();
  $("win-max").onclick = () => win.toggleMaximize();
  $("win-close").onclick = () => win.close();
  const syncMax = async () => {
    const max = await win.isMaximized();
    $("win-max").innerHTML = icon(max ? "winRestore" : "winMax");
    $("win-max").title = max ? "Önceki boyut" : "Büyüt";
  };
  win.onResized(syncMax);
  syncMax();
}

const state = {
  base: BASE,
  token: localStorage.getItem("token") || "",
  me: null,
  servers: [],
  channels: [],
  voice: [],          // [{userId, username, channelId, sessionId, tracks, muted, deafened}]
  online: [],
  serverId: Number(localStorage.getItem("serverId")) || null,
  channelId: null,    // seçili yazı kanalı
  messages: new Map(), // channelId -> {list, more, loading}
  myVoice: null,      // {channelId, sessionId, tracks}
  muted: localStorage.getItem("muted") === "1",
  deafened: localStorage.getItem("deafened") === "1",
  watching: null,     // ekranını izlediğimiz oturum id'si
  usage: null,        // {usedGB, limitGB}
};

// ---------------------------------------------------------------- yardımcılar

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function colorFor(name) {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.codePointAt(0)) >>> 0;
  const colors = ["#5865f2", "#3ba55c", "#faa61a", "#ed4245", "#eb459e", "#9b59b6", "#1abc9c", "#e67e22"];
  return colors[h % colors.length];
}

function avatar(name, cls = "avatar") {
  return `<div class="${cls}" style="background:${colorFor(name)}">${esc(name.slice(0, 1).toUpperCase())}</div>`;
}

let toastTimer;
function toast(text, isErr = false) {
  const t = $("toast");
  t.textContent = text;
  t.className = "toast" + (isErr ? " err" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.add("hidden"), 4000);
}

function fmtTime(ms) {
  const d = new Date(ms);
  const today = new Date();
  const hm = d.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === today.toDateString()) return `Bugün ${hm}`;
  return `${d.toLocaleDateString("tr-TR")} ${hm}`;
}

/** Basit modal. fields: HTML; onOk(body) true dönerse kapanır. */
function modal(title, html, onOk, okText = "Tamam") {
  $("modal-title").textContent = title;
  $("modal-body").innerHTML = html;
  $("modal-ok").textContent = okText;
  $("modal-ok").disabled = false;
  $("modal").classList.remove("hidden");
  const close = () => $("modal").classList.add("hidden");
  $("modal-cancel").onclick = close;
  $("modal-ok").onclick = async () => {
    if ((await onOk($("modal-body"))) !== false) close();
  };
  const first = $("modal-body").querySelector("input[type=text], input:not([type])");
  if (first) {
    first.focus();
    first.onkeydown = (e) => e.key === "Enter" && $("modal-ok").click();
  }
}

// ------------------------------------------------------------------ giriş

let registering = false;

function showAuth() {
  $("app").classList.add("hidden");
  $("auth").classList.remove("hidden");
}

$("auth-toggle").onclick = (e) => {
  e.preventDefault();
  registering = !registering;
  $("invite-row").classList.toggle("hidden", !registering);
  $("auth-submit").textContent = registering ? "Kayıt ol" : "Giriş yap";
  $("auth-sub").textContent = registering ? "Hesap oluştur" : "Tekrar hoş geldin!";
  $("auth-toggle-text").textContent = registering ? "Zaten hesabın var mı?" : "Hesabın yok mu?";
  $("auth-toggle").textContent = registering ? "Giriş yap" : "Kayıt ol";
};

$("auth-form").onsubmit = async (e) => {
  e.preventDefault();
  $("auth-error").textContent = "";
  const base = BASE;
  const body = { username: $("auth-user").value, password: $("auth-pass").value };
  if (registering) body.invite = $("auth-invite").value;
  try {
    const r = await fetch(`${base}/api/${registering ? "register" : "login"}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const data = await r.json();
    if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
    state.token = data.token;
    localStorage.setItem("token", data.token);
    start();
  } catch (err) {
    $("auth-error").textContent = err.message === "Failed to fetch" ? "Sunucuya ulaşılamadı" : err.message;
  }
};

$("btn-logout").onclick = async () => {
  await leaveVoice();
  localStorage.removeItem("token");
  state.token = "";
  if (ws) {
    ws.onclose = null;
    ws.close();
  }
  showAuth();
};

// --------------------------------------------------------------- websocket

let ws = null;
let wsRetry = 0;
let pingTimer = null;

function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

function setConn(text, bad = false) {
  $("conn-status").textContent = text;
  $("conn-status").className = "conn-status" + (bad ? " bad" : "");
}

function connect() {
  const url = state.base.replace(/^http/, "ws") + `/api/ws?token=${encodeURIComponent(state.token)}`;
  ws = new WebSocket(url);
  setConn("Bağlanıyor…");

  ws.onopen = () => {
    wsRetry = 0;
    setConn("");
    clearInterval(pingTimer);
    pingTimer = setInterval(() => ws.readyState === WebSocket.OPEN && ws.send("ping"), 30000);
  };

  ws.onmessage = (ev) => {
    if (ev.data === "pong") return;
    handle(JSON.parse(ev.data));
  };

  ws.onclose = async () => {
    clearInterval(pingTimer);
    wsRetry++;
    if (wsRetry >= 2) {
      // Token geçersizse tekrar denemek yerine giriş ekranına dön.
      try {
        const r = await fetch(`${state.base}/api/ice`, { headers: { Authorization: `Bearer ${state.token}` } });
        if (r.status === 401) {
          localStorage.removeItem("token");
          state.token = "";
          return showAuth();
        }
      } catch {}
    }
    const delay = Math.min(10000, 500 * 2 ** wsRetry);
    setConn("Bağlantı koptu, yeniden deneniyor…", true);
    setTimeout(connect, delay);
  };
}

function handle(m) {
  switch (m.t) {
    case "ready": {
      state.me = m.me;
      state.servers = m.servers;
      state.channels = m.channels;
      state.voice = m.voice;
      state.online = m.online;
      state.usage = m.usage;
      $("me-name").textContent = m.me.username;
      $("me-avatar").outerHTML = `<div class="avatar" id="me-avatar" style="background:${colorFor(m.me.username)}">${esc(m.me.username[0].toUpperCase())}</div>`;
      // Yeniden bağlandıysak ses durumumuzu tekrar bildir.
      if (state.myVoice) {
        send({ t: "voice_join", ...state.myVoice, muted: state.muted, deafened: state.deafened });
        if (state.watching) send({ t: "voice_update", watching: state.watching });
      }
      // Mesajları yeniden yükle (koptuğumuz sırada gelenler olabilir).
      state.messages.clear();
      if (!state.servers.find((s) => s.id === state.serverId)) state.serverId = state.servers[0]?.id ?? null;
      selectServer(state.serverId);
      checkUpdate();
      break;
    }
    case "message": {
      const c = state.messages.get(m.message.channelId);
      if (c) {
        c.list.push(m.message);
        if (m.message.channelId === state.channelId) renderMessages(true);
      }
      break;
    }
    case "history": {
      const c = state.messages.get(m.channelId) || { list: [], more: true };
      const known = new Set(c.list.map((x) => x.id));
      c.list = [...m.messages.filter((x) => !known.has(x.id)), ...c.list];
      c.more = m.more;
      c.loading = false;
      state.messages.set(m.channelId, c);
      if (m.channelId === state.channelId) renderMessages(c.list.length <= m.messages.length);
      break;
    }
    case "server_created":
      state.servers.push(m.server);
      break;
    case "server_deleted":
      state.servers = state.servers.filter((s) => s.id !== m.serverId);
      state.channels = state.channels.filter((c) => c.serverId !== m.serverId);
      if (state.serverId === m.serverId) selectServer(state.servers[0]?.id);
      break;
    case "channel_created":
      state.channels.push(m.channel);
      renderChannels();
      break;
    case "channel_deleted":
      if (state.myVoice?.channelId === m.channelId) leaveVoice();
      state.channels = state.channels.filter((c) => c.id !== m.channelId);
      if (state.channelId === m.channelId) state.channelId = null;
      renderChannels();
      renderMain();
      break;
    case "voice":
      state.voice = m.voice;
      if (m.usage) state.usage = m.usage;
      renderChannels();
      syncVoice();
      break;
    case "voice_disabled":
      toast(m.error, true);
      if (state.myVoice) queue(doLeave);
      break;
    case "presence":
      state.online = m.online;
      break;
    case "error":
      toast(m.error, true);
      break;
  }
}

// ------------------------------------------------------------------ çizim

function selectServer(id) {
  state.serverId = id;
  localStorage.setItem("serverId", id);
  const text = state.channels.filter((c) => c.serverId === id && c.kind === "text");
  const saved = Number(localStorage.getItem(`ch:${id}`));
  state.channelId = text.find((c) => c.id === saved)?.id ?? text[0]?.id ?? null;
  renderChannels();
  renderMain();
}

function selectChannel(id) {
  state.channelId = id;
  localStorage.setItem(`ch:${state.serverId}`, id);
  renderChannels();
  renderMain();
}

function renderChannels() {
  const chans = state.channels.filter((c) => c.serverId === state.serverId);
  const text = chans.filter((c) => c.kind === "text");
  const voice = chans.filter((c) => c.kind === "voice");

  const chanRow = (c, ic) =>
    `<div class="channel ${c.id === state.channelId ? "active" : ""}" data-id="${c.id}" data-kind="${c.kind}">
       <span class="hash">${icon(ic, 18)}</span><span class="name">${esc(c.name)}</span>
       <button class="del" data-del="${c.id}" title="Kanalı sil">${icon("x", 14)}</button>
     </div>`;

  let html = `<div class="category"><span>Metin kanalları</span><button data-add="text" title="Kanal ekle">${icon("plus", 16)}</button></div>`;
  html += text.map((c) => chanRow(c, "hash")).join("");
  html += `<div class="category"><span>Ses kanalları</span><button data-add="voice" title="Kanal ekle">${icon("plus", 16)}</button></div>`;
  for (const c of voice) {
    html += chanRow(c, "volume");
    const users = state.voice.filter((v) => v.channelId === c.id);
    html += `<div class="voice-users">${users
      .map((u) => {
        const live = u.tracks.includes("screen");
        const isMe = u.userId === state.me?.id;
        const watching = state.watching === u.sessionId;
        return `<div class="voice-user ${speaking.has(u.sessionId) ? "speaking" : ""}" data-sid="${esc(u.sessionId)}">${avatar(u.username)}<span>${esc(u.username)}</span>
          <span class="flags">${u.deafened ? icon("headphonesOff", 14) : u.muted ? icon("micOff", 14) : ""}
          ${live ? `<span class="live ${watching ? "watching" : ""}" data-watch="${esc(u.sessionId)}" data-me="${isMe ? 1 : 0}"
              title="${isMe ? "Ekranını paylaşıyorsun" : "İzlemek için tıkla"}">CANLI</span>` : ""}</span></div>`;
      })
      .join("")}</div>`;
  }
  const el = $("channels");
  el.innerHTML = html;

  el.querySelectorAll(".channel").forEach((n) => {
    n.onclick = (e) => {
      if (e.target.closest(".del")) return;
      const id = Number(n.dataset.id);
      if (n.dataset.kind === "text") selectChannel(id);
      else joinVoice(id);
    };
  });
  el.querySelectorAll("[data-del]").forEach((b) => {
    b.onclick = () => {
      const ch = state.channels.find((c) => c.id === Number(b.dataset.del));
      modal("Kanalı sil", `<p>“${esc(ch.name)}” kanalı ve tüm mesajları silinecek. Emin misin?</p>`, () => {
        send({ t: "delete_channel", channelId: ch.id });
      }, "Sil");
    };
  });
  el.querySelectorAll("[data-add]").forEach((b) => {
    b.onclick = () => {
      const kind = b.dataset.add;
      modal(kind === "text" ? "Metin kanalı oluştur" : "Ses kanalı oluştur",
        `<input id="f-name" placeholder="kanal-adi" maxlength="50" />`, (body) => {
          const name = body.querySelector("#f-name").value.trim();
          if (!name) return false;
          send({ t: "create_channel", serverId: state.serverId, name, kind });
        }, "Oluştur");
    };
  });
  el.querySelectorAll("[data-watch]").forEach((b) => {
    b.onclick = () => {
      if (b.dataset.me === "1") return toast("Kendi ekranını paylaşıyorsun");
      watch(state.watching === b.dataset.watch ? null : b.dataset.watch);
    };
  });

  // Ses paneli
  $("voice-panel").classList.toggle("hidden", !state.myVoice);
  if (state.myVoice) {
    const ch = state.channels.find((c) => c.id === state.myVoice.channelId);
    const srv = state.servers.find((s) => s.id === ch?.serverId);
    $("voice-channel-name").textContent = `${ch?.name ?? ""} / ${srv?.name ?? ""}`;
    $("btn-screen").classList.toggle("on", state.myVoice.tracks.includes("screen"));
  }
  if (state.usage) {
    const { usedGB, limitGB } = state.usage;
    const u = $("usage");
    u.textContent = `Ses/görüntü trafiği (31 gün): ${usedGB.toFixed(1)} / ${limitGB} GB`;
    u.classList.toggle("warn", usedGB >= limitGB * 0.8);
  }
  $("btn-mute").classList.toggle("off", state.muted);
  $("btn-deafen").classList.toggle("off", state.deafened);
  $("btn-mute").innerHTML = icon(state.muted ? "micOff" : "mic");
  $("btn-deafen").innerHTML = icon(state.deafened ? "headphonesOff" : "headphones");
}

function renderMain() {
  const watchingUser = state.watching && state.voice.find((v) => v.sessionId === state.watching);
  $("stage").classList.toggle("hidden", !watchingUser);
  const ch = state.channels.find((c) => c.id === state.channelId);
  $("chat").classList.toggle("hidden", !!watchingUser || !ch);
  $("empty").classList.toggle("hidden", !!watchingUser || !!ch);

  if (watchingUser) {
    $("channel-title").innerHTML = `${icon("monitor", 18)}<span>${esc(watchingUser.username)} ekranını paylaşıyor</span>`;
    $("stage-title").textContent = watchingUser.username;
    return;
  }
  if (!ch) {
    $("channel-title").textContent = "";
    return;
  }
  $("channel-title").innerHTML = `${icon("hash", 20)}<span>${esc(ch.name)}</span>`;
  $("composer-input").placeholder = `#${ch.name} kanalına mesaj gönder`;
  if (!state.messages.has(ch.id)) {
    state.messages.set(ch.id, { list: [], more: true, loading: true });
    send({ t: "history", channelId: ch.id });
  }
  renderMessages(true);
  $("composer-input").focus();
}

function renderMessages(scrollToBottom) {
  const el = $("messages");
  const c = state.messages.get(state.channelId);
  if (!c) return;
  const prevHeight = el.scrollHeight;
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;

  let html = c.more && c.list.length ? `<div class="load-more muted small">Yukarı kaydırarak eski mesajları yükle</div>` : "";
  let prev = null;
  for (const m of c.list) {
    const grouped = prev && prev.userId === m.userId && m.createdAt - prev.createdAt < 5 * 60 * 1000;
    html += grouped
      ? `<div class="msg"><div class="spacer"></div><div class="body"><div class="content">${esc(m.content)}</div></div></div>`
      : `<div class="msg first">${avatar(m.username)}<div class="body">
           <div class="meta"><span class="author">${esc(m.username)}</span><span class="time">${fmtTime(m.createdAt)}</span></div>
           <div class="content">${esc(m.content)}</div></div></div>`;
    prev = m;
  }
  el.innerHTML = html;
  if (scrollToBottom === true && (atBottom || prevHeight === 0 || el.dataset.ch !== String(state.channelId))) {
    el.scrollTop = el.scrollHeight;
  } else if (scrollToBottom === false) {
    el.scrollTop = el.scrollHeight - prevHeight; // eski mesajlar eklendi, konumu koru
  } else if (atBottom) {
    el.scrollTop = el.scrollHeight;
  }
  el.dataset.ch = String(state.channelId);
}

$("messages").onscroll = () => {
  const el = $("messages");
  const c = state.messages.get(state.channelId);
  if (el.scrollTop < 40 && c && c.more && !c.loading && c.list.length) {
    c.loading = true;
    send({ t: "history", channelId: state.channelId, before: c.list[0].id });
  }
};

$("composer").onsubmit = (e) => {
  e.preventDefault();
  const input = $("composer-input");
  const content = input.value.trim();
  if (!content || !state.channelId) return;
  send({ t: "send", channelId: state.channelId, content });
  input.value = "";
};

// -------------------------------------------------------------------- ses

let voiceBusy = Promise.resolve();
/** Ses işlemlerini sıraya koyar (aynı anda iki işlem çalışmasın). */
function queue(fn) {
  voiceBusy = voiceBusy.then(fn).catch((e) => toast(String(e), true));
  return voiceBusy;
}

function joinVoice(channelId) {
  if (state.myVoice?.channelId === channelId) return;
  return queue(async () => {
    if (state.myVoice) await doLeave();
    toast("Ses kanalına bağlanılıyor…");
    const res = await invoke("voice_join", {
      baseUrl: state.base,
      token: state.token,
      muted: state.muted,
      deafened: state.deafened,
    });
    state.myVoice = { channelId, sessionId: res.sessionId, tracks: res.tracks };
    send({ t: "voice_join", ...state.myVoice, muted: state.muted, deafened: state.deafened });
    $("toast").classList.add("hidden");
    renderChannels();
  });
}

async function doLeave() {
  await invoke("voice_leave");
  speaking.clear();
  state.myVoice = null;
  state.watching = null;
  send({ t: "voice_leave" });
  renderChannels();
  renderMain();
}

function leaveVoice() {
  if (!state.myVoice) return Promise.resolve();
  return queue(doLeave);
}

let syncPending = false;
function syncVoice() {
  if (!state.myVoice) return;
  // İzlediğimiz kişi paylaşımı bıraktıysa izlemeyi kapat.
  if (state.watching) {
    const w = state.voice.find((v) => v.sessionId === state.watching);
    if (!w || !w.tracks.includes("screen") || w.channelId !== state.myVoice.channelId) {
      state.watching = null;
      renderMain();
    }
  }
  if (syncPending) return;
  syncPending = true;
  queue(async () => {
    syncPending = false;
    if (!state.myVoice) return;
    const participants = state.voice
      .filter((v) => v.channelId === state.myVoice.channelId)
      .map((v) => ({ sessionId: v.sessionId, tracks: v.tracks }));
    await invoke("voice_sync", { participants, watch: state.watching });
  });
}

function watch(sessionId) {
  if (sessionId) {
    const u = state.voice.find((v) => v.sessionId === sessionId);
    if (!state.myVoice || u?.channelId !== state.myVoice.channelId) {
      return toast("İzlemek için önce o ses kanalına katıl");
    }
  }
  state.watching = sessionId;
  send({ t: "voice_update", watching: sessionId });
  clearCanvas();
  renderChannels();
  renderMain();
  syncVoice();
}

$("stage-close").onclick = () => watch(null);

$("btn-leave").onclick = () => leaveVoice();

$("btn-mute").onclick = () => {
  state.muted = !state.muted;
  // Sağırken susturmayı kaldırmak sağırlığı da kaldırır (Discord gibi).
  if (!state.muted && state.deafened) setDeafened(false);
  applyMute();
};

$("btn-deafen").onclick = () => setDeafened(!state.deafened);

function setDeafened(v) {
  state.deafened = v;
  localStorage.setItem("deafened", v ? "1" : "0");
  if (v) state.muted = true;
  invoke("set_deafened", { deafened: v }).catch(() => {});
  applyMute();
}

function applyMute() {
  localStorage.setItem("muted", state.muted ? "1" : "0");
  invoke("set_muted", { muted: state.muted }).catch(() => {});
  if (state.myVoice) send({ t: "voice_update", muted: state.muted, deafened: state.deafened });
  renderChannels();
}

// ------------------------------------------------------------ ekran paylaşımı

$("btn-screen").onclick = async () => {
  if (!state.myVoice) return;
  if (state.myVoice.tracks.includes("screen")) {
    return queue(async () => {
      const tracks = await invoke("screen_stop");
      state.myVoice.tracks = tracks;
      send({ t: "voice_update", tracks });
      renderChannels();
    });
  }
  let sources;
  try {
    sources = await invoke("screen_sources");
  } catch (e) {
    return toast(String(e), true);
  }
  if (!sources.length) {
    return toast("Paylaşılabilecek ekran bulunamadı. macOS'ta Sistem Ayarları > Gizlilik > Ekran Kaydı iznini kontrol et.", true);
  }
  const groups = {
    screen: sources.filter((s) => s.kind === "screen"),
    window: sources.filter((s) => s.kind !== "screen"),
  };
  let tab = groups.screen.length ? "screen" : "window";
  let selected = groups[tab][0] ?? null;

  const html = `
    <div class="tabs">
      <button class="tab" data-tab="screen">${icon("monitor", 16)}<span>Tüm ekran</span></button>
      <button class="tab" data-tab="window">${icon("window", 16)}<span>Uygulamalar</span></button>
    </div>
    <div class="source-list"></div>`;

  modal("Ekran paylaş", html, () => {
    if (!selected) return false;
    const src = selected;
    queue(async () => {
      const tracks = await invoke("screen_start", { id: src.id, kind: src.kind });
      state.myVoice.tracks = tracks;
      send({ t: "voice_update", tracks });
      renderChannels();
    });
  }, "Paylaş");

  const body = $("modal-body");
  const list = body.querySelector(".source-list");

  function renderTab() {
    body.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
    const items = groups[tab];
    if (!selected || !items.includes(selected)) selected = items[0] ?? null;
    list.innerHTML = items.length
      ? items
          .map((s, i) => `<div class="source ${s === selected ? "selected" : ""}" data-i="${i}">${icon(tab === "screen" ? "monitor" : "window", 18)}<span>${esc(s.title)}</span></div>`)
          .join("")
      : `<p class="muted small">${tab === "screen" ? "Ekran bulunamadı." : "Açık uygulama penceresi bulunamadı."}</p>`;
    list.querySelectorAll(".source").forEach((n) => {
      n.onclick = () => {
        list.querySelectorAll(".source").forEach((x) => x.classList.remove("selected"));
        n.classList.add("selected");
        selected = items[Number(n.dataset.i)];
      };
      n.ondblclick = () => $("modal-ok").click();
    });
    $("modal-ok").disabled = !selected;
  }

  body.querySelectorAll(".tab").forEach((b) => {
    b.onclick = () => {
      tab = b.dataset.tab;
      selected = null;
      renderTab();
    };
  });
  renderTab();
};

// İzlenen ekranın kareleri Rust'tan JPEG olarak gelir.
const canvas = $("stage-canvas");
const ctx2d = canvas.getContext("2d");
let decoding = false;

function clearCanvas() {
  ctx2d.clearRect(0, 0, canvas.width, canvas.height);
}

if (tauri) {
  const frames = new tauri.core.Channel();
  frames.onmessage = async (data) => {
    if (decoding || !state.watching) return; // geride kalırsak kare atla
    const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data);
    const len = bytes[0];
    const owner = new TextDecoder().decode(bytes.subarray(1, 1 + len));
    if (owner !== state.watching) return;
    decoding = true;
    try {
      const bmp = await createImageBitmap(new Blob([bytes.subarray(1 + len)], { type: "image/jpeg" }));
      if (canvas.width !== bmp.width || canvas.height !== bmp.height) {
        canvas.width = bmp.width;
        canvas.height = bmp.height;
      }
      ctx2d.drawImage(bmp, 0, 0);
      bmp.close();
    } catch {}
    decoding = false;
  };
  invoke("screen_subscribe", { channel: frames });
}

// ------------------------------------------------------ kim konuşuyor
// Rust tarafı 250 ms'de bir ses seviyelerini gönderir (0..1).
const SPEAK_LEVEL = 0.02;
const speaking = new Set();

function applySpeaking(levels) {
  const now = new Set();
  if (state.myVoice) {
    if (!state.muted && levels.me > SPEAK_LEVEL) now.add(state.myVoice.sessionId);
    for (const [sid, lvl] of Object.entries(levels.peers || {})) if (lvl > SPEAK_LEVEL) now.add(sid);
  }
  speaking.clear();
  now.forEach((x) => speaking.add(x));
  document.querySelectorAll(".voice-user[data-sid]").forEach((el) => el.classList.toggle("speaking", speaking.has(el.dataset.sid)));
}

if (tauri) {
  const levels = new tauri.core.Channel();
  levels.onmessage = applySpeaking;
  invoke("levels_subscribe", { channel: levels });
}

// ------------------------------------------------------------ güncelleme
// Yeni bir sürüm yayınlandıysa "Güncelle" düğmesi çıkar.

let updateTimer = null;

/** manual: kullanıcı düğmeye bastıysa sonucu her durumda bildir. */
async function checkUpdate(manual = false) {
  if (!tauri) return;
  clearTimeout(updateTimer);
  updateTimer = setTimeout(checkUpdate, 30 * 60 * 1000);
  const btn = $("btn-check-update");
  btn.classList.add("spin");
  try {
    const r = await invoke("update_check");
    $("btn-update").classList.toggle("hidden", !r.available);
    if (manual) {
      if (!r.supported) toast("Bu sürüm elle derlendiği için otomatik güncellenmiyor");
      else if (!r.available) toast("Uygulama güncel");
    }
  } catch (e) {
    if (manual) toast(`Güncelleme denetlenemedi: ${e}`, true);
    else console.warn("güncelleme kontrolü:", e);
  } finally {
    btn.classList.remove("spin");
  }
}

$("btn-check-update").onclick = () => checkUpdate(true);

$("btn-update").onclick = async () => {
  const btn = $("btn-update");
  btn.disabled = true;
  btn.textContent = "İndiriliyor…";
  try {
    // Başarılı olursa uygulama kendini yeniden başlatır.
    await invoke("update_apply");
  } catch (e) {
    toast(`Güncellenemedi: ${e}`, true);
    btn.disabled = false;
    btn.textContent = "Güncelle";
  }
};

// ----------------------------------------------------------------- başlat

function start() {
  $("auth").classList.add("hidden");
  $("app").classList.remove("hidden");
  connect();
}

localStorage.removeItem("base");
if (state.token) start();
else showAuth();
