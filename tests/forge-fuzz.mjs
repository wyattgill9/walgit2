// Forge fuzz (D50): users doing everything people do with a repository, from an empty one,
// against a running walgit, checked against a model of what every issue and pull request must be.
//
//   NODE_EXTRA_CA_CERTS=ca.pem WALGIT_CA=ca.pem node tests/forge-fuzz.mjs
//
// Env: WALGIT_URL (https://walgit.localhost:8080), REPO (test/forge-demo, must not exist),
// SEED (1), STEPS (300), WALGIT_CA (CA file for git; node reads NODE_EXTRA_CA_CERTS).
// MODE=snapshot OUT=file writes every item (detail + timeline) as JSON, for comparing a cold
// restart. The server needs token auth with these users (docs/FORGE.md "Fuzz"):
//   alice-token, bob-token (write), carol-token (read only), maint-token (write, admin).
//
// Every step runs one action and checks its answer; every 25 steps and at the end, every item
// on the server must equal the model. At the end the git side is checked too: a fresh clone
// passes `git fsck`, every merge commit is on main, deleted branches are gone.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const BASE = process.env.WALGIT_URL ?? "https://walgit.localhost:8080";
const REPO = process.env.REPO ?? "test/forge-demo";
const SEED = Number(process.env.SEED ?? 1);
const STEPS = Number(process.env.STEPS ?? 300);
const CA = process.env.WALGIT_CA ?? "";
const API = `${BASE}/${REPO}/api`;
const REMOTE = `${BASE}/${REPO}.git`;
const USERS = {
  alice: { token: "alice-token", write: true },
  bob: { token: "bob-token", write: true },
  carol: { token: "carol-token", write: false },
  maint: { token: "maint-token", write: true },
};
const DEVS = ["alice", "bob"];
const EVERYONE = ["alice", "bob", "carol", "maint"];
const LABELS = { bug: "#e06c75", enhancement: "#61afef", docs: "#98c379", question: "#c678dd", "good first issue": "#e5c07b" };
const FILES = ["src/lib.rs", "src/main.rs", "docs/guide.md"];

// ---- seeded randomness ----
let s = SEED >>> 0 || 1;
const rand = () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
const pick = (a) => a[Math.floor(rand() * a.length)];
const chance = (p) => rand() < p;
const int = (lo, hi) => lo + Math.floor(rand() * (hi - lo + 1));

