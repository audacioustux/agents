// Brainstorm Companion: HTTP file server + form-POST events + SSE reloads.
//
// Two client flows replace the old hand-rolled WebSocket:
//
//   1. User clicks a [data-choice] element → browser POSTs JSON to /event
//      via navigator.sendBeacon (survives page reloads, no JS plumbing).
//   2. fs.watch fires when the agent updates a screen file → server pushes
//      `data: reload\n\n` over text/event-stream → browser reloads.
//
// Both are HTTP primitives; no framing, masking, opcodes, or hand-rolled
// protocol. The companion is reachable on any local browser tab and, when
// bound to a non-loopback host, by any host that can route to it. The token
// authenticates the real client uniformly across loopback, tunnel, and
// remote binds — and defeats DNS rebinding — where a Host/Origin allowlist
// cannot. It rides the served URL as ?key= and is mirrored into a cookie on
// first load so same-origin subresources (/files/*) can authenticate.

const crypto = require('crypto');
const http = require('http');
const fs = require('fs');
const path = require('path');

// ========== Configuration ==========

const randomPort = () => 49152 + Math.floor(Math.random() * 16383);
let PORT = process.env.BRAINSTORM_PORT ? Number(process.env.BRAINSTORM_PORT) : randomPort();
const HOST = process.env.BRAINSTORM_HOST || '127.0.0.1';
const URL_HOST = process.env.BRAINSTORM_URL_HOST || (HOST === '127.0.0.1' ? 'localhost' : HOST);
const SESSION_DIR = process.env.BRAINSTORM_DIR || '/tmp/brainstorm';
const CONTENT_DIR = path.join(SESSION_DIR, 'content');
const STATE_DIR = path.join(SESSION_DIR, 'state');
const TOKEN_FILE = process.env.BRAINSTORM_TOKEN_FILE || null;
let ownerPid = process.env.BRAINSTORM_OWNER_PID ? Number(process.env.BRAINSTORM_OWNER_PID) : null;

const IDLE_TIMEOUT_MS = 30 * 60 * 1000;
const MAX_EVENT_BYTES = 64 * 1024; // POST /event body cap (1 JSON user click)

const MIME_TYPES = {
  '.html': 'text/html', '.css': 'text/css', '.js': 'application/javascript',
  '.json': 'application/json', '.png': 'image/png', '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.svg': 'image/svg+xml',
};

// ========== Token ==========

function generateToken() {
  return crypto.randomBytes(32).toString('hex');
}
function chmodOwnerOnly(file) {
  try { fs.chmodSync(file, 0o600); } catch (_) { /* best effort */ }
}
function initialToken() {
  if (process.env.BRAINSTORM_TOKEN) {
    return { value: process.env.BRAINSTORM_TOKEN, source: 'env' };
  }
  if (TOKEN_FILE) {
    try {
      const t = fs.readFileSync(TOKEN_FILE, 'utf-8').trim();
      if (/^[0-9a-f]{32,}$/i.test(t)) {
        chmodOwnerOnly(TOKEN_FILE);
        return { value: t, source: 'file' };
      }
    } catch (_) { /* no prior token recorded */ }
  }
  return { value: generateToken(), source: 'generated' };
}
const tokenInfo = initialToken();
let TOKEN = tokenInfo.value;
let tokenSource = tokenInfo.source;
let COOKIE_NAME = 'brainstorm-key-' + PORT; // refined to the actual bound port in onListen

// ========== Helpers ==========

const WAITING_PAGE = `<!DOCTYPE html><meta charset="utf-8"><title>Brainstorm Companion</title>
<style>body{font-family:system-ui,sans-serif;padding:2rem;max-width:800px;margin:0 auto}h1{color:#333}p{color:#666}</style>
<h1>Brainstorm Companion</h1><p>Waiting for the agent to push a screen...</p>`;
const FORBIDDEN_PAGE = `<!DOCTYPE html><meta charset="utf-8"><title>Session key required</title>
<style>body{font-family:system-ui,sans-serif;padding:2rem;max-width:800px;margin:0 auto}h1{color:#333}p{color:#666}code{background:#f0f0f0;padding:.1em .3em;border-radius:4px}</style>
<h1>Session key required</h1><p>This page needs the full URL your coding agent gave you, including the <code>?key=&hellip;</code> part. Copy the complete URL and open it again.</p>`;

function isFullDocument(html) {
  const t = html.trimStart().toLowerCase();
  return t.startsWith('<!doctype') || t.startsWith('<html');
}
function wrapInFrame(content) {
  return frameTemplate.replace('<!-- CONTENT -->', content);
}
function getNewestScreen() {
  const files = fs.readdirSync(CONTENT_DIR)
    .filter((f) => f.endsWith('.html'))
    .map((f) => ({ path: path.join(CONTENT_DIR, f), mtime: fs.statSync(path.join(CONTENT_DIR, f)).mtime.getTime() }))
    .sort((a, b) => b.mtime - a.mtime);
  return files.length > 0 ? files[0].path : null;
}

