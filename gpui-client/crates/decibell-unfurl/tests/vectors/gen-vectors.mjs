// Expected values for decibell-unfurl's tests, produced by running the Electron client's real
// TypeScript: electron/main/{linkPreview,publicNet,gifs}.ts, src/features/servers/inviteLink.ts,
// richText.ts's mediaLabelFor and LinkEmbeds.tsx's isGifUrl. Each module is bundled with the repo's
// esbuild; the `electron` import is a stub (app version "0.0.0-vectors", ipcMain.handle records the
// handlers so the vectors go through the real IPC entry points, shell.openExternal records its URL).
//
//   ELECTRON_RUN_AS_NODE=1 electron-client/node_modules/electron/dist/electron \
//     gpui-client/crates/decibell-unfurl/tests/vectors/gen-vectors.mjs
//
// (Electron's own Node — 20.18 in Electron 33 — so TextDecoder, URL and BlockList are the ones the
// client ships; plain `node` works too.) Writes tests/vectors/*.json and the compressed fixtures.
//
// The pipeline vectors unfurl every case in tests/fixtures/routes.json against a local server that
// serves those routes on 127.0.0.1, plain HTTP and TLS on the same port (an og:image reference is
// upgraded to https, so a probe of an image on an http page reaches the same port over TLS; the
// certificate is a throwaway made with `openssl`, and certificate checks are off for this run). For
// that run only, publicNet is wrapped so 127.0.0.0/8 counts as public: exactly the relaxation the
// Rust tests' test-only constructor applies.
// Origins in results are written as {origin} / {hostport}; lone surrogates (JS strings can hold
// them, Rust strings can't) are written as U+FFFD, which is also what Chromium draws for them.

import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as fs from "node:fs";
import { execFileSync } from "node:child_process";
import * as http from "node:http";
import * as net from "node:net";
import * as os from "node:os";
import * as tls from "node:tls";
import * as path from "node:path";
import * as zlib from "node:zlib";

const here = path.dirname(fileURLToPath(import.meta.url));
const crate = path.resolve(here, "../..");
const fixtures = path.join(crate, "tests/fixtures");
const repo = path.resolve(crate, "../../..");
const mainDir = path.join(repo, "electron-client/electron/main");
const srcDir = path.join(repo, "electron-client/src");
const esbuild = createRequire(path.join(repo, "electron-client/package.json"))("esbuild");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "unfurl-vectors-"));

globalThis.__stub = { handlers: new Map(), opened: [], appPath: path.join(tmp, "no-app") };

const STUBS = {
  electron: `
    export const app = { getVersion: () => "0.0.0-vectors", isPackaged: true, getAppPath: () => globalThis.__stub.appPath };
    export const ipcMain = { handle: (ch, fn) => globalThis.__stub.handlers.set(ch, fn) };
    export const shell = { openExternal: async (u) => { globalThis.__stub.opened.push(u); } };
    export const net = {};`,
  chatStore: `export const useChatStore = { getState: () => ({ setPendingInvite() {} }) };`,
  // Hermetic: no name resolves (fixture names like cdn.example.net fail as NXDOMAIN, as with the
  // Rust tests' scripted resolver), so this run never leaves the machine.
  publicNet: `
    import * as real from ${JSON.stringify(path.join(mainDir, "publicNet.ts"))};
    const dns = { lookup: (hostname, _o, cb) => { const e = new Error("getaddrinfo ENOTFOUND " + hostname); e.code = "ENOTFOUND"; process.nextTick(cb, e); } };
    export function isPublicAddress(ip) { return /^127\\.\\d+\\.\\d+\\.\\d+$/.test(ip) || real.isPublicAddress(ip); }
    export const publicOnlyLookup = (hostname, options, callback) => {
      dns.lookup(hostname, { ...options, all: true }, (err, addresses) => {
        if (err) return callback(err, "");
        if (addresses.length === 0 || addresses.some((a) => !isPublicAddress(a.address))) {
          const e = new Error(hostname + " resolves to a non-public address"); e.code = "ENOTPUBLIC"; return callback(e, "");
        }
        if (options.all) callback(null, addresses); else callback(null, addresses[0].address, addresses[0].family);
      });
    };`,
};

