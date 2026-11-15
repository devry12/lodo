// lodo release server (Cloudflare Worker + R2).
//
//   /                    download page
//   /latest.json         latest release manifest (signed, see latest.json.sig)
//   /latest.json.sig
//   /releases/<v>/<f>    release files (immutable, cached for a long time)
//   /download/<os>       redirect to the latest file: mac | windows | linux | deb
//
// This Worker only reads from R2. Files are uploaded by CI / scripts/publish.sh.

const SHORT = "public, max-age=60";
const FOREVER = "public, max-age=31536000, immutable";

const DOWNLOADS = {
  mac: ["darwin-aarch64-dmg", "darwin-x86_64-dmg"],
  windows: ["windows-x86_64-app"],
  linux: ["linux-x86_64-app"],
  deb: ["linux-x86_64-deb"],
};

export default {
  async fetch(request, env) {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("Method not allowed", { status: 405, headers: { allow: "GET, HEAD" } });
    }
    const url = new URL(request.url);
    const path = decodeURIComponent(url.pathname);

    if (path === "/" || path === "/index.html") return landing(env);
    if (path === "/latest.json" || path === "/latest.json.sig") return object(env, request, path.slice(1), SHORT);
    if (path.startsWith("/releases/") && !path.includes("..")) return object(env, request, path.slice(1), FOREVER);

    const dl = path.match(/^\/download\/(mac|windows|linux|deb)\/?$/);
    if (dl) {
      const m = await manifest(env);
      const key = m && DOWNLOADS[dl[1]].find((k) => m.assets[k]);
      if (!key) return new Response("Not available for this platform yet.", { status: 404 });
      return Response.redirect(new URL("/" + m.assets[key].url, url).toString(), 302);
    }
    return new Response("Not found.", { status: 404 });
  },
};

async function manifest(env) {
  const obj = await env.RELEASES.get("latest.json");
  return obj ? obj.json() : null;
}

async function object(env, request, key, cache) {
  const obj = await env.RELEASES.get(key, { range: request.headers, onlyIf: request.headers });
  if (!obj) return new Response("Not found.", { status: 404 });
  const headers = new Headers();
  obj.writeHttpMetadata(headers);
  headers.set("etag", obj.httpEtag);
  headers.set("cache-control", cache);
  headers.set("accept-ranges", "bytes");
  if (key.endsWith(".json")) headers.set("content-type", "application/json; charset=utf-8");
  if (key.endsWith(".sig")) headers.set("content-type", "text/plain; charset=utf-8");
  if (key.startsWith("releases/")) {
    headers.set("content-disposition", `attachment; filename="${key.split("/").pop()}"`);
  }
  // onlyIf precondition failed -> object without a body.
  if (!("body" in obj)) return new Response(null, { status: 304, headers });
  // R2 always sets obj.range; only answer 206 when the client sent a Range header.
  if (request.headers.has("range") && obj.range && "offset" in obj.range) {
    const end = obj.range.offset + obj.range.length - 1;
    headers.set("content-range", `bytes ${obj.range.offset}-${end}/${obj.size}`);
    return new Response(request.method === "HEAD" ? null : obj.body, { status: 206, headers });
  }
  return new Response(request.method === "HEAD" ? null : obj.body, { headers });
}

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function mb(asset) {
  return asset ? `${(asset.size / 1048576).toFixed(1)} MB` : "";
}

