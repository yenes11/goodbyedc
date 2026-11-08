# goodbyedc

Arkadaş grubu için küçük ve ücretsiz bir Discord alternatifi. Sadece **yazı**, **ses** ve **ekran paylaşımı** var.

```
┌──────────── Masaüstü istemci (tamamı Rust) ───────────┐
│  arayüz: egui (native) | ses/ekran: libwebrtc         │
└──────┬───────────────────────────────┬────────────────┘
       │ WebSocket (yazı + durum)      │ WebRTC (ses/video)
┌──────▼───────────────────┐   ┌───────▼────────────────────┐
│ Cloudflare Worker +      │   │ Cloudflare Realtime        │
│ Durable Object (SQLite)  │   │ SFU + TURN                 │
│ server/                  │   │ (ayda 1000 GB ücretsiz)    │
└──────────────────────────┘   └────────────────────────────┘
```

| Klasör | İçerik |
|---|---|
| `server/` | Cloudflare Worker (TypeScript). Kullanıcılar, sunucular, kanallar, mesajlar ve SFU proxy'si |
| `client/` | Rust masaüstü uygulaması. Arayüz egui ile native çizilir (web görüntüleyici yok). Ses (mikrofon/hoparlör, yankı engelleme), ekran yakalama, WebRTC, kendini güncelleme |

---

## 1. Cloudflare kurulumu (bir kez, ücretsiz)

1. https://dash.cloudflare.com adresinden ücretsiz hesap aç.
2. **Realtime → SFU** bölümünde yeni bir uygulama oluştur. **App ID** ve **App Token**'ı not al.
3. (Önerilir) **Realtime → TURN** bölümünde bir TURN anahtarı oluştur. **Key ID** ve **API Token**'ı not al.
   TURN olmadan da çalışır, ama bazı ağlarda (katı NAT, şirket ağı) ses bağlanmayabilir.

## 2. Sunucuyu yayınla

```bash
cd server
npm install
npx wrangler login
```

`wrangler.jsonc` içindeki `REALTIME_APP_ID` ve `TURN_KEY_ID` alanlarını doldur, sonra gizli değerleri gir:

```bash
npx wrangler secret put INVITE_CODE          # arkadaşlarına vereceğin davet kodu
npx wrangler secret put REALTIME_APP_TOKEN
npx wrangler secret put TURN_KEY_TOKEN       # TURN kullanmıyorsan atla
npx wrangler deploy
```

Komut sana `https://goodbyedc.<hesabın>.workers.dev` gibi bir adres verir. Bu adres `client/src/config.rs` içinde sabit yazılıdır.

Yerelde denemek için `.dev.vars.example` dosyasını `.dev.vars` olarak kopyala, değerleri doldur ve `npm run dev` çalıştır. Adres `http://127.0.0.1:8787` olur.

## 3. Masaüstü uygulaması

Gerekenler: [Rust](https://rustup.rs). macOS'ta Xcode Command Line Tools, Windows'ta Visual Studio Build Tools (C++).
İlk derleme, hazır WebRTC kütüphanesini indirdiği için birkaç dakika sürer.

```bash
cd client
cargo run                 # geliştirme modunda çalıştır
cargo build --release     # tek dosyalık uygulama: target/release/goodbyedc(.exe)
```

**Windows sürümü otomatik:** `main` dalına `client/` içinde bir değişiklik push'lanınca GitHub Actions
Windows exe'sini derler ve en son sürüm (latest release) olarak yayınlar. Uygulamadaki
**Güncellemeleri denetle** düğmesi bu sürümü indirip kendini günceller. Sadece GitHub'da derlenen
exe'ler kendini günceller; elle derlenenler güncelleme aramaz.

İmzasız uygulamalarda macOS'ta ilk açılışta *sağ tık → Aç*, Windows'ta ise SmartScreen uyarısında *Yine de çalıştır* demek gerekir.

Geliştirme için ortam değişkenleri: `GOODBYEDC_SERVER` (başka sunucu), `GOODBYEDC_CONFIG_DIR` (ayar klasörü).

### İzinler
- **macOS:** İlk ses bağlantısında mikrofon izni sorulur. Ekran paylaşımı için *Sistem Ayarları → Gizlilik ve Güvenlik → Ekran Kaydı* altında uygulamaya izin ver, sonra uygulamayı yeniden başlat.
- **Linux (Wayland):** Ekran paylaşımı PipeWire üzerinden çalışır.

---

## Nasıl kullanılır
- İlk girişte **Kayıt ol** seç ve davet kodunu gir.
- Kanal başlıklarının yanındaki **+** ile kanal eklenir.
- Bir **ses kanalına** tıklayınca bağlanırsın. Alttaki mikrofon düğmesi susturur, kulaklık sağırlaştırır.
- Ses panelindeki ekran düğmesi paylaşımı başlatır. Paylaşan kişinin yanında **CANLI** yazar; ona tıklayınca ekranını izlersin.
  Görüntü sadece izleyenlere gönderilir, böylece bant genişliği boşa harcanmaz.

## Teknik notlar
- Bütün veriler tek bir Durable Object'teki SQLite veritabanında durur. Ücretsiz plan 5 GB depolama sağlıyor, bu da yıllarca yetecek kadar mesaj demek.
- SFU App Token'ı istemciye hiç gönderilmez. İstemci `/api/rtc/*` üzerinden Worker'a istek atar, Worker oturumun o kullanıcıya ait olduğunu doğrulayıp isteği Cloudflare'e iletir.
- Ses, libwebrtc'nin platform ses modülüyle çalışır: yankı engelleme, gürültü bastırma ve otomatik kazanç dahil.
- Ekran paylaşımı en fazla 1080p/30fps ve 3 Mbps. Ayarlar `client/src/screen.rs` ve `voice.rs` içinde.
- İzlenen ekran RGBA'ya çevrilip doğrudan GPU dokusu olarak çizilir.
- Ayarlar (giriş, susturma, son kanal) macOS'ta `~/Library/Application Support/goodbyedc/`, Windows'ta `%APPDATA%\goodbyedc\` altında tutulur.
