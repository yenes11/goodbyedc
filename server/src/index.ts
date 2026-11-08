import { DurableObject } from "cloudflare:workers";

// ---------------------------------------------------------------------------
// goodbyedc backend
//
// Tek bir Durable Object ("Hub") tüm uygulamayı yönetir: kullanıcılar,
// sunucular, kanallar, mesajlar (SQLite) ve WebSocket bağlantıları.
// Ses/ekran paylaşımı Cloudflare Realtime SFU üzerinden geçer; bu Worker
// sadece SFU API'sine giden istekleri doğrulayıp (token ekleyerek) iletir.
// ---------------------------------------------------------------------------

export interface Env {
  HUB: DurableObjectNamespace<Hub>;
  INVITE_CODE: string;
  REALTIME_APP_ID: string;
  REALTIME_APP_TOKEN: string;
  TURN_KEY_ID?: string;
  TURN_KEY_TOKEN?: string;
  /** Son 31 gündeki tahmini SFU trafiği için üst sınır (GB). Varsayılan 800. */
  USAGE_LIMIT_GB?: string;
}

const CORS: Record<string, string> = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, POST, PUT, OPTIONS",
  "Access-Control-Allow-Headers": "Authorization, Content-Type",
};

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json", ...CORS },
  });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    if (req.method === "OPTIONS") return new Response(null, { headers: CORS });
    const url = new URL(req.url);
    if (!url.pathname.startsWith("/api/")) {
      return new Response("goodbyedc sunucusu calisiyor", { headers: CORS });
    }
    // 5-20 kişi için tek bir Hub yeterli.
    const hub = env.HUB.get(env.HUB.idFromName("main"));
    return hub.fetch(req);
  },
} satisfies ExportedHandler<Env>;

// ---------------------------------------------------------------------------

type ChannelKind = "text" | "voice";

interface VoiceInfo {
  channelId: number;
  sessionId: string;
  tracks: string[]; // "mic", "screen"
  muted: boolean;
  deafened: boolean;
  watching: string | null; // ekranını izlediği oturum id'si
}

interface Attachment {
  userId: number;
  username: string;
  voice: VoiceInfo | null;
}

const MAX_MESSAGE_LEN = 4000;
const HISTORY_PAGE = 50;
const PBKDF2_ITERATIONS = 100_000;
const RTC_BASE = "https://rtc.live.cloudflare.com/v1";

// ---- Kullanım tahmini (bilerek yüksek tutulur) ----
// SFU'dan bir istemciye giden bir ses akışı: Opus + RTP/UDP/IP ek yükü.
const AUDIO_BPS = 64_000;
// Bir izleyiciye giden ekran görüntüsü: istemcideki 3 Mbps tavan + ek yük.
const SCREEN_BPS = 3_300_000;
const WINDOW_DAYS = 31;
const USAGE_TICK_MS = 60_000;
const LIMIT_MSG = "Son 31 günün ücretsiz trafik sınırına ulaşıldı. Sesli sohbet geçici olarak kapalı.";