// ========== Auth ==========

function timingSafeEqualStr(a, b) {
  const ab = Buffer.from(String(a));
  const bb = Buffer.from(String(b));
  if (ab.length !== bb.length) return false;
  return crypto.timingSafeEqual(ab, bb);
}
function parseCookies(header) {
  const out = {};
  if (!header) return out;
  for (const part of header.split(';')) {
    const eq = part.indexOf('=');
    if (eq < 0) continue;
    out[part.slice(0, eq).trim()] = part.slice(eq + 1).trim();
  }
  return out;
}
function isAuthorized(req) {
  const q = req.url.indexOf('?');
  if (q >= 0) {
    const params = new URLSearchParams(req.url.slice(q + 1));
    const key = params.get('key');
    if (key && timingSafeEqualStr(key, TOKEN)) return true;
  }
  const cookie = parseCookies(req.headers['cookie'])[COOKIE_NAME];
  return Boolean(cookie && timingSafeEqualStr(cookie, TOKEN));
}
function pathnameOf(url) {
  const q = url.indexOf('?');
  return q >= 0 ? url.slice(0, q) : url;
}

// ========== Activity ==========

let lastActivity = Date.now();
function touchActivity() { lastActivity = Date.now(); }

// ========== SSE clients ==========

const sseClients = new Set();

function broadcastReload() {
  for (const res of sseClients) {
    try { res.write('data: reload\n\n'); }
    catch (_) { sseClients.delete(res); }
  }
}

// ========== HTTP handler ==========

const frameTemplate = fs.readFileSync(path.join(__dirname, 'frame-template.html'), 'utf-8');

function renderIndex() {
  const screenFile = getNewestScreen();
  let html = screenFile
    ? (raw => isFullDocument(raw) ? raw : wrapInFrame(raw))(fs.readFileSync(screenFile, 'utf-8'))
    : WAITING_PAGE;
  return html;
}

function handleRequest(req, res) {
  if (!isAuthorized(req)) {
    res.writeHead(403, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(FORBIDDEN_PAGE);
    return;
  }
  touchActivity();

  // Mirror the key into a cookie so same-origin subresources (/files/*) can
  // authenticate after bootstrap. HttpOnly keeps it away from page scripts.
  res.setHeader('Set-Cookie',
    COOKIE_NAME + '=' + TOKEN + '; HttpOnly; SameSite=Strict; Path=/');

  const pathname = pathnameOf(req.url);

  if (req.method === 'GET' && pathname === '/') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(renderIndex());
  } else if (req.method === 'GET' && pathname.startsWith('/files/')) {
    const filePath = path.join(CONTENT_DIR, path.basename(pathname.slice(7)));
    if (!fs.existsSync(filePath)) {
      res.writeHead(404);
      res.end('Not found');
      return;
    }
    const ext = path.extname(filePath).toLowerCase();
    res.writeHead(200, { 'Content-Type': MIME_TYPES[ext] || 'application/octet-stream' });
    res.end(fs.readFileSync(filePath));
  } else if (req.method === 'GET' && pathname === '/events') {
    // Server-Sent Events: server → browser only. Browser reloads on `data: reload`.
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
    });
    res.write('retry: 1000\n\n');
    sseClients.add(res);
    req.on('close', () => sseClients.delete(res));
    // We don't end the response — SSE stays open until the client disconnects.
  } else if (req.method === 'POST' && pathname === '/event') {
    let size = 0;
    const chunks = [];
    let aborted = false;
    req.on('data', (chunk) => {
      if (aborted) return;
      size += chunk.length;
      if (size > MAX_EVENT_BYTES) {
        aborted = true;
        res.writeHead(413, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'event too large', limit: MAX_EVENT_BYTES }));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => {
      if (aborted) return;
      let event;
      try {
        event = JSON.parse(Buffer.concat(chunks).toString('utf-8'));
      } catch (e) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'invalid JSON', detail: e.message }));
        return;
      }
      touchActivity();
      console.log(JSON.stringify({ source: 'user-event', ...event }));
      if (event.choice) {
        fs.appendFileSync(path.join(STATE_DIR, 'events'), JSON.stringify(event) + '\n');
      }
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end('{"ok":true}');
    });
  } else {
    res.writeHead(404);
    res.end('Not found');
  }
}

// ========== Server startup ==========