// ---- transport ----
async function api(user, method, path, body) {
  const headers = { Accept: "application/json" };
  if (user) headers.Authorization = `Bearer ${USERS[user].token}`;
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const r = await fetch(`${API}${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    json = text;
  }
  return { status: r.status, json };
}
const work = mkdtempSync(join(tmpdir(), "forge-fuzz-"));
function git(user, cwd, ...args) {
  const auth = user ? ["-c", `http.extraHeader=Authorization: Bearer ${USERS[user].token}`] : [];
  const ca = CA ? ["-c", `http.sslCAInfo=${CA}`] : [];
  const who = user ? ["-c", `user.name=${user}`, "-c", `user.email=${user}@example.com`] : [];
  return execFileSync("git", [...ca, ...auth, ...who, ...args], { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}
const dir = (user) => join(work, user);

// ---- assertions ----
const log = [];
let step = 0;
function fail(msg) {
  console.error(`\nFAIL at step ${step} (seed ${SEED}): ${msg}\nlast actions:\n  ${log.slice(-12).join("\n  ")}`);
  process.exit(1);
}
function expect(res, status, what) {
  if (res.status !== status) fail(`${what}: expected ${status}, got ${res.status}: ${JSON.stringify(res.json).slice(0, 300)}`);
  return res.json;
}
const counts = {};
const did = (name) => {
  counts[name] = (counts[name] ?? 0) + 1;
  log.push(`${step} ${name}`);
};

// ---- the model ----
/** number → what the server must say about it. */
const items = new Map();
let next = 1;
const mentions = []; // [source, target] that must show as "referenced" on target
const merges = []; // {number, method, commit}
let branchesDeleted = 0; // a reopened pull request can push its branch back, so `exists` tracks it per pull
const branches = new Map(); // pr number → { user, branch }
const refsIn = (text) => [...text.matchAll(/(^|[^\w&/#[])#(\d+)\b/g)].map((m) => Number(m[2]));
function noteMentions(source, text) {
  for (const t of refsIn(text)) if (t !== source && items.has(t)) mentions.push([source, t]);
}
const can = (user, item) => USERS[user].write || item.author === user;
const decision = (item) => {
  const states = [...item.reviews.values()];
  return states.includes("changes_requested") ? "changes_requested" : states.includes("approved") ? "approved" : undefined;
};
const openOf = (kind) => [...items.values()].filter((i) => i.kind === kind && i.state === "open");
const sentence = () => {
  const refs = items.size && chance(0.35) ? ` See #${int(1, next - 1)}.` : "";
  return pick(["Steps to reproduce attached.", "Looks related to the parser.", "Works for me on main.", "Needs a test.", "Bumping this."]) + refs;
};

// ---- git workspace ----
function edit(user, n) {
  const path = pick(FILES);
  const file = join(dir(user), path);
  const lines = readFileSync(file, "utf8").split("\n");
  lines[int(0, 7)] = `// ${user} step ${step}.${n}`;
  writeFileSync(file, lines.join("\n"));
  git(user, dir(user), "commit", "-qam", `${user}: change ${path} (step ${step}.${n})`);
}
function seedRepository() {
  mkdirSync(dir("maint"), { recursive: true });
  git(null, dir("maint"), "init", "-q", "-b", "main");
  const files = {
    "README.md": "# forge-demo\n\nA small project the forge fuzz works on.\n",
    "Cargo.toml": '[package]\nname = "forge-demo"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/lib.rs": Array.from({ length: 12 }, (_, i) => `pub fn f${i}() -> u32 { ${i} }`).join("\n") + "\n",
    "src/main.rs": Array.from({ length: 12 }, (_, i) => `// main line ${i}`).join("\n") + '\nfn main() { println!("hi"); }\n',
    "docs/guide.md": Array.from({ length: 12 }, (_, i) => `Guide line ${i}.`).join("\n") + "\n",
  };
  for (const [p, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir("maint"), p)), { recursive: true });
    writeFileSync(join(dir("maint"), p), text);
  }
  git("maint", dir("maint"), "add", ".");
  git("maint", dir("maint"), "commit", "-qm", "Initial project");
  for (let i = 0; i < 3; i++) edit("maint", i);
  git("maint", dir("maint"), "push", "-q", REMOTE, "main");
  git("maint", dir("maint"), "remote", "add", "origin", REMOTE);
  for (const u of DEVS) git(u, work, "clone", "-q", REMOTE, u);
}

