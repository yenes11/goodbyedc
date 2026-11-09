//! Links in chat: finding them and previewing them (page title, description, image).
//!
//! Preview data is fetched by each client on its own: the page's HTML is
//! downloaded and its Open Graph tags (og:title, og:description, og:image,
//! og:site_name — the metadata sites provide for link previews) are read; the
//! image is shrunk and handed to the UI. If the link points directly at an image,
//! the image itself is shown.
//!
//! This file also shows a typical Rust pattern for text: Rust strings are UTF-8,
//! where one character can take 1–4 bytes. So "character index" and "byte index"
//! differ for text like "ğüş". `find_urls` works with *character* indices (by
//! first collecting `chars()` into a Vec), which the UI later needs.

use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::StreamExt;

/// A preview ready for the UI.
pub struct Preview {
    pub site: Option<String>,
    pub title: Option<String>,
    pub desc: Option<String>,
    /// (width, height, RGBA pixels)
    pub image: Option<(usize, usize, Vec<u8>)>,
}

/// Character ranges `(start, end)` of the links in `text` (character indices,
/// not byte indices; `end` is exclusive).
pub fn find_urls(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
        let starts = rest.starts_with("https://") || rest.starts_with("http://");
        // Don't start in the middle of a word.
        if starts && (i == 0 || chars[i - 1].is_whitespace() || "([<\"'".contains(chars[i - 1])) {
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() && !"<>\"".contains(chars[j]) {
                j += 1;
            }
            // Trailing punctuation isn't part of the link ("see https://x.com.").
            while j > i && ".,!?;:'\")]}".contains(chars[j - 1]) {
                // Keep a closing parenthesis if the link has a matching opening one
                // (Wikipedia links like .../Rust_(programming_language)).
                if chars[j - 1] == ')' {
                    let open = chars[i..j].iter().filter(|c| **c == '(').count();
                    let close = chars[i..j].iter().filter(|c| **c == ')').count();
                    if close <= open {
                        break;
                    }
                }
                j -= 1;
            }
            if j - i > 10 {
                out.push((i, j));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Returns the text in a character range.
pub fn slice_chars(text: &str, (a, b): (usize, usize)) -> String {
    text.chars().skip(a).take(b - a).collect()
}

const MAX_HTML: usize = 3 * 1024 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;

pub async fn fetch(http: &reqwest::Client, url: &str) -> Result<Preview> {
    let resp = http
        .get(url)
        .timeout(Duration::from_secs(10))
        .header("User-Agent", "Mozilla/5.0 (compatible; goodbyedc link preview)")
        .header("Accept-Language", "tr,en;q=0.8")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    let final_url = resp.url().clone();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    // The link points directly at an image.
    if ctype.starts_with("image/") {
        let bytes = read_limited(resp, MAX_IMAGE).await?;
        let image = tokio::task::spawn_blocking(move || decode_thumb(&bytes)).await?;
        return Ok(Preview { site: None, title: None, desc: None, image });
    }
    if !ctype.contains("html") {
        return Err(anyhow!("önizlenemeyen içerik: {ctype}"));
    }

    let bytes = read_html(resp).await?;
    let html = String::from_utf8_lossy(&bytes).to_string();
    let meta = parse_meta(&html);
    let get = |keys: &[&str]| keys.iter().find_map(|k| meta.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()));

    let title = get(&["og:title", "twitter:title"]).or_else(|| html_title(&html));
    let desc = get(&["og:description", "twitter:description", "description"]);
    let site = get(&["og:site_name"]).or_else(|| final_url.host_str().map(|h| h.trim_start_matches("www.").to_string()));
    let image_url = get(&["og:image:secure_url", "og:image", "og:image:url", "twitter:image", "twitter:image:src"]);

    if title.is_none() && desc.is_none() && image_url.is_none() {
        return Err(anyhow!("önizleme bilgisi yok"));
    }

    let mut image = None;
    if let Some(src) = image_url {
        if let Ok(img_url) = final_url.join(&src) {
            let res = async {
                let r = http
                    .get(img_url)
                    .timeout(Duration::from_secs(10))
                    .header("User-Agent", "Mozilla/5.0 (compatible; goodbyedc link preview)")
                    .send()
                    .await?;
                if !r.status().is_success() {
                    return Err(anyhow!("resim HTTP {}", r.status()));
                }
                let bytes = read_limited(r, MAX_IMAGE).await?;
                Ok::<_, anyhow::Error>(tokio::task::spawn_blocking(move || decode_thumb(&bytes)).await?)
            }
            .await;
            match res {
                Ok(i) => image = i,
                Err(e) => log::debug!("önizleme resmi alınamadı: {e:#}"),
            }
        }
    }

    Ok(Preview {
        site,
        title: title.map(|t| clip(&t, 140)),
        desc: desc.map(|d| clip(&d, 280)),
        image,
    })
}

/// Reads the HTML until `</head>` is seen (at most MAX_HTML): all the tags we
/// need are in the head, so the (often huge) body can be skipped.
async fn read_html(resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let from = out.len().saturating_sub(8);
        out.extend_from_slice(&chunk);
        if out[from..].windows(7).any(|w| w.eq_ignore_ascii_case(b"</head>")) || out.len() >= MAX_HTML {
            break;
        }
    }
    Ok(out)
}

async fn read_limited(resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = max.saturating_sub(out.len());
        out.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if out.len() >= max {
            break;
        }
    }
    Ok(out)
}

