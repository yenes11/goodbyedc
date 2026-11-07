// goodbyedc arayüzü
// Yazı/sinyal: Worker'a WebSocket (bu dosya)
// Ses/ekran:   Rust tarafı (invoke ile çağrılır)

const tauri = window.__TAURI__;
const invoke = tauri ? tauri.core.invoke : async () => { throw new Error("Tauri dışında çalışıyor"); };

const $ = (id) => document.getElementById(id);

const state = {
  base: localStorage.getItem("base") || "",
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
  $("auth-url").value = state.base;
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
  const base = $("auth-url").value.trim().replace(/\/+$/, "");
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
    state.base = base;
    state.token = data.token;
    localStorage.setItem("base", base);
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
      $("me-name").textContent = m.me.username;
      $("me-avatar").outerHTML = `<div class="avatar" id="me-avatar" style="background:${colorFor(m.me.username)}">${esc(m.me.username[0].toUpperCase())}</div>`;
      // Yeniden bağlandıysak ses durumumuzu tekrar bildir.
      if (state.myVoice) {
        send({ t: "voice_join", ...state.myVoice, muted: state.muted, deafened: state.deafened });
      }
      // Mesajları yeniden yükle (koptuğumuz sırada gelenler olabilir).
      state.messages.clear();
      if (!state.servers.find((s) => s.id === state.serverId)) state.serverId = state.servers[0]?.id ?? null;
      selectServer(state.serverId);
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
      renderServers();
      break;
    case "server_deleted":
      state.servers = state.servers.filter((s) => s.id !== m.serverId);
      state.channels = state.channels.filter((c) => c.serverId !== m.serverId);
      if (state.serverId === m.serverId) selectServer(state.servers[0]?.id);
      else renderServers();
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
      renderChannels();
      syncVoice();
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

function renderServers() {
  const el = $("servers");
  el.innerHTML =
    state.servers
      .map((s) => {
        const initials = s.name.split(/\s+/).map((w) => w[0]).join("").slice(0, 3).toUpperCase();
        return `<div class="server-icon ${s.id === state.serverId ? "active" : ""}" data-id="${s.id}" title="${esc(s.name)}">${esc(initials)}</div>`;
      })
      .join("") + `<div class="sep"></div><div class="server-icon add" id="add-server" title="Sunucu ekle">+</div>`;
  el.querySelectorAll(".server-icon[data-id]").forEach((n) => (n.onclick = () => selectServer(Number(n.dataset.id))));
  $("add-server").onclick = () =>
    modal("Sunucu oluştur", `<input id="f-name" placeholder="Sunucu adı" maxlength="50" />`, (b) => {
      const name = b.querySelector("#f-name").value.trim();
      if (!name) return false;
      send({ t: "create_server", name });
    }, "Oluştur");
  const srv = state.servers.find((s) => s.id === state.serverId);
  $("server-name").textContent = srv ? srv.name : "";
}

function selectServer(id) {
  state.serverId = id;
  localStorage.setItem("serverId", id);
  const text = state.channels.filter((c) => c.serverId === id && c.kind === "text");
  const saved = Number(localStorage.getItem(`ch:${id}`));
  state.channelId = text.find((c) => c.id === saved)?.id ?? text[0]?.id ?? null;
  renderServers();
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

  const chanRow = (c, icon) =>
    `<div class="channel ${c.id === state.channelId ? "active" : ""}" data-id="${c.id}" data-kind="${c.kind}">
       <span class="hash">${icon}</span><span class="name">${esc(c.name)}</span>
       <button class="del" data-del="${c.id}" title="Kanalı sil">✕</button>
     </div>`;

  let html = `<div class="category"><span>Metin kanalları</span><button data-add="text" title="Kanal ekle">+</button></div>`;
  html += text.map((c) => chanRow(c, "#")).join("");
  html += `<div class="category"><span>Ses kanalları</span><button data-add="voice" title="Kanal ekle">+</button></div>`;
  for (const c of voice) {
    html += chanRow(c, "🔊");
    const users = state.voice.filter((v) => v.channelId === c.id);
    html += `<div class="voice-users">${users
      .map((u) => {
        const live = u.tracks.includes("screen");
        const isMe = u.userId === state.me?.id;
        const watching = state.watching === u.sessionId;
        return `<div class="voice-user">${avatar(u.username)}<span>${esc(u.username)}</span>
          <span class="flags">${u.muted ? "🔇" : ""}${u.deafened ? "🙉" : ""}
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
  $("btn-mute").classList.toggle("off", state.muted);
  $("btn-deafen").classList.toggle("off", state.deafened);
  $("btn-mute").textContent = state.muted ? "🔇" : "🎙";
  const srv = state.servers.find((s) => s.id === state.serverId);
  $("server-name").textContent = srv ? srv.name : "";
}

function renderMain() {
  const watchingUser = state.watching && state.voice.find((v) => v.sessionId === state.watching);
  $("stage").classList.toggle("hidden", !watchingUser);
  const ch = state.channels.find((c) => c.id === state.channelId);
  $("chat").classList.toggle("hidden", !!watchingUser || !ch);
  $("empty").classList.toggle("hidden", !!watchingUser || !!ch);

  if (watchingUser) {
    $("channel-title").textContent = `🖥 ${watchingUser.username} ekranını paylaşıyor`;
    $("stage-title").textContent = watchingUser.username;
    return;
  }
  if (!ch) {
    $("channel-title").textContent = "";
    return;
  }
  $("channel-title").textContent = `# ${ch.name}`;
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

$("server-delete").onclick = () => {
  const srv = state.servers.find((s) => s.id === state.serverId);
  if (!srv) return;
  modal("Sunucuyu sil", `<p>“${esc(srv.name)}” sunucusu, tüm kanalları ve mesajlarıyla silinecek. Emin misin?</p>`, () => {
    send({ t: "delete_server", serverId: srv.id });
  }, "Sil");
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
  let selected = sources[0];
  const html = sources
    .map((s, i) => `<div class="source ${i === 0 ? "selected" : ""}" data-i="${i}">${s.kind === "screen" ? "🖥" : "🪟"} ${esc(s.title)}</div>`)
    .join("");
  modal("Ekran paylaş", html, () => {
    queue(async () => {
      const tracks = await invoke("screen_start", { id: selected.id, kind: selected.kind });
      state.myVoice.tracks = tracks;
      send({ t: "voice_update", tracks });
      renderChannels();
    });
  }, "Paylaş");
  $("modal-body").querySelectorAll(".source").forEach((n) => {
    n.onclick = () => {
      $("modal-body").querySelectorAll(".source").forEach((x) => x.classList.remove("selected"));
      n.classList.add("selected");
      selected = sources[Number(n.dataset.i)];
    };
    n.ondblclick = () => $("modal-ok").click();
  });
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

// ----------------------------------------------------------------- başlat

function start() {
  $("auth").classList.add("hidden");
  $("app").classList.remove("hidden");
  connect();
}

if (state.token && state.base) start();
else showAuth();