let bundleSeq = 0;
async function load(entry, exports, { relaxLoopback = false, instance = "" } = {}) {
  const source = fs.readFileSync(entry, "utf8") + `\nexport { ${exports.join(", ")} };\n`;
  const out = await esbuild.build({
    stdin: { contents: source, resolveDir: path.dirname(entry), sourcefile: path.basename(entry), loader: "ts" },
    bundle: true,
    platform: "node",
    format: "esm",
    target: "node20",
    write: false,
    logLevel: "error",
    plugins: [
      {
        name: "stubs",
        setup(b) {
          b.onResolve({ filter: /^electron$/ }, () => ({ path: "electron", namespace: "stub" }));
          b.onResolve({ filter: /stores\/chatStore$/ }, () => ({ path: "chatStore", namespace: "stub" }));
          if (relaxLoopback) b.onResolve({ filter: /^\.\/publicNet$/ }, () => ({ path: "publicNet", namespace: "stub" }));
          b.onLoad({ filter: /.*/, namespace: "stub" }, (a) => ({ contents: STUBS[a.path], loader: "js", resolveDir: mainDir }));
        },
      },
    ],
  });
  const file = path.join(tmp, `bundle-${++bundleSeq}.mjs`);
  fs.writeFileSync(file, out.outputFiles[0].text);
  return import(pathToFileURL(file).href + (instance ? `?${instance}` : ""));
}