async function landing(env) {
  const m = await manifest(env);
  const a = m ? m.assets : {};
  const mac = a["darwin-aarch64-dmg"] || a["darwin-x86_64-dmg"];
  const win = a["windows-x86_64-app"];
  const lin = a["linux-x86_64-app"];
  const deb = a["linux-x86_64-deb"];
  const btn = (os, label, asset, note) =>
    asset
      ? `<a class="dl" data-os="${os}" href="/download/${os}"><span class="os">${label}</span><span class="meta">${esc(note)} · ${mb(asset)}</span></a>`
      : `<span class="dl off"><span class="os">${label}</span><span class="meta">coming soon</span></span>`;

  const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>lodo · one address for your home server</title>
<meta name="description" content="lodo picks the route to your home server automatically: your LAN IP at home, your VPN when you are away.">
<style>
:root{--bg:#f6f6f3;--card:#fff;--fg:#1c1c1e;--muted:#6b6b70;--line:#e3e3df;--accent:#0a84ff;--home:#34c759}
@media (prefers-color-scheme:dark){:root{--bg:#141415;--card:#1d1d1f;--fg:#f2f2f2;--muted:#9a9aa0;--line:#2c2c2f;--accent:#409cff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
main{max-width:720px;margin:0 auto;padding:56px 16px 40px}
h1{font-size:44px;line-height:1;margin:0 0 6px;letter-spacing:-.02em;display:flex;align-items:center;gap:12px}
h1 i{width:18px;height:18px;border-radius:50%;background:var(--home);display:inline-block}
.lead{font-size:20px;color:var(--muted);margin:0 0 32px}
.addr{font:600 15px ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--card);border:1px solid var(--line);border-radius:10px;padding:14px 16px;display:grid;grid-template-columns:auto 1fr;gap:4px 14px;margin:0 0 32px}
.addr span{color:var(--muted);font-weight:400}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:10px;margin:0 0 10px}
.dl{display:flex;flex-direction:column;gap:2px;padding:16px;border-radius:12px;background:var(--card);border:1px solid var(--line);color:inherit;text-decoration:none}
.dl:hover{border-color:var(--accent)}
.dl.mine{border-color:var(--accent);box-shadow:0 0 0 1px var(--accent)}
.dl.off{opacity:.5}
.os{font-weight:600;font-size:17px}
.meta{color:var(--muted);font-size:13px}
.ver{color:var(--muted);font-size:14px;margin:0 0 40px}
ol{padding-left:20px;margin:0 0 32px}
li{margin:0 0 6px}
h2{font-size:18px;margin:0 0 10px}
footer{color:var(--muted);font-size:13px;border-top:1px solid var(--line);padding-top:16px}
a{color:var(--accent)}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.92em}
</style>
</head>
<body>
<main>
<h1><i></i>lodo</h1>
<p class="lead">One address for your home server, at home or on the go.</p>

<div class="addr">git.home.arpa <span>at home → 192.168.1.10</span>
git.home.arpa <span>away → via Netbird / Tailscale</span></div>

<div class="grid">
${btn("mac", "macOS", mac, "Apple Silicon")}
${btn("windows", "Windows", win, "64-bit")}
${btn("linux", "Linux", lin, "AppImage")}
</div>
<p class="ver">${m ? `Version ${esc(m.version)}${m.date ? " · " + esc(m.date) : ""}${deb ? ` · <a href="/download/deb">.deb for Ubuntu/Debian</a> (${mb(deb)})` : ""}` : "No release yet."}</p>

<h2>How it works</h2>
<ol>
<li>Install lodo, open it, click <b>Mulai</b> (Start) and enter your admin password once.</li>
<li>Click <b>Tambah server</b> (Add server): enter a name, the home IP, and the VPN IP. Netbird and Tailscale peers are detected automatically.</li>
<li>Use <code>git.home.arpa</code> everywhere: in git, SSH, or your browser, with HTTPS. lodo picks the route on its own and keeps itself up to date.</li>
</ol>

<footer>Addresses use suffixes reserved for private networks, such as <code>home.arpa</code>, so they never clash with real websites. lodo is <a href="https://github.com/devry12/lodo">open source</a> (MIT).</footer>
</main>
<script>
(function(){var u=navigator.userAgent,os=/Mac/.test(u)?"mac":/Win/.test(u)?"windows":/Linux/.test(u)?"linux":"";
var el=os&&document.querySelector('.dl[data-os="'+os+'"]');if(el)el.classList.add("mine")})();
</script>
</body>
</html>`;
  return new Response(html, { headers: { "content-type": "text/html; charset=utf-8", "cache-control": SHORT } });
}