export class Hub extends DurableObject<Env> {
  sql: SqlStorage;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`
      CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE COLLATE NOCASE,
        pass_hash TEXT NOT NULL,
        salt TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS tokens (
        token TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS servers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS channels (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        server_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        channel_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        content TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_messages_channel ON messages(channel_id, id);
      CREATE TABLE IF NOT EXISTS usage_daily (
        day TEXT PRIMARY KEY,
        bytes REAL NOT NULL
      );
      CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS rtc_sessions (
        session_id TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL,
        created_at INTEGER NOT NULL
      );
    `);
    // İlk çalıştırmada varsayılan sunucu.
    const count = this.sql.exec("SELECT COUNT(*) AS c FROM servers").one().c as number;
    if (count === 0) {
      const now = Date.now();
      const sid = this.sql
        .exec("INSERT INTO servers (name, created_at) VALUES (?, ?) RETURNING id", "Ana Sunucu", now)
        .one().id as number;
      this.sql.exec(
        "INSERT INTO channels (server_id, name, kind, created_at) VALUES (?, 'genel', 'text', ?), (?, 'Sohbet', 'voice', ?)",
        sid, now, sid, now,
      );
    }
    // Hibernation sırasında DO'yu uyandırmadan ping'e cevap ver.
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  // ------------------------------------------------------------------ HTTP

  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    const path = url.pathname;
    try {
      if (path === "/api/register" && req.method === "POST") return await this.register(req);
      if (path === "/api/login" && req.method === "POST") return await this.login(req);
      if (path === "/api/ws") return this.websocket(req, url);
      if (path === "/api/ice" && req.method === "GET") {
        const user = this.authHeader(req);
        if (!user) return json({ error: "yetkisiz" }, 401);
        return await this.iceServers();
      }
      if (path.startsWith("/api/rtc/")) {
        const user = this.authHeader(req);
        if (!user) return json({ error: "yetkisiz" }, 401);
        return await this.rtcProxy(req, path.slice("/api/rtc".length), user.id);
      }
      return json({ error: "bulunamadı" }, 404);
    } catch (e) {
      return json({ error: String(e) }, 500);
    }
  }

  async register(req: Request): Promise<Response> {
    const body = (await req.json()) as { username?: string; password?: string; invite?: string };
    const username = (body.username ?? "").trim();
    const password = body.password ?? "";
    if (!this.env.INVITE_CODE || body.invite !== this.env.INVITE_CODE) {
      return json({ error: "Davet kodu hatalı" }, 403);
    }
    if (!/^[\p{L}\p{N}_.-]{2,24}$/u.test(username)) {
      return json({ error: "Kullanıcı adı 2-24 karakter olmalı (harf, rakam, _ . -)" }, 400);
    }
    if (password.length < 6) return json({ error: "Şifre en az 6 karakter olmalı" }, 400);
    const exists = this.sql.exec("SELECT id FROM users WHERE username = ?", username).toArray();
    if (exists.length) return json({ error: "Bu kullanıcı adı alınmış" }, 409);

    const salt = randomHex(16);
    const hash = await hashPassword(password, salt);
    const id = this.sql
      .exec(
        "INSERT INTO users (username, pass_hash, salt, created_at) VALUES (?, ?, ?, ?) RETURNING id",
        username, hash, salt, Date.now(),
      )
      .one().id as number;
    return json({ token: this.newToken(id), user: { id, username } });
  }

  async login(req: Request): Promise<Response> {
    const body = (await req.json()) as { username?: string; password?: string };
    const rows = this.sql
      .exec("SELECT id, username, pass_hash, salt FROM users WHERE username = ?", (body.username ?? "").trim())
      .toArray();
    const row = rows[0];
    if (!row || (await hashPassword(body.password ?? "", row.salt as string)) !== row.pass_hash) {
      return json({ error: "Kullanıcı adı veya şifre hatalı" }, 401);
    }
    const id = row.id as number;
    return json({ token: this.newToken(id), user: { id, username: row.username } });
  }

  newToken(userId: number): string {
    const token = randomHex(32);
    this.sql.exec("INSERT INTO tokens (token, user_id, created_at) VALUES (?, ?, ?)", token, userId, Date.now());
    return token;
  }

  userByToken(token: string | null): { id: number; username: string } | null {
    if (!token) return null;
    const row = this.sql
      .exec("SELECT u.id, u.username FROM tokens t JOIN users u ON u.id = t.user_id WHERE t.token = ?", token)
      .toArray()[0];
    return row ? { id: row.id as number, username: row.username as string } : null;
  }

  authHeader(req: Request) {
    const h = req.headers.get("Authorization") ?? "";
    return this.userByToken(h.startsWith("Bearer ") ? h.slice(7) : null);
  }

  // ---------------------------------------------------------- Realtime SFU

  async iceServers(): Promise<Response> {
    const servers: unknown[] = [{ urls: ["stun:stun.cloudflare.com:3478"] }];
    if (this.env.TURN_KEY_ID && this.env.TURN_KEY_TOKEN) {
      const r = await fetch(`${RTC_BASE}/turn/keys/${this.env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
        method: "POST",
        headers: { Authorization: `Bearer ${this.env.TURN_KEY_TOKEN}`, "Content-Type": "application/json" },
        body: JSON.stringify({ ttl: 86400 }),
      });
      if (r.ok) {
        const data = (await r.json()) as { iceServers: unknown };
        const list = Array.isArray(data.iceServers) ? data.iceServers : [data.iceServers];
        servers.push(...list);
      }
    }
    return json({ iceServers: servers });
  }

  /**
   * İstemciden gelen SFU isteklerini Cloudflare Realtime API'sine iletir.
   * İzin verilen uç noktalar:
   *   POST /sessions/new
   *   POST /sessions/:id/tracks/new
   *   PUT  /sessions/:id/renegotiate
   *   PUT  /sessions/:id/tracks/close
   * Kullanıcı sadece kendi oluşturduğu oturumlar üzerinde işlem yapabilir.
   */
  async rtcProxy(req: Request, sub: string, userId: number): Promise<Response> {
    if (!this.env.REALTIME_APP_ID || !this.env.REALTIME_APP_TOKEN) {
      return json({ error: "Sunucuda REALTIME_APP_ID / REALTIME_APP_TOKEN ayarlanmamış" }, 500);
    }
    if ((sub === "/sessions/new" || sub.endsWith("/tracks/new")) && this.overLimit()) {
      return json({ error: LIMIT_MSG }, 403);
    }
    const target = `${RTC_BASE}/apps/${this.env.REALTIME_APP_ID}${sub}`;
    const headers = { Authorization: `Bearer ${this.env.REALTIME_APP_TOKEN}`, "Content-Type": "application/json" };

    if (sub === "/sessions/new" && req.method === "POST") {
      const r = await fetch(target, { method: "POST", headers });
      const data = (await r.json()) as { sessionId?: string };
      if (r.ok && data.sessionId) {
        this.sql.exec(
          "INSERT OR REPLACE INTO rtc_sessions (session_id, user_id, created_at) VALUES (?, ?, ?)",
          data.sessionId, userId, Date.now(),
        );
        // Eski oturum kayıtlarını temizle (1 günden eski).
        this.sql.exec("DELETE FROM rtc_sessions WHERE created_at < ?", Date.now() - 86_400_000);
      }
      return json(data, r.status);
    }

    const m = sub.match(/^\/sessions\/([A-Za-z0-9_-]+)\/(tracks\/new|renegotiate|tracks\/close)$/);
    if (!m) return json({ error: "izin verilmeyen uç nokta" }, 403);
    const [, sessionId, action] = m;
    const owner = this.sql.exec("SELECT user_id FROM rtc_sessions WHERE session_id = ?", sessionId).toArray()[0];
    if (!owner || owner.user_id !== userId) return json({ error: "bu oturum size ait değil" }, 403);
    const method = action === "tracks/new" ? "POST" : "PUT";
    if (req.method !== method) return json({ error: "yanlış method" }, 405);

    const r = await fetch(target, { method, headers, body: await req.text() });
    return new Response(r.body, { status: r.status, headers: { "Content-Type": "application/json", ...CORS } });
  }

  // ------------------------------------------------------------- WebSocket

  websocket(req: Request, url: URL): Response {
    if (req.headers.get("Upgrade") !== "websocket") return json({ error: "websocket bekleniyor" }, 426);
    const user = this.userByToken(url.searchParams.get("token"));
    if (!user) return json({ error: "yetkisiz" }, 401);

    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    this.ctx.acceptWebSocket(server);
    const att: Attachment = { userId: user.id, username: user.username, voice: null };
    server.serializeAttachment(att);

    server.send(JSON.stringify({
      t: "ready",
      me: user,
      servers: this.sql.exec("SELECT id, name FROM servers ORDER BY id").toArray(),
      channels: this.sql.exec("SELECT id, server_id AS serverId, name, kind FROM channels ORDER BY id").toArray(),
      voice: this.voiceState(),
      online: this.onlineUsers(),
      usage: this.usageInfo(),
    }));
    this.broadcast({ t: "presence", online: this.onlineUsers() });
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    if (typeof raw !== "string") return;
    let msg: any;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    const att = ws.deserializeAttachment() as Attachment;
    const reply = (data: unknown) => ws.send(JSON.stringify(data));
    const err = (e: string) => reply({ t: "error", error: e });

    switch (msg.t) {
      case "send": {
        const content = String(msg.content ?? "").trim();
        if (!content || content.length > MAX_MESSAGE_LEN) return err("Mesaj boş veya çok uzun");
        if (!this.channelExists(msg.channelId, "text")) return err("Kanal bulunamadı");
        const now = Date.now();
        const id = this.sql
          .exec(
            "INSERT INTO messages (channel_id, user_id, content, created_at) VALUES (?, ?, ?, ?) RETURNING id",
            msg.channelId, att.userId, content, now,
          )
          .one().id as number;
        this.broadcast({
          t: "message",
          message: { id, channelId: msg.channelId, userId: att.userId, username: att.username, content, createdAt: now },
        });
        return;
      }
      case "history": {
        const before = Number.isFinite(msg.before) ? msg.before : Number.MAX_SAFE_INTEGER;
        const rows = this.sql
          .exec(
            `SELECT m.id, m.channel_id AS channelId, m.user_id AS userId, u.username, m.content, m.created_at AS createdAt
             FROM messages m JOIN users u ON u.id = m.user_id
             WHERE m.channel_id = ? AND m.id < ? ORDER BY m.id DESC LIMIT ?`,
            msg.channelId, before, HISTORY_PAGE,
          )
          .toArray()
          .reverse();
        return reply({ t: "history", channelId: msg.channelId, messages: rows, more: rows.length === HISTORY_PAGE });
      }
      case "create_server": {
        const name = String(msg.name ?? "").trim().slice(0, 50);
        if (!name) return err("Sunucu adı boş olamaz");
        const now = Date.now();
        const sid = this.sql
          .exec("INSERT INTO servers (name, created_at) VALUES (?, ?) RETURNING id", name, now)
          .one().id as number;
        const tc = this.sql
          .exec("INSERT INTO channels (server_id, name, kind, created_at) VALUES (?, 'genel', 'text', ?) RETURNING id", sid, now)
          .one().id as number;
        const vc = this.sql
          .exec("INSERT INTO channels (server_id, name, kind, created_at) VALUES (?, 'Sohbet', 'voice', ?) RETURNING id", sid, now)
          .one().id as number;
        this.broadcast({ t: "server_created", server: { id: sid, name } });
        this.broadcast({ t: "channel_created", channel: { id: tc, serverId: sid, name: "genel", kind: "text" } });
        this.broadcast({ t: "channel_created", channel: { id: vc, serverId: sid, name: "Sohbet", kind: "voice" } });
        return;
      }
      case "delete_server": {
        const sid = Number(msg.serverId);
        const servers = this.sql.exec("SELECT COUNT(*) AS c FROM servers").one().c as number;
        if (servers <= 1) return err("Son sunucu silinemez");
        this.sql.exec("DELETE FROM messages WHERE channel_id IN (SELECT id FROM channels WHERE server_id = ?)", sid);
        this.sql.exec("DELETE FROM channels WHERE server_id = ?", sid);
        this.sql.exec("DELETE FROM servers WHERE id = ?", sid);
        this.broadcast({ t: "server_deleted", serverId: sid });
        return;
      }
      case "create_channel": {
        const name = String(msg.name ?? "").trim().slice(0, 50);
        const kind: ChannelKind = msg.kind === "voice" ? "voice" : "text";
        if (!name) return err("Kanal adı boş olamaz");
        const srv = this.sql.exec("SELECT id FROM servers WHERE id = ?", msg.serverId).toArray()[0];
        if (!srv) return err("Sunucu bulunamadı");
        const id = this.sql
          .exec(
            "INSERT INTO channels (server_id, name, kind, created_at) VALUES (?, ?, ?, ?) RETURNING id",
            msg.serverId, name, kind, Date.now(),
          )
          .one().id as number;
        this.broadcast({ t: "channel_created", channel: { id, serverId: msg.serverId, name, kind } });
        return;
      }
      case "delete_channel": {
        const id = Number(msg.channelId);
        this.sql.exec("DELETE FROM messages WHERE channel_id = ?", id);
        this.sql.exec("DELETE FROM channels WHERE id = ?", id);
        this.broadcast({ t: "channel_deleted", channelId: id });
        return;
      }
      case "voice_join": {
        if (!this.channelExists(msg.channelId, "voice")) return err("Ses kanalı bulunamadı");
        if (this.overLimit()) {
          reply({ t: "voice_disabled", error: LIMIT_MSG });
          return;
        }
        att.voice = {
          channelId: msg.channelId,
          sessionId: String(msg.sessionId),
          tracks: sanitizeTracks(msg.tracks),
          muted: !!msg.muted,
          deafened: !!msg.deafened,
          watching: null,
        };
        ws.serializeAttachment(att);
        await this.broadcastVoice();
        return;
      }
      case "voice_update": {
        if (!att.voice) return;
        if (msg.tracks !== undefined) att.voice.tracks = sanitizeTracks(msg.tracks);
        if (msg.muted !== undefined) att.voice.muted = !!msg.muted;
        if (msg.deafened !== undefined) att.voice.deafened = !!msg.deafened;
        if (msg.watching !== undefined) att.voice.watching = typeof msg.watching === "string" ? msg.watching : null;
        ws.serializeAttachment(att);
        await this.broadcastVoice();
        return;
      }
      case "voice_leave": {
        att.voice = null;
        ws.serializeAttachment(att);
        await this.broadcastVoice();
        return;
      }
    }
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    await this.onSocketGone(ws);
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    await this.onSocketGone(ws);
  }

  async onSocketGone(ws: WebSocket) {
    try {
      ws.close(1000, "bye");
    } catch {}
    await this.broadcastVoice(ws);
    this.broadcast({ t: "presence", online: this.onlineUsers(ws) }, ws);
  }

  // --------------------------------------------------------------- helpers

  channelExists(id: unknown, kind: ChannelKind): boolean {
    return this.sql.exec("SELECT id FROM channels WHERE id = ? AND kind = ?", Number(id), kind).toArray().length > 0;
  }

  sockets(exclude?: WebSocket): WebSocket[] {
    return this.ctx.getWebSockets().filter((s) => s !== exclude && s.readyState === WebSocket.OPEN);
  }

  voiceState(exclude?: WebSocket) {
    const out: Array<{ userId: number; username: string } & VoiceInfo> = [];
    for (const s of this.sockets(exclude)) {
      const a = s.deserializeAttachment() as Attachment | null;
      if (a?.voice) out.push({ userId: a.userId, username: a.username, ...a.voice });
    }
    return out;
  }

  onlineUsers(exclude?: WebSocket) {
    const map = new Map<number, string>();
    for (const s of this.sockets(exclude)) {
      const a = s.deserializeAttachment() as Attachment | null;
      if (a) map.set(a.userId, a.username);
    }
    return [...map].map(([id, username]) => ({ id, username }));
  }

  async broadcastVoice(exclude?: WebSocket) {
    await this.updateUsage(exclude);
    this.broadcast({ t: "voice", voice: this.voiceState(exclude), usage: this.usageInfo() }, exclude);
  }

  // ---------------------------------------------------------- kullanım

  /** Dakikada bir: biriken trafiği yaz, sınırı kontrol et. */
  async alarm(): Promise<void> {
    await this.updateUsage();
    if (this.voiceState().length) {
      this.broadcast({ t: "voice", voice: this.voiceState(), usage: this.usageInfo() });
    }
  }

  getKV(key: string): string | null {
    const row = this.sql.exec("SELECT value FROM kv WHERE key = ?", key).toArray()[0];
    return row ? (row.value as string) : null;
  }

  setKV(key: string, value: string) {
    this.sql.exec("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)", key, value);
  }

  /** Mevcut ses durumuna göre SFU'dan istemcilere giden tahmini bit/sn. */
  computeRate(voice: Array<{ channelId: number; sessionId: string } & VoiceInfo>): number {
    let bps = 0;
    for (const p of voice) {
      const same = voice.filter((o) => o.channelId === p.channelId && o.sessionId !== p.sessionId);
      // p, kanaldaki herkesin mikrofonunu çeker.
      bps += same.filter((o) => o.tracks.includes("mic")).length * AUDIO_BPS;
      // p birinin ekranını izliyorsa.
      if (p.watching && same.some((o) => o.sessionId === p.watching && o.tracks.includes("screen"))) {
        bps += SCREEN_BPS;
      }
    }
    return bps;
  }

  /** Son güncellemeden bu yana geçen trafiği ekler ve yeni hızı kaydeder. */
  async updateUsage(exclude?: WebSocket) {
    const now = Date.now();
    const last = Number(this.getKV("usage_last") ?? now);
    const rate = Number(this.getKV("usage_rate") ?? 0);
    if (rate > 0 && now > last) {
      const bytes = (rate / 8) * ((now - last) / 1000);
      const day = new Date(now).toISOString().slice(0, 10);
      this.sql.exec(
        "INSERT INTO usage_daily (day, bytes) VALUES (?, ?) ON CONFLICT(day) DO UPDATE SET bytes = bytes + excluded.bytes",
        day, bytes,
      );
    }
    this.setKV("usage_last", String(now));

    let newRate = this.computeRate(this.voiceState(exclude));
    if (newRate > 0 && this.overLimit()) {
      this.disableVoice(exclude);
      newRate = 0;
    }
    this.setKV("usage_rate", String(newRate));
    if (newRate > 0 && (await this.ctx.storage.getAlarm()) === null) {
      await this.ctx.storage.setAlarm(now + USAGE_TICK_MS);
    }
    this.sql.exec("DELETE FROM usage_daily WHERE day < ?", dayOffset(-40));
  }

  /** Sınır aşıldı: herkesi ses kanalından çıkar. */
  disableVoice(exclude?: WebSocket) {
    for (const s of this.sockets(exclude)) {
      const a = s.deserializeAttachment() as Attachment | null;
      if (a?.voice) {
        a.voice = null;
        s.serializeAttachment(a);
      }
    }
    this.broadcast({ t: "voice_disabled", error: LIMIT_MSG }, exclude);
  }

  limitBytes(): number {
    const gb = Number(this.env.USAGE_LIMIT_GB || 800);
    return (Number.isFinite(gb) && gb > 0 ? gb : 800) * 1e9;
  }

  usedBytes(): number {
    const row = this.sql
      .exec("SELECT COALESCE(SUM(bytes), 0) AS b FROM usage_daily WHERE day > ?", dayOffset(-WINDOW_DAYS))
      .one();
    return row.b as number;
  }

  overLimit(): boolean {
    return this.usedBytes() >= this.limitBytes();
  }

  usageInfo() {
    return { usedGB: +(this.usedBytes() / 1e9).toFixed(2), limitGB: this.limitBytes() / 1e9 };
  }

  broadcast(data: unknown, exclude?: WebSocket) {
    const text = JSON.stringify(data);
    for (const s of this.sockets(exclude)) {
      try {
        s.send(text);
      } catch {}
    }
  }
}

function dayOffset(days: number): string {
  return new Date(Date.now() + days * 86_400_000).toISOString().slice(0, 10);
}

function sanitizeTracks(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  return v.filter((t) => t === "mic" || t === "screen");
}

function randomHex(bytes: number): string {
  const a = crypto.getRandomValues(new Uint8Array(bytes));
  return [...a].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function hashPassword(password: string, saltHex: string): Promise<string> {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(password), "PBKDF2", false, ["deriveBits"]);
  const salt = new Uint8Array(saltHex.match(/../g)!.map((h) => parseInt(h, 16)));
  const bits = await crypto.subtle.deriveBits({ name: "PBKDF2", hash: "SHA-256", salt, iterations: PBKDF2_ITERATIONS }, key, 256);
  return [...new Uint8Array(bits)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