function startServer() {
  if (!fs.existsSync(CONTENT_DIR)) fs.mkdirSync(CONTENT_DIR, { recursive: true });
  if (!fs.existsSync(STATE_DIR)) fs.mkdirSync(STATE_DIR, { recursive: true });

  // Track known files to distinguish new screens from updates.
  // macOS fs.watch reports 'rename' for both new files and overwrites,
  // so we can't rely on eventType alone.
  const knownFiles = new Set(
    fs.readdirSync(CONTENT_DIR).filter((f) => f.endsWith('.html'))
  );

  const server = http.createServer(handleRequest);

  const debounceTimers = new Map();
  const watcher = fs.watch(CONTENT_DIR, (eventType, filename) => {
    if (!filename || !filename.endsWith('.html')) return;
    if (debounceTimers.has(filename)) clearTimeout(debounceTimers.get(filename));
    debounceTimers.set(filename, setTimeout(() => {
      debounceTimers.delete(filename);
      const filePath = path.join(CONTENT_DIR, filename);
      if (!fs.existsSync(filePath)) return; // file was deleted
      touchActivity();
      if (!knownFiles.has(filename)) {
        knownFiles.add(filename);
        const eventsFile = path.join(STATE_DIR, 'events');
        if (fs.existsSync(eventsFile)) fs.unlinkSync(eventsFile);
        console.log(JSON.stringify({ type: 'screen-added', file: filePath }));
      } else {
        console.log(JSON.stringify({ type: 'screen-updated', file: filePath }));
      }
      broadcastReload();
    }, 100));
  });
  watcher.on('error', (err) => console.error('fs.watch error:', err.message));

  function shutdown(reason) {
    console.log(JSON.stringify({ type: 'server-stopped', reason }));
    const infoFile = path.join(STATE_DIR, 'server-info');
    if (fs.existsSync(infoFile)) fs.unlinkSync(infoFile);
    fs.writeFileSync(
      path.join(STATE_DIR, 'server-stopped'),
      JSON.stringify({ reason, timestamp: Date.now() }) + '\n'
    );
    for (const res of sseClients) { try { res.end(); } catch (_) { /* ignore */ } }
    sseClients.clear();
    watcher.close();
    clearInterval(lifecycleCheck);
    server.close(() => process.exit(0));
  }

  function ownerAlive() {
    if (!ownerPid) return true;
    try { process.kill(ownerPid, 0); return true; } catch (e) { return e.code === 'EPERM'; }
  }

  // Check every 60s: exit if owner process died or idle for 30 minutes
  const lifecycleCheck = setInterval(() => {
    if (!ownerAlive()) shutdown('owner process exited');
    else if (Date.now() - lastActivity > IDLE_TIMEOUT_MS) shutdown('idle timeout');
  }, 60 * 1000);
  lifecycleCheck.unref();

  // Validate owner PID at startup. If it's already dead, the PID resolution
  // was wrong (common on WSL, Tailscale SSH, and cross-user scenarios).
  // Disable monitoring and rely on the idle timeout instead.
  if (ownerPid && !ownerAlive()) {
    console.log(JSON.stringify({ type: 'owner-pid-invalid', pid: ownerPid, reason: 'dead at startup' }));
    ownerPid = null;
  }

  let triedFallback = false;

  function onListen() {
    // Cookie name keys on the ACTUAL bound port (may differ from the preferred
    // one after an EADDRINUSE fallback) so it can't collide with another server's
    // cookie in the shared localhost jar.
    COOKIE_NAME = 'brainstorm-key-' + PORT;
    // Persist the bound token so the next restart of this session reuses it —
    // but ONLY when we got our preferred port. On a fallback we bound a
    // *different* port because someone else holds the preferred one; persisting
    // would overwrite the shared files and strand that other session's open tab.
    if (TOKEN_FILE && !triedFallback) {
      try {
        fs.writeFileSync(TOKEN_FILE, TOKEN, { mode: 0o600 });
      } catch (_) { /* best effort */ }
    }
    const info = JSON.stringify({
      type: 'server-started', port: Number(PORT), host: HOST,
      url_host: URL_HOST, url: 'http://' + URL_HOST + ':' + PORT + '/?key=' + TOKEN,
      screen_dir: CONTENT_DIR, state_dir: STATE_DIR, idle_timeout_ms: IDLE_TIMEOUT_MS,
    });
    console.log(info);
    fs.writeFileSync(path.join(STATE_DIR, 'server-info'), info + '\n', { mode: 0o600 });
  }

  server.on('error', (err) => {
    if (err.code === 'EADDRINUSE' && !triedFallback) {
      if (tokenSource === 'env') {
        console.error('Server failed to bind: preferred port is in use and BRAINSTORM_TOKEN is set; refusing fallback with explicit token');
        process.exit(1);
      }
      triedFallback = true;
      PORT = randomPort();
      if (tokenSource === 'file') {
        TOKEN = generateToken();
        tokenSource = 'generated-fallback';
      }
      server.listen(PORT, HOST, onListen);
    } else {
      console.error('Server failed to bind:', err.message);
      process.exit(1);
    }
  });
  server.listen(PORT, HOST, onListen);
}

if (require.main === module) {
  startServer();
}