/// Decodes the image (PNG/JPEG/WebP/GIF via the `image` crate) and shrinks it
/// to at most 480x320. Runs on a blocking thread (`spawn_blocking`) because
/// decoding is CPU work that would otherwise stall the async runtime.
fn decode_thumb(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > 480 || img.height() > 320 { img.thumbnail(480, 320) } else { img };
    let rgba = img.to_rgba8();
    Some((rgba.width() as usize, rgba.height() as usize, rgba.into_raw()))
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>().trim_end())
    }
}

/// Collects `<meta property|name="..." content="...">` tags (names lowercased).
///
/// A tiny hand-written parser instead of a full HTML library: it only needs the
/// `<meta>` tags in the head. `to_ascii_lowercase` (not `to_lowercase`) is used
/// for the search copy because it never changes the byte length, so byte offsets
/// found in `lower` are valid in `html` too.
fn parse_meta(html: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0;
    // Only look at the <head> part (if there is one).
    let end = lower.find("</head>").unwrap_or(lower.len());
    while let Some(i) = lower[pos..end].find("<meta") {
        let start = pos + i;
        let Some(close) = lower[start..end].find('>') else { break };
        let tag = &html[start..start + close];
        let attrs = parse_attrs(tag);
        let name = attrs
            .iter()
            .find(|(k, _)| k == "property" || k == "name")
            .map(|(_, v)| v.to_lowercase());
        let content = attrs.iter().find(|(k, _)| k == "content").map(|(_, v)| decode_entities(v));
        if let (Some(n), Some(c)) = (name, content) {
            if !c.trim().is_empty() {
                out.push((n, c.trim().to_string()));
            }
        }
        pos = start + close + 1;
    }
    out
}

/// Extracts a tag's attributes as (lowercased name, value) pairs.
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = tag.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    // Skip the tag name
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        let ks = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' && chars[i] != '/' {
            i += 1;
        }
        let key: String = chars[ks..i].iter().collect::<String>().to_lowercase();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let q = chars[i];
                i += 1;
                let vs = i;
                while i < chars.len() && chars[i] != q {
                    i += 1;
                }
                val = chars[vs..i].iter().collect();
                i += 1;
            } else {
                let vs = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                val = chars[vs..i].iter().collect();
            }
        }
        if !key.is_empty() {
            out.push((key, val));
        }
    }
    out
}

fn html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let a = lower.find("<title")?;
    let a = a + lower[a..].find('>')? + 1;
    let b = a + lower[a..].find("</title>")?;
    let t = decode_entities(html[a..b].trim());
    (!t.is_empty()).then_some(t)
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest[..rest.len().min(10)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let t = "bak: https://youtu.be/abc123, ve (https://en.wikipedia.org/wiki/Rust_(programming_language)).";
        let u: Vec<String> = find_urls(t).into_iter().map(|r| slice_chars(t, r)).collect();
        assert_eq!(u, vec!["https://youtu.be/abc123", "https://en.wikipedia.org/wiki/Rust_(programming_language)"]);
    }

    #[test]
    fn meta() {
        let h = r#"<head><meta property="og:title" content="Başlık &amp; ek"><meta name='description' content=açıklama></head>"#;
        let m = parse_meta(h);
        assert_eq!(m[0], ("og:title".into(), "Başlık & ek".into()));
        assert_eq!(m[1], ("description".into(), "açıklama".into()));
    }
}

#[cfg(test)]
mod live {
    /// Needs the network: `cargo test --bin goodbyedc live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_previews() {
        let http = reqwest::Client::new();
        for url in ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "https://github.com/yenes11/goodbyedc", "https://en.wikipedia.org/wiki/Rust_(programming_language)"] {
            match super::fetch(&http, url).await {
                Ok(p) => println!("{url}\n  site={:?}\n  title={:?}\n  desc={:?}\n  image={:?}", p.site, p.title, p.desc.map(|d| d.chars().take(60).collect::<String>()), p.image.map(|(w, h, _)| (w, h))),
                Err(e) => println!("{url}: HATA {e:#}"),
            }
        }
    }
}
