#!/bin/zsh
# goodbyedc: Cloudflare token'larını panodan okuyup Worker'a kaydeder.
cd "$(dirname "$0")"
export PATH="/usr/local/bin:/opt/homebrew/bin:$PATH"
for pair in "SFU App Token:REALTIME_APP_TOKEN" "TURN API Token:TURN_KEY_TOKEN"; do
  label="${pair%%:*}"; name="${pair##*:}"
  echo ""
  echo "==> Cloudflare panelinden $label'i KOPYALA (Cmd+C), sonra burada Enter'a bas"
  read
  n=$(pbpaste | tr -d ' \n\r' | wc -c | tr -d ' ')
  echo "Karakter sayisi: $n"
  if [ "$n" -lt 20 ]; then echo "Cok kisa gorunuyor, kopyalama basarisiz olmus olabilir. Tekrar dene."; read; fi
  pbpaste | tr -d ' \n\r' | npx wrangler secret put "$name"
done
echo ""
echo "Bitti. Bu pencereyi kapatabilirsin."
