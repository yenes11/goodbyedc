# goodbyedc

Arkadaş grubu için küçük ve ücretsiz bir Discord alternatifi. Sadece **yazı**, **ses** ve **ekran paylaşımı** var.

```
┌────────── Masaüstü istemci (Rust + Tauri 2) ──────────┐
│  arayüz: client/ui   |   ses/ekran: libwebrtc (Rust)  │
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
| `client/src-tauri/` | Rust masaüstü uygulaması. Ses (mikrofon/hoparlör, yankı engelleme), ekran yakalama, WebRTC |
| `client/ui/` | Arayüz (saf HTML/CSS/JS, build adımı yok) |

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

Komut sana `https://goodbyedc.<hesabın>.workers.dev` gibi bir adres verir. Uygulamada **Sunucu adresi** alanına bu adres yazılır.

Yerelde denemek için `.dev.vars.example` dosyasını `.dev.vars` olarak kopyala, değerleri doldur ve `npm run dev` çalıştır. Adres `http://127.0.0.1:8787` olur.

## 3. Masaüstü uygulaması

Gerekenler: [Rust](https://rustup.rs). macOS'ta Xcode Command Line Tools, Windows'ta Visual Studio Build Tools (C++).
İlk derleme, hazır WebRTC kütüphanesini indirdiği için birkaç dakika sürer.

```bash
cd client/src-tauri
cargo run                 # geliştirme modunda çalıştır
```

Arkadaşlarına dağıtılacak kurulum dosyası için:

```bash
cargo install tauri-cli --version "^2"
cd client/src-tauri
cargo tauri icon icons/icon.png   # tüm platform ikonlarını üretir (bir kez)
cargo tauri build                 # macOS: .dmg / Windows: .msi + .exe / Linux: .deb + .AppImage
```

Çıktılar `client/src-tauri/target/release/bundle/` klasörüne düşer. Her işletim sistemi için o sistemde derlemek gerekir.
İmzasız uygulamalarda macOS'ta ilk açılışta *sağ tık → Aç*, Windows'ta ise SmartScreen uyarısında *Yine de çalıştır* demek gerekir.

### İzinler
- **macOS:** İlk ses bağlantısında mikrofon izni sorulur. Ekran paylaşımı için *Sistem Ayarları → Gizlilik ve Güvenlik → Ekran Kaydı* altında uygulamaya izin ver, sonra uygulamayı yeniden başlat.
- **Linux (Wayland):** Ekran paylaşımı PipeWire üzerinden çalışır.

---

## Nasıl kullanılır
- İlk girişte **Kayıt ol** seç, sunucu adresini ve davet kodunu gir.
- Soldaki **+** ile sunucu, kanal başlıklarının yanındaki **+** ile kanal eklenir.
- Bir **ses kanalına** tıklayınca bağlanırsın. Alttaki 🎙 susturur, 🎧 sağırlaştırır.
- 🖥 ekran paylaşımını başlatır. Paylaşan kişinin yanında **CANLI** yazar; ona tıklayınca ekranını izlersin.
  Görüntü sadece izleyenlere gönderilir, böylece bant genişliği boşa harcanmaz.

## Teknik notlar
- Bütün veriler tek bir Durable Object'teki SQLite veritabanında durur. Ücretsiz plan 5 GB depolama sağlıyor, bu da yıllarca yetecek kadar mesaj demek.
- SFU App Token'ı istemciye hiç gönderilmez. İstemci `/api/rtc/*` üzerinden Worker'a istek atar, Worker oturumun o kullanıcıya ait olduğunu doğrulayıp isteği Cloudflare'e iletir.
- Ses, libwebrtc'nin platform ses modülüyle çalışır: yankı engelleme, gürültü bastırma ve otomatik kazanç dahil.
- Ekran paylaşımı en fazla 1080p/30fps ve 3 Mbps. Ayarlar `client/src-tauri/src/screen.rs` ve `voice.rs` içinde.
- İzlenen ekran, Rust tarafında JPEG'e çevrilip arayüze gönderilir ve `<canvas>`'a çizilir.