// JSON with lone surrogates made well-formed, and the test server's origin templated.
function write(name, value, origin) {
  let text = JSON.stringify(value, (_k, v) => (typeof v === "string" ? v.toWellFormed() : v), 1);
  if (origin) {
    const hostport = origin.replace(/^http:\/\//, "");
    text = text.split(origin).join("{origin}").split(hostport).join("{hostport}");
  }
  fs.writeFileSync(path.join(here, name), text + "\n");
  console.log(`wrote ${name}`);
}

// ── Compressed fixtures (committed; regenerated only when missing) ─────────────────────────────
function ensureCompressed() {
  const html = path.join(fixtures, "html/og-full.html");
  const raw = fs.readFileSync(html);
  const make = (suffix, fn) => {
    const p = html + suffix;
    if (!fs.existsSync(p)) fs.writeFileSync(p, fn());
  };
  make(".gz", () => zlib.gzipSync(raw));
  make(".br", () => zlib.brotliCompressSync(raw));
  make(".zz", () => zlib.deflateSync(raw));
  make(".bad-crc.gz", () => {
    const gz = Buffer.from(zlib.gzipSync(raw));
    gz[gz.length - 6] ^= 0xff;
    return gz;
  });
  make(".truncated.gz", () => {
    const gz = zlib.gzipSync(raw);
    return gz.subarray(0, gz.length - 4);
  });
}

// ── The fixture server ───────────────────────────────────────────────────────────────────────
const manifest = JSON.parse(fs.readFileSync(path.join(fixtures, "routes.json"), "utf8"));

function bodyOf(spec) {
  if (spec.file) return fs.readFileSync(path.join(fixtures, spec.file));
  if (spec.text !== undefined) return Buffer.from(spec.text, "utf8");
  return Buffer.concat(
    spec.parts.map((p) =>
      p.hex !== undefined
        ? Buffer.from(p.hex, "hex")
        : p.repeatHex !== undefined
          ? Buffer.alloc(p.count, Buffer.from(p.repeatHex, "hex"))
          : p.repeat !== undefined
            ? Buffer.from(p.repeat.repeat(p.count), "utf8")
            : Buffer.from(p.text, "utf8"),
    ),
  );
}

function subst(s, origin) {
  const hostport = origin.replace(/^http:\/\//, "");
  return s.split("{origin}").join(origin).split("{hostport}").join(hostport).split("{port}").join(hostport.split(":")[1]);
}

function startServer() {
  execFileSync("openssl", [
    "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-days", "2", "-subj", "/CN=127.0.0.1",
    "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", path.join(tmp, "key.pem"), "-out", path.join(tmp, "cert.pem"),
  ], { stdio: "ignore" });
  process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";
  return new Promise((resolve) => {
    let origin = "";
    const app = http.createServer((req, res) => {
      const route = manifest.routes[req.url.split("?")[0]];
      if (!route) {
        res.writeHead(404, ["content-type", "text/plain"]);
        res.end("not found");
        return;
      }
      res.writeHead(route.status, route.headers.flatMap(([k, v]) => [k, subst(v, origin)]));
      res.end(route.status === 204 || route.status === 304 ? undefined : bodyOf(route.body));
    });
    const tlsServer = tls.createServer({ key: fs.readFileSync(path.join(tmp, "key.pem")), cert: fs.readFileSync(path.join(tmp, "cert.pem")) });
    tlsServer.on("secureConnection", (s) => app.emit("connection", s));
    tlsServer.on("tlsClientError", () => {});
    // One port, both protocols: a TLS ClientHello starts with 0x16.
    const server = net.createServer((socket) => {
      socket.once("readable", () => {
        const first = socket.read(1);
        if (!first) return socket.destroy();
        socket.unshift(first);
        (first[0] === 0x16 ? tlsServer : app).emit("connection", socket);
      });
    });
    server.listen(0, "127.0.0.1", () => {
      origin = `http://127.0.0.1:${server.address().port}`;
      resolve({ server, origin });
    });
  });
}

// ── Guard ────────────────────────────────────────────────────────────────────────────────────
async function guardVectors(lp) {
  const pn = await import(pathToFileURL(path.join(tmp, "publicNet.mjs")).href);
  const ips = [
    "127.0.0.1", "127.255.255.255", "0.0.0.0", "0.255.255.255", "1.0.0.0", "9.255.255.255", "10.0.0.0", "10.255.255.255",
    "11.0.0.0", "100.63.255.255", "100.64.0.0", "100.127.255.255", "100.128.0.0", "169.254.0.0", "169.254.169.254",
    "169.255.0.0", "172.15.255.255", "172.16.0.0", "172.31.255.255", "172.32.0.0", "192.0.0.0", "192.0.0.255", "192.0.1.0",
    "192.0.2.0", "192.0.2.255", "192.0.3.0", "192.88.99.0", "192.88.99.255", "192.88.100.0", "192.167.255.255",
    "192.168.0.0", "192.168.255.255", "192.169.0.0", "198.17.255.255", "198.18.0.0", "198.19.255.255", "198.20.0.0",
    "198.51.100.1", "198.51.101.0", "203.0.113.1", "203.0.114.0", "223.255.255.255", "224.0.0.0", "239.255.255.255",
    "240.0.0.0", "255.255.255.255", "8.8.8.8", "93.184.216.34",
    "::", "::1", "::2", "::7f00:1", "::127.0.0.1", "::8.8.8.8", "::ffff:127.0.0.1", "::ffff:7f00:1", "0:0:0:0:0:ffff:7f00:1",
    "::ffff:a9fe:a9fe", "::ffff:10.0.0.1", "::ffff:8.8.8.8", "::ffff:100.64.0.1", "::ffff:0:127.0.0.1", "::ffff:0:8.8.8.8",
    "::fffe:7f00:1", "::1:0:0:1", "64:ff9b::8.8.8.8", "64:ff9b::1:0", "64:ff9b:0:0:0:1::1", "64:ff9b:1::1", "64:ff9b:1:ffff::1",
    "64:ff9b:2::1", "100::1", "100::ffff:ffff:ffff:ffff", "100:0:0:1::", "2001::1", "2001:0:ffff::1", "2001:1::1",
    "2001:db8::1", "2001:db8:ffff::1", "2001:db9::1", "2002::1", "2002:7f00:1::1", "2002:ffff::1", "2003::1", "fbff::1",
    "fc00::1", "fdff:ffff::1", "fe00::1", "fe7f::1", "fe80::1", "febf::1", "fec0::1", "feff::1", "ff00::1", "ff02::1",
    "ffff::1", "2606:4700:4700::1111", "2a00:1450:4001:80b::200e",
    "fe80::1%eth0", "127.1", "0x7f000001", "01.2.3.4", "1.2.3.4.5", "", "garbage", "::ffff:1.2.3.4%1",
  ];
  const isPublic = ips.map((ip) => ({ ip, public: pn.isPublicAddress(ip) }));
  const urls = [
    "https://example.com/", "http://example.com./x", "HTTP://EXAMPLE.COM:80/", "https://example.com:443/", "ftp://example.com/",
    "file:///etc/passwd", "data:text/html,hi", "javascript:alert(1)", "blob:https://example.com/x", "ws://example.com/",
    "https://user@example.com/", "https://:pw@example.com/", "https://user:pw@example.com/", "https://@example.com/",
    "http://localhost/", "http://LOCALHOST./", "http://localhost../", "http://a.b.localhost/", "http://printer.local/",
    "http://printer.local./", "http://local/", "http://metadata.google.internal/", "http://box.localdomain/",
    "http://nas.home.arpa/", "http://home.arpa/", "http://xhome.arpa/", "http://ｌｏｃａｌｈｏｓｔ/", "http://localhost%2e/",
    "http://loc%61lhost/", "http://notlocalhost/", "http://localhost.example.com/", "http://xn--localhost/",
    "http://127.0.0.1/", "http://127.1/", "http://127.0.1/", "http://0x7f.1/", "http://0x7f000001/", "http://0X7F.0.0.1/",
    "http://2130706433/", "http://017700000001/", "http://0177.0.0.1/", "http://0177.0.0.01/", "http://127.0.0.1./",
    "http://127.0.0.1../", "http://１２７.０.０.１/", "http://0/", "http://0.0.0.0/", "http://00000/", "http://4294967295/",
    "http://4294967296/", "http://1.2.3.4.5/", "http://0x100000000/", "http://169.254.169.254/latest/meta-data/",
    "http://[::1]/", "http://[::]/", "http://[0::1]/", "http://[::ffff:127.0.0.1]/", "http://[::ffff:7f00:1]/",
    "http://[0:0:0:0:0:ffff:127.0.0.1]/", "http://[::7f00:1]/", "http://[::127.0.0.1]/", "http://[::ffff:a9fe:a9fe]/",
    "http://[64:ff9b::7f00:1]/", "http://[2002:7f00:1::]/", "http://[2001::1]/", "http://[fe80::1]/",
    "http://[fe80::1%25eth0]/", "http://[fd00::1]/", "http://[fec0::1]/", "http://[ff02::1]/", "http://[::ffff:8.8.8.8]/",
    "http://[2606:4700:4700::1111]/", "http://[::1/", "http://8.8.8.8:22/", "https://1.1.1.1:65535/x",
    "http://8.8.8.8:65536/", "http://./", "http://../", "http:///x", "http://%30/", "http://example.com%00/",
    "http://exa mple.com/", "http://example.com\\@127.0.0.1/", "http://127.0.0.1\\@example.com/",
    "http://example.com#@127.0.0.1/", "http://example.com?@127.0.0.1/", "  http://127.0.0.1/  ", "\thttp://example.com/\n",
    "http://ex\tample.com/", "http://xn--nxasmq6b.com/", "http://ελληνικά.com/", "http://EXAMPLE。COM/",
  ];
  const assert = urls.map((url) => {
    let u;
    try {
      u = new URL(url);
    } catch (e) {
      return { url, href: null, hostname: null, result: e.message };
    }
    let result = "ok";
    try {
      lp.assertAllowed(u);
    } catch (e) {
      result = e.message;
    }
    return { url, href: u.href, hostname: u.hostname, result };
  });
  const joins = [
    ["/a/b/c", "../d"], ["/a/b/c", "//other.example/x"], ["/a/b/c", "https://elsewhere/"], ["/a/b/c", "?q=1"],
    ["/a/b/c", "#f"], ["/a/b/c", " /trim "], ["/a/b/c", "http://[bad"], ["/a/b/c", "\\\\back\\slash"],
    ["/a/b/c", "/cafÃ©"], ["/a/b/c", "x y|z^{}`\"<>"], ["/a/b/c", "ftp://h/x"], ["/a/b/c", "HTTPS://UP.example/"],
    ["/a/b/c", "//127.0.0.1:1/"], ["/a/b/c", "http:relative"], ["/a/b/c", "https:/one-slash"], ["/a/b/c", ""],
  ].map(([basePath, input]) => {
    const base = "https://example.com" + basePath;
    try {
      return { base, input, href: new URL(input, base).href };
    } catch (e) {
      return { base, input, href: null };
    }
  });
  return { isPublic, assert, joins };
}

// ── Extraction ───────────────────────────────────────────────────────────────────────────────
function imagesFor(lp) {
  const out = [];
  const add = (name, bytes) => out.push({ name, hex: Buffer.from(bytes).toString("hex"), dims: lp.probeImageDims(new Uint8Array(bytes)) });
  for (const f of fs.readdirSync(path.join(fixtures, "images")).sort()) {
    const b = fs.readFileSync(path.join(fixtures, "images", f));
    add(f, b);
    for (const n of [0, 3, 9, 10, 23, 24, 29, 30]) if (n < b.length) add(`${f}[:${n}]`, b.subarray(0, n));
  }
  const big = bodyOf(manifest.routes["/images/big-exif.jpg"].body);
  out.push({ name: "big-exif.jpg[:65536]", generated: "big-exif:65536", dims: lp.probeImageDims(new Uint8Array(big.subarray(0, 65536))) });
  out.push({ name: "big-exif.jpg", generated: "big-exif", dims: lp.probeImageDims(new Uint8Array(big)) });
  // JPEG oddities: fill bytes, standalone markers, a 0xff run, a SOF2 (progressive), a DHT before SOF.
  add("jpeg-fill", Buffer.from("ffd8ffffffe000040000ff01ffd0ffc2000b08012c019003012200aabbccdd", "hex"));
  add("jpeg-dht-sof3", Buffer.from("ffd8ffc40004000000ffc3000b0800640080030122000000000000", "hex"));
  add("jpeg-sos-first", Buffer.from("ffd8ffda0004000000ffc0000b08001000200301220000000000", "hex"));
  add("jpeg-garbage", Buffer.from("ffd800112233ffc0000b0800100020030122000000000000", "hex"));
  add("webp-unknown-chunk", Buffer.concat([Buffer.from("RIFF\0\0\0\0WEBPALPH", "latin1"), Buffer.alloc(20)]));
  add("png-huge", Buffer.from("89504e470d0a1a0a0000000d49484452ffffffff80000000", "hex"));
  add("gif87", Buffer.from("474946383761ffff0100", "hex"));
  return out;
}

function extractVectors(lp) {
  const ctOf = (file) => {
    const r = Object.values(manifest.routes).find((r) => r.body.file === `html/${file}`);
    return r ? (r.headers.find(([k]) => k === "content-type")?.[1] ?? "").toLowerCase() : "text/html";
  };
  const pages = [];
  for (const f of fs.readdirSync(path.join(fixtures, "html")).sort()) {
    if (!f.endsWith(".html")) continue;
    const bytes = new Uint8Array(fs.readFileSync(path.join(fixtures, "html", f)));
    for (const ct of [ctOf(f), "text/html"]) {
      const decoded = lp.decodeHtml(bytes, ct);
      const p = lp.parsePage(decoded);
      pages.push({ file: f, contentType: ct, decoded, page: { meta: Object.fromEntries(p.meta), title: p.title, oembed: p.oembed } });
      if (ct === "text/html") break;
    }
  }
  const entities = [
    "plain", "a &amp; b", "&AMP;&Amp;&aMp;", "&#39;&#x27;&#X27;&#039;", "&#0;&#x0;&#-1;", "&#x10FFFF;&#x110000;&#1114112;",
    "&#xD800;&#xDFFF;&#55296;", "&#99999999999999999999999;&#xFFFFFFFFFFFFFFFFFFFF;", "&#x;&#;&;&a;&1a;", "&amp &lt;",
    "&nbsp;&ndash;&mdash;&hellip;&copy;&reg;&trade;&laquo;&raquo;&lsquo;&rsquo;&ldquo;&rdquo;&bull;&middot;&euro;&pound;&yen;&deg;&times;",
    "&eacute;&apos;&quot;&gt;&lt;", "&amp;amp;", "&#128512;&#x1f600;", "&#65&#66;", "&Kmp;", "&amſ;",
  ];
  const clean = [
    ["  a \n\t b ", 10], ["abcdefghijk", 5], ["abc defghijk", 5], ["ab😀cd", 4], ["ab😀cd", 3], ["　﻿ x  ", 10],
    ["\u0085x\u0085", 10], ["x".repeat(201), 200], ["word ".repeat(80), 350], ["a b c", 10], ["", 5],
  ];
  const attrs = [
    ` property="og:title" CONTENT='x "y"' data-x=1 bare name=a/b`, `a=1 a=2 B=3 b=4`, `x = "spaced"  y= 'z' `,
    `"quoted-name"=1 ok=2`, `=lone`, `k=v=w`, `k="unterminated`, "multi\nline=\"v\nw\"", `ä=1 ÉCOLE=2`, `k=\`tick\``, `/ self`,
  ];
  const toInt = ["1200", " 630px", "+5", "-5", "0", "99999", "100000", "1e3", "0x10", "", "abc", "﻿ 7", "\u00857", "12.9"];
  const resolve = [
    ["/a.png", "https://example.com/dir/page"], ["a.png", "https://example.com/dir/page"], ["../a.png", "https://example.com/d1/d2/p"],
    ["//cdn.example.com/a.png", "http://example.com/"], ["http://cdn.example.com/a.png", "https://example.com/"],
    ["http://cdn.example.com:80/a.png", "https://example.com/"], ["http://cdn.example.com:443/a.png", "https://example.com/"],
    ["http://cdn.example.com:8080/a.png", "https://example.com/"], ["ftp://cdn.example.com/a.png", "https://example.com/"],
    ["data:image/png;base64,AAAA", "https://example.com/"], ["javascript:alert(1)", "https://example.com/"],
    ["https://user:pw@cdn.example.com/a.png", "https://example.com/"], ["https://user@cdn.example.com/a.png", "https://example.com/"],
    ["  https://cdn.example.com/a b.png  ", "https://example.com/"], [" https://cdn.example.com/x.png", "https://example.com/"],
    ["https://cdn.example.com/" + "a".repeat(2030), "https://example.com/"], ["https://cdn.example.com/" + "a".repeat(2040), "https://example.com/"],
    ["http://[bad", "https://example.com/"], ["", "https://example.com/"], ["https://ÉXAMPLE.com/ü.png", "https://example.com/"],
    ["HTTP://CDN.EXAMPLE.COM/A.PNG", "https://example.com/"], ["/a.png", "http://127.0.0.1:8080/x"],
  ].map(([raw, base]) => ({ raw, base, out: lp.resolveImage(raw, base) }));
  const colors = ["#abc", "#ABCD", "#a1b2c3", "#A1B2C3D4", " #abc ", "#abcde", "#ggg", "abc", "#", "red", "", "#١٢٣"];
  return {
    pages,
    entities: entities.map((s) => ({ in: s, out: lp.decodeEntities(s) })),
    clean: clean.map(([s, max]) => ({ in: s, max, out: lp.clean(s, max) })),
    attrs: attrs.map((s) => ({ in: s, out: Object.fromEntries(lp.parseAttrs(s)) })),
    toInt: toInt.map((s) => ({ in: s, out: lp.toInt(s) })),
    resolveImage: resolve,
    validColor: colors.map((c) => ({ in: c, out: lp.validColor(c) })),
    images: imagesFor(lp),
  };
}

// ── Pipeline (real unfurl through the IPC handler, against the fixture server) ───────────────
async function pipelineVectors(origin) {
  globalThis.__stub.handlers.clear();
  const lp = await load(path.join(mainDir, "linkPreview.ts"), [], { relaxLoopback: true });
  lp.registerLinkPreviewHandlers();
  const fetchPreview = globalThis.__stub.handlers.get("decibell:linkPreview:fetch");
  const openExternal = globalThis.__stub.handlers.get("decibell:shell:openExternal");
  const cases = [];
  for (const template of manifest.unfurl) {
    const url = subst(template, origin);
    cases.push({ url: template, preview: await fetchPreview(null, url) });
  }
  const external = [];
  for (const url of [
    "https://Example.com/a b", "http://example.com", "javascript:alert(1)", "file:///etc/passwd", "decibell://invite/ABCD",
    "mailto:a@b.c", "https://x.y/" + "a".repeat(2040), "https://x.y/" + "a".repeat(2030), "not a url", "  https://trim.me/  ",
    "HTTPS://UPPER.EXAMPLE/Path", "https://user:pw@example.com/",
  ]) {
    globalThis.__stub.opened.length = 0;
    await openExternal(null, url);
    external.push({ url, opened: globalThis.__stub.opened[0] ?? null });
  }
  return { cases, external };
}

// ── GIFs ─────────────────────────────────────────────────────────────────────────────────────
let gifInstance = 0;
async function gifModule() {
  globalThis.__stub.handlers.clear();
  const g = await load(path.join(mainDir, "gifs.ts"), ["localeParts", "cursorInt"], { instance: `i${++gifInstance}` });
  g.registerGifHandlers();
  return {
    g,
    status: () => globalThis.__stub.handlers.get("decibell:gifs:status")(null),
    search: (...a) => globalThis.__stub.handlers.get("decibell:gifs:search")(null, ...a),
  };
}

async function gifVectors() {
  const requests = [];
  let reply = { status: 200, body: "{}" };
  globalThis.fetch = async (u, init) => {
    requests.push({ url: String(u), headers: init?.headers ?? null, timeout: !!init?.signal });
    return new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } });
  };
  const fixture = (f) => fs.readFileSync(path.join(fixtures, "gifs", f), "utf8");

  // Searches: the request each builds and how each answer parses.
  const searches = [];
  for (const provider of ["klipy", "giphy"]) {
    const key = provider === "klipy" ? "test key/é+1" : "giphy key&=?";
    process.env.GIF_API_KEY = key;
    process.env.GIF_API_PROVIDER = provider;
    const m = await gifModule();
    const answers = fs.readdirSync(path.join(fixtures, "gifs")).filter((f) => f.startsWith(provider)).sort();
    const queries = [
      ["", null, "en-US", false], ["cat", null, "en-US", false], ["  cats  and dogs ", "2", "de_DE", true],
      ["x".repeat(250), "48", "zh-Hans-CN", false], ["😀".repeat(120), "abc", "fil-PH", false], ["q&a=1+2 / ü", "-3", "e", false],
      ["dog", "1e3", null, true], ["dog", "999999999999999999999999", "en", false], ["dog", "0", "EN-us", false],
      ["dog", "", "", false], ["　trim﻿", "7.9", "eng-GBR", false],
    ];
    for (const [q, pos, locale, unfiltered] of queries) {
      reply = { status: 200, body: fixture(answers.find((f) => f.includes("search"))) };
      requests.length = 0;
      const result = await m.search(q, pos, locale, unfiltered);
      searches.push({ provider, key, query: q, pos, locale, unfiltered, request: requests[0], answer: answers.find((f) => f.includes("search")), status: 200, result });
    }
    for (const answer of answers) {
      for (const status of [200, 401, 502]) {
        reply = { status, body: fixture(answer) };
        requests.length = 0;
        const result = await m.search("cat", "24", "en-US", false);
        searches.push({ provider, key, query: "cat", pos: "24", locale: "en-US", unfiltered: false, request: requests[0], answer, status, result });
      }
    }
    for (const [status, body] of [[502, "<html>Bad gateway</html>"], [200, ""], [200, "null"], [200, "[1,2]"], [429, '{"meta":{"msg":"Too many"}}']]) {
      reply = { status, body };
      requests.length = 0;
      const result = await m.search("", null, null, false);
      searches.push({ provider, key, query: "", pos: null, locale: null, unfiltered: false, request: requests[0], answerText: body, status, result });
    }
  }

  // Configuration: environment first, then gifs.json.
  const configs = [];
  const appDir = path.join(tmp, "app");
  fs.mkdirSync(path.join(appDir, "resources"), { recursive: true });
  const fileCases = [
    ["env", " k1 ", " GIPHY ", null], ["env", "k2", undefined, null], ["env", "k3", "tenor", null], ["env", "   ", "giphy", '{"provider":"giphy","key":"file-key"}'],
    ["file", undefined, undefined, '﻿{"provider":"KLIPY","key":"  abc  "}'], ["file", undefined, undefined, '{"provider":"giphy","key":"g"}'],
    ["file", undefined, undefined, '{"provider":"giphy","key":""}'], ["file", undefined, undefined, '{"provider":"tenor","key":"t"}'],
    ["file", undefined, undefined, '{"provider":" klipy","key":"k"}'], ["file", undefined, undefined, "null"],
    ["file", undefined, undefined, "[]"], ["file", undefined, undefined, "not json"], ["file", undefined, undefined, '{"provider":"klipy","key":5}'],
    ["file", undefined, undefined, null],
  ];
  for (const [kind, envKey, envProvider, file] of fileCases) {
    if (envKey === undefined) delete process.env.GIF_API_KEY;
    else process.env.GIF_API_KEY = envKey;
    if (envProvider === undefined) delete process.env.GIF_API_PROVIDER;
    else process.env.GIF_API_PROVIDER = envProvider;
    const gifsJson = path.join(appDir, "resources", "gifs.json");
    if (file === null) fs.rmSync(gifsJson, { force: true });
    else fs.writeFileSync(gifsJson, file);
    globalThis.__stub.appPath = appDir;
    const m = await gifModule();
    const status = await m.status();
    reply = { status: 200, body: '{"result":true,"data":{"data":[]},"meta":{"status":200},"data2":[]}' };
    requests.length = 0;
    const result = await m.search("", null, null, false);
    let key = null;
    if (requests[0]) {
      const u = new URL(requests[0].url);
      key = status.provider === "klipy" ? decodeURIComponent(u.pathname.split("/")[3]) : u.searchParams.get("api_key");
    }
    configs.push({ kind, envKey: envKey ?? null, envProvider: envProvider ?? null, file, status, key, notConfigured: result.ok === false && result.error === "not-configured" });
  }
  delete process.env.GIF_API_KEY;
  delete process.env.GIF_API_PROVIDER;

  const { g } = await gifModule();
  const locales = ["en-US", "en", "e", "eng-GBR", "en_gb", "zh-Hans-CN", "es-419", "fil-PH", "EN-us", "", "1en", "en-", "en-U", "abcd-ef"];
  const cursors = [null, "", "0", "-0", "7", "7.9", "1e3", "abc", "-3", " 12", "999999999999999999999999", "9".repeat(400)];
  return {
    searches,
    configs,
    localeParts: locales.map((l) => ({ in: l, out: g.localeParts(l) })),
    cursorInt: cursors.map((c) => ({ in: c, out: g.cursorInt(c, 5) })),
  };
}