// ---- actions: each returns false when it found nothing to act on ----
const A = {
  async pushMain() {
    git("maint", dir("maint"), "fetch", "-q", "origin");
    git(null, dir("maint"), "checkout", "-q", "-B", "main", "origin/main");
    edit("maint", 0);
    git("maint", dir("maint"), "push", "-q", "origin", "main");
    did("push main");
  },

  async openPull() {
    const user = pick(DEVS);
    const branch = `${user}/f${step}`;
    git(user, dir(user), "fetch", "-q", "origin");
    git(null, dir(user), "checkout", "-q", "-B", branch, "origin/main");
    for (let n = 0; n < int(1, 3); n++) edit(user, n);
    git(user, dir(user), "push", "-q", "origin", branch);
    const fixes = openOf("issue").length && chance(0.4) ? pick(openOf("issue")).number : null;
    const body = `${sentence()}${fixes ? ` Fixes #${fixes}.` : ""}`;
    const draft = chance(0.2);
    const pr = expect(await api(user, "POST", "/pulls", { title: `Change from ${branch}`, body, base: "main", head: branch, draft }), 201, "open pull");
    if (pr.number !== next) fail(`pull got #${pr.number}, expected #${next}`);
    items.set(next, { number: next, kind: "pull", author: user, state: "open", reason: "", merged: false, draft, labels: new Set(), assignees: new Set(), comments: 0, title: `Change from ${branch}`, body, reviews: new Map() });
    branches.set(next, { user, branch, exists: true });
    noteMentions(next, body);
    next++;
    did(draft ? "open draft pull" : "open pull");
  },

  async pushToPull() {
    const pr = openOf("pull").find((p) => chance(0.5)) ?? openOf("pull")[0];
    if (!pr) return false;
    const { user, branch } = branches.get(pr.number);
    git(null, dir(user), "checkout", "-q", branch);
    edit(user, 9);
    git(user, dir(user), "push", "-q", "origin", branch);
    branches.get(pr.number).exists = true;
    const tip = git(null, dir(user), "rev-parse", "HEAD");
    const got = expect(await api(null, "GET", `/pulls/${pr.number}`), 200, "read pull");
    if (got.head_sha !== tip) fail(`#${pr.number} shows head ${got.head_sha}, the branch is at ${tip}`);
    did("push to pull");
  },

  async review() {
    const pr = pick([...items.values()].filter((i) => i.kind === "pull")) ?? null;
    if (!pr) return false;
    const user = pick(EVERYONE);
    const state = pick(["approved", "changes_requested", "commented"]);
    const cmp = await api(null, "GET", `/pulls/${pr.number}`);
    const files = await api(null, "GET", `/compare/${cmp.json.base_sha}...${cmp.json.head_sha}`);
    const path = files.json.stats?.[0]?.path;
    const comments = path && chance(0.6) ? [{ path, side: "RIGHT", line: 1, body: `Nit in ${path}. ${sentence()}` }] : [];
    const body = state === "approved" && chance(0.5) ? "" : sentence();
    const res = await api(user, "POST", `/pulls/${pr.number}/reviews`, { state, body, comments });
    const refused = pr.state !== "open" || (pr.author === user && state !== "commented");
    if (refused) {
      expect(res, 400, `review #${pr.number} by ${user} (${state}, ${pr.state})`);
      did("refused review");
      return;
    }
    expect(res, 201, `review #${pr.number} by ${user}`);
    if (state !== "commented") pr.reviews.set(user, state);
    noteMentions(pr.number, [body, ...comments.map((c) => c.body)].join(" "));
    did(`review ${state}`);
  },

  async merge() {
    const pr = pick(openOf("pull"));
    if (!pr) return false;
    if (chance(0.15)) {
      expect(await api("carol", "POST", `/pulls/${pr.number}/merge`, {}), 403, "carol merges");
      did("refused merge (read only)");
      return;
    }
    if (pr.draft) {
      expect(await api("maint", "POST", `/pulls/${pr.number}/merge`, {}), 400, `merge draft #${pr.number}`);
      expect(await api(pr.author, "PATCH", `/pulls/${pr.number}`, { draft: false }), 200, "ready for review");
      pr.draft = false;
      did("refused draft merge, then ready");
      return;
    }
    const m = expect(await api("maint", "GET", `/pulls/${pr.number}/merge`), 200, "mergeability");
    if (m.up_to_date) {
      expect(await api("maint", "POST", `/pulls/${pr.number}/merge`, {}), 400, "merge up to date");
      did("nothing to merge");
      return;
    }
    if (!m.mergeable) {
      expect(await api("maint", "POST", `/pulls/${pr.number}/merge`, {}), 409, `merge conflicting #${pr.number}`);
      // The author merges main into the branch, keeping theirs, and pushes: no conflict left.
      const { user, branch } = branches.get(pr.number);
      git(user, dir(user), "fetch", "-q", "origin");
      git(null, dir(user), "checkout", "-q", branch);
      git(user, dir(user), "merge", "-q", "-X", "ours", "--no-edit", "origin/main");
      git(user, dir(user), "push", "-q", "origin", branch);
      branches.get(pr.number).exists = true;
      did("conflict, then main merged into the branch");
      return;
    }
    const method = pick(m.rebaseable ? ["merge", "squash", "rebase"] : ["merge", "squash"]);
    if (!m.rebaseable) {
      // GitHub offers no rebase here: the server must refuse it.
      expect(await api("maint", "POST", `/pulls/${pr.number}/merge`, { method: "rebase" }), 409, `rebase of unrebaseable #${pr.number}`);
      did("refused rebase (not rebaseable)");
    }
    const done = expect(await api("maint", "POST", `/pulls/${pr.number}/merge`, { method, sha: m.head_sha }), 200, `merge #${pr.number} (${method})`);
    Object.assign(pr, { state: "closed", merged: true, reason: "", draft: false });
    merges.push({ number: pr.number, method, commit: done.merge_commit });
    // GitHub's closing keywords, anywhere in the body (the same rule as the server).
    for (const [, , k] of pr.body.matchAll(/\b(close[sd]?|fix(?:e[sd])?|resolve[sd]?):?\s+#(\d+)/gi)) {
      const fixed = items.get(Number(k));
      if (fixed?.kind === "issue" && fixed.state === "open") Object.assign(fixed, { state: "closed", reason: "completed" });
    }
    did(`merge (${method})`);
  },

  async closeOrReopenPull() {
    const pr = pick([...items.values()].filter((i) => i.kind === "pull" && !i.merged));
    if (!pr) return false;
    const user = pick(EVERYONE);
    const state = pr.state === "open" ? "closed" : "open";
    const res = await api(user, "PATCH", `/pulls/${pr.number}`, { state });
    if (!can(user, pr)) {
      expect(res, 400, `${user} ${state} #${pr.number}`);
      did("refused close/reopen");
      return;
    }
    expect(res, 200, `${state} #${pr.number}`);
    Object.assign(pr, { state, reason: state === "closed" ? "completed" : "" });
    did(`pull ${state}`);
  },

  async reopenMerged() {
    const pr = pick(merges.map((m) => items.get(m.number)));
    if (!pr) return false;
    expect(await api("maint", "PATCH", `/pulls/${pr.number}`, { state: "open" }), 400, "reopen merged");
    did("refused reopen of merged");
  },

  async openIssue() {
    const user = pick(["alice", "bob", "carol", "carol"]);
    const labels = chance(0.5) ? [pick(Object.keys(LABELS))] : [];
    const body = sentence();
    const res = await api(user, "POST", "/issues", { title: `${pick(["Crash", "Typo", "Slow", "Question about"])} ${pick(["parser", "docs", "CLI", "build"])} (${step})`, body, labels });
    if (labels.length && !USERS[user].write) {
      expect(res, 400, "read-only user sets labels");
      did("refused labels at open");
      return;
    }
    const issue = expect(res, 201, "open issue");
    if (issue.number !== next) fail(`issue got #${issue.number}, expected #${next}`);
    items.set(next, { number: next, kind: "issue", author: user, state: "open", reason: "", merged: false, draft: false, labels: new Set(labels), assignees: new Set(), comments: 0, title: issue.title, body, reviews: new Map() });
    noteMentions(next, `${issue.title} ${body}`);
    next++;
    did("open issue");
  },

  async comment() {
    const item = pick([...items.values()]);
    if (!item) return false;
    const user = pick(EVERYONE);
    const body = sentence();
    const close = item.kind === "issue" && item.state === "open" && chance(0.15) ? pick(["completed", "not_planned"]) : undefined;
    const res = await api(user, "POST", `/issues/${item.number}/comments`, { body, close });
    if (close && !can(user, item)) {
      expect(res, 400, `${user} closes #${item.number} with a comment`);
      did("refused close with comment");
      return;
    }
    expect(res, 201, `comment on #${item.number}`);
    item.comments++;
    noteMentions(item.number, body);
    if (close) Object.assign(item, { state: "closed", reason: close });
    did(close ? "close with comment" : "comment");
  },

  async closeOrReopenIssue() {
    const issue = pick([...items.values()].filter((i) => i.kind === "issue"));
    if (!issue) return false;
    const user = pick(EVERYONE);
    const closing = issue.state === "open";
    const reason = pick(["completed", "not_planned"]);
    const res = await api(user, "PATCH", `/issues/${issue.number}`, closing ? { state: "closed", state_reason: reason } : { state: "open" });
    if (!can(user, issue)) {
      expect(res, 400, `${user} ${closing ? "closes" : "reopens"} #${issue.number}`);
      did("refused issue state");
      return;
    }
    expect(res, 200, "issue state");
    Object.assign(issue, closing ? { state: "closed", reason } : { state: "open", reason: "" });
    did(closing ? `close issue (${reason})` : "reopen issue");
  },

  async labelOrAssign() {
    const item = pick([...items.values()]);
    if (!item) return false;
    const user = pick(EVERYONE);
    const label = chance(0.6);
    const set = label ? item.labels : item.assignees;
    const value = label ? pick(Object.keys(LABELS)) : pick(EVERYONE);
    const add = !set.has(value);
    const key = `${add ? "add" : "remove"}_${label ? "labels" : "assignees"}`;
    const res = await api(user, "PATCH", `/issues/${item.number}`, { [key]: [value] });
    if (!USERS[user].write) {
      expect(res, 400, `${user} ${key}`);
      did("refused label/assign");
      return;
    }
    expect(res, 200, key);
    add ? set.add(value) : set.delete(value);
    did(key);
  },

  async editTitle() {
    const item = pick([...items.values()]);
    if (!item) return false;
    const user = pick(EVERYONE);
    const title = `${item.title.replace(/ \(edited \d+\)$/, "")} (edited ${step})`;
    const res = await api(user, "PATCH", `/issues/${item.number}`, { title });
    if (!can(user, item)) {
      expect(res, 400, "edit title");
      did("refused edit");
      return;
    }
    expect(res, 200, "edit title");
    item.title = title;
    did("edit title");
  },

  async editOrDeleteComment() {
    const item = pick([...items.values()].filter((i) => i.comments > 0));
    if (!item) return false;
    const detail = expect(await api(null, "GET", `/issues/${item.number}`), 200, "read item");
    const c = pick(detail.timeline.filter((t) => t.type === "comment" && !t.deleted));
    if (!c) fail(`#${item.number} counts ${item.comments} comments but the timeline has none`);
    const user = pick(EVERYONE);
    const remove = chance(0.4);
    const res = remove
      ? await api(user, "DELETE", `/issues/${item.number}/comments/${c.id}`)
      : await api(user, "PATCH", `/issues/${item.number}/comments/${c.id}`, { body: `${c.body} (edited)` });
    if (!USERS[user].write && c.actor !== user) {
      expect(res, 400, `${user} edits ${c.actor}'s comment`);
      did("refused comment edit");
      return;
    }
    expect(res, 200, remove ? "delete comment" : "edit comment");
    if (remove) item.comments--;
    did(remove ? "delete comment" : "edit comment");
  },

  async deleteBranch() {
    const pr = [...items.values()].find((i) => i.kind === "pull" && i.state === "closed" && branches.get(i.number).exists);
    if (!pr) return false;
    expect(await api("maint", "DELETE", `/pulls/${pr.number}/branch`), 200, `delete branch of #${pr.number}`);
    branches.get(pr.number).exists = false;
    branchesDeleted++;
    did("delete branch");
  },

  async labelCatalog() {
    const name = pick(Object.keys(LABELS));
    if (chance(0.3)) {
      const res = await api("maint", "DELETE", `/labels/${encodeURIComponent(name)}`);
      if (res.status === 200) {
        for (const i of items.values()) i.labels.delete(name);
        did("delete label");
      } else expect(res, 400, "delete missing label");
    } else {
      expect(await api("maint", "PUT", `/labels/${encodeURIComponent(name)}`, { color: LABELS[name], description: `${name} (${step})` }), 200, "define label");
      did("define label");
    }
  },

  async probes() {
    expect(await api(null, "POST", "/issues", { title: "anonymous" }), 401, "anonymous write");
    expect(await api("carol", "PUT", "/labels/bug", { color: "#ffffff" }), 400, "read-only user edits labels");
    did("permission probes");
  },
};

const WEIGHTS = [
  ["pushMain", 6], ["openPull", 10], ["pushToPull", 5], ["review", 10], ["merge", 9], ["closeOrReopenPull", 3],
  ["reopenMerged", 1], ["openIssue", 10], ["comment", 14], ["closeOrReopenIssue", 6], ["labelOrAssign", 8],
  ["editTitle", 3], ["editOrDeleteComment", 4], ["deleteBranch", 3], ["labelCatalog", 2], ["probes", 1],
];

// ---- reconciliation: the server must equal the model ----
async function reconcile() {
  for (const m of items.values()) {
    const got = expect(await api(null, "GET", `/issues/${m.number}`), 200, `read #${m.number}`);
    const want = {
      kind: m.kind, state: m.state, merged: m.merged, draft: m.draft, title: m.title, comments: m.comments,
      labels: [...m.labels].sort(), assignees: [...m.assignees].sort(),
      reason: m.kind === "issue" ? m.reason : "", decision: m.kind === "pull" ? decision(m) : undefined,
    };
    const have = {
      kind: got.kind, state: got.state, merged: got.merged, draft: got.draft, title: got.title, comments: got.comments,
      labels: [...got.labels].sort(), assignees: [...got.assignees].sort(),
      reason: m.kind === "issue" ? got.state_reason : "", decision: m.kind === "pull" ? got.review_decision : undefined,
    };
    if (JSON.stringify(want) !== JSON.stringify(have)) fail(`#${m.number} differs\n  model:  ${JSON.stringify(want)}\n  server: ${JSON.stringify(have)}`);
  }
  for (const [kind, path] of [["issue", "/issues"], ["pull", "/pulls"]]) {
    const page = expect(await api(null, "GET", `${path}?q=${encodeURIComponent("is:open")}`), 200, `list ${kind}`);
    const all = [...items.values()].filter((i) => i.kind === kind);
    const open = all.filter((i) => i.state === "open").length;
    if (page.open_count !== open || page.closed_count !== all.length - open) fail(`${kind} counts ${page.open_count}/${page.closed_count}, model ${open}/${all.length - open}`);
  }
  const sum = expect(await api(null, "GET", "/forge/summary"), 200, "summary");
  if (sum.open_issues !== openOf("issue").length || sum.open_pulls !== openOf("pull").length) fail(`summary ${JSON.stringify(sum)}`);
  for (const [source, target] of mentions) {
    const t = expect(await api(null, "GET", `/issues/${target}`), 200, "read target");
    if (!t.timeline.some((e) => e.type === "referenced" && e.source === source)) fail(`#${source} mentions #${target}, but #${target} doesn't show it`);
  }
}

async function snapshot(out) {
  const all = [];
  for (let n = 1; ; n++) {
    const r = await api(null, "GET", `/issues/${n}`);
    if (r.status === 404) break;
    all.push(expect(r, 200, `read #${n}`));
  }
  writeFileSync(out, JSON.stringify(all, null, 1));
  console.log(`${all.length} items written to ${out}`);
}

async function main() {
  if (process.env.MODE === "snapshot") return snapshot(process.env.OUT ?? "forge-snapshot.json");
  console.log(`forge fuzz: ${REPO}, seed ${SEED}, ${STEPS} steps, workspace ${work}`);
  const created = await fetch(`${BASE}/${REPO}/api`, { method: "PUT", headers: { Authorization: `Bearer ${USERS.maint.token}` } });
  if (created.status !== 201) fail(`create ${REPO}: ${created.status} (it must not exist yet)`);
  seedRepository();
  for (const [name, color] of Object.entries(LABELS)) expect(await api("maint", "PUT", `/labels/${encodeURIComponent(name)}`, { color, description: name }), 200, "seed label");
  const total = WEIGHTS.reduce((n, [, w]) => n + w, 0);
  for (step = 1; step <= STEPS; step++) {
    let r = rand() * total;
    const [name] = WEIGHTS.find(([, w]) => (r -= w) < 0) ?? WEIGHTS[0];
    try {
      if ((await A[name]()) === false) did(`${name} (nothing to do)`);
    } catch (e) {
      fail(`${name} threw: ${e.stderr ?? e.message ?? e}`);
    }
    if (step % 25 === 0) {
      await reconcile();
      process.stdout.write(`  step ${step}: ${items.size} items, ${merges.length} merged — model matches\n`);
    }
  }
  await reconcile();

  // The git side, from a fresh clone.
  const fresh = join(work, "fresh");
  git("maint", work, "clone", "-q", REMOTE, fresh);
  git(null, fresh, "fsck", "--full", "--no-progress");
  const main = git(null, fresh, "rev-parse", "origin/main");
  for (const m of merges) {
    try {
      git(null, fresh, "merge-base", "--is-ancestor", m.commit, main);
    } catch {
      fail(`merge of #${m.number} (${m.commit}) is not on main`);
    }
    const parents = git(null, fresh, "log", "-1", "--format=%P", m.commit).split(" ").length;
    if ((m.method === "merge") !== (parents === 2)) fail(`#${m.number} ${m.method} commit has ${parents} parents`);
  }
  for (const [n, b] of branches) {
    if (b.exists) continue;
    if (git("maint", fresh, "ls-remote", "origin", `refs/heads/${branches.get(n).branch}`)) fail(`branch of #${n} (${b.branch}) was deleted but exists`);
  }
  console.log(`\nOK seed ${SEED}: ${STEPS} steps, ${items.size} items (${openOf("issue").length} open issues, ${openOf("pull").length} open pulls), ${merges.length} merges, ${mentions.length} cross-references, ${branchesDeleted} branches deleted; fresh clone fsck clean`);
  console.log(Object.entries(counts).sort((a, b) => b[1] - a[1]).map(([k, v]) => `  ${v}\t${k}`).join("\n"));
}

await main();