// ── Links ────────────────────────────────────────────────────────────────────────────────────
async function linkVectors() {
  const inv = await load(path.join(srcDir, "features/servers/inviteLink.ts"), []);
  const rt = await load(path.join(srcDir, "features/chat/richText.ts"), []);
  const embedsSrc = fs.readFileSync(path.join(srcDir, "features/chat/LinkEmbeds.tsx"), "utf8");
  const fnSrc = /function isGifUrl\(url: string\): boolean \{[\s\S]*?\n\}/.exec(embedsSrc)[0];
  const js = (await esbuild.transform(fnSrc + "\nreturn isGifUrl;", { loader: "ts" })).code;
  const isGifUrl = new Function(js)();
  const invites = [
    "decibell://invite/ABCDEFGHIJ", "DECIBELL://Invite/ABCDEFGHIJ/", "decibell://invite/abcd", "decibell://invite/abc",
    "decibell://invite/" + "a".repeat(64), "decibell://invite/" + "a".repeat(65), "decibell://invite/example.com:8082/abcd1234",
    "decibell://invite/[::1]:8082/abcd1234", "decibell://invite/[2001:DB8::1]:1/abcd1234", "decibell://invite/[]:1/abcd1234",
    "decibell://invite/10.0.0.2/8082/abcd1234", "decibell://invite/host:0/abcd1234", "decibell://invite/host:65535/abcd1234",
    "decibell://invite/host:65536/abcd1234", "decibell://invite/host:123456/abcd1234", "decibell://invite/1234/5678",
    "decibell://invite/ABCD EFGH", "decibell://invite/ABCD\nEFGH", "decibell://invite/ABCD-EFGH", "https://invite/ABCDEFGHIJ",
    "decibell://invite/ABCDEFGHIJ\n", " decibell://invite/ABCDEFGHIJ", "decibell://invite/höst:80/abcd", "decibell://invite/h　st:80/abcd",
    "decibell://invite/a:b:80/abcd", "decibell://invite/host:080/abcd1234", "decibell://invite//80/abcd", "decibell://invite/ABCD//",
  ].map((url) => ({ url, parsed: inv.parseInviteLink(url) ?? null, isInvite: inv.isInviteLink(url) }));
  const media = [
    "https://static.klipy.com/x/abc.GIF", "https://media.giphy.com/media/x/giphy.gif?cid=1", "https://x.y/a.jpeg#f", "https://x.y/a.JPG",
    "https://x.y/a.png", "https://x.y/a.webp", "https://x.y/a.avif", "https://x.y/a.svg", "https://x.y/a.gif.png", "https://x.y/a.png/",
    "https://x.y/%2Egif", "https://x.y/a%2Egif", "https://x.y/a.gif%20", "not a url", "decibell://invite/x.gif", "https://x.y/.gif",
  ].map((href) => ({ href, label: rt.mediaLabelFor(href), isGif: isGifUrl(href) }));
  return { invites, media, build: inv.buildInviteLink("XYZ123ABCD") };
}

// ── Main ─────────────────────────────────────────────────────────────────────────────────────
ensureCompressed();
// publicNet standalone (the real one) for isPublicAddress.
fs.writeFileSync(
  path.join(tmp, "publicNet.mjs"),
  (await esbuild.build({ entryPoints: [path.join(mainDir, "publicNet.ts")], bundle: true, platform: "node", format: "esm", write: false })).outputFiles[0].text,
);
const lpInternals = await load(path.join(mainDir, "linkPreview.ts"), [
  "assertAllowed", "decodeHtml", "decodeEntities", "clean", "parseAttrs", "parsePage", "toInt", "resolveImage", "validColor", "probeImageDims",
]);
write("guard.json", await guardVectors(lpInternals));
write("extract.json", extractVectors(lpInternals));
const { server, origin } = await startServer();
try {
  write("pipeline.json", await pipelineVectors(origin), origin);
} finally {
  server.close();
  delete process.env.NODE_TLS_REJECT_UNAUTHORIZED;
}
write("gifs.json", await gifVectors());
write("links.json", await linkVectors());
fs.rmSync(tmp, { recursive: true, force: true });
console.log(`node ${process.versions.node}${process.versions.electron ? `, electron ${process.versions.electron}` : ""}`);
process.exit(0);
