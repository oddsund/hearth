const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let state = { addons: [], addons_dir: "" };
let updates = {}; // id -> UpdateInfo
let dlRows = []; // zips found in Downloads
let actionableRows = [];
let busy = false;

let linkOpen = null; // id of the addon whose Link editor is open

const sourceLabel = (provider, source) =>
  provider === "github" ? source
  : provider === "wowi" ? `WoWInterface #${source}`
  : provider === "curse" ? `CurseForge #${source}`
  : "manual (zip only)";

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function status(msg, isError = false) {
  const el = $("status");
  el.textContent = msg;
  el.classList.toggle("error", isError);
}

// One-off actions (check updates, save settings, remove): block only the global buttons.
async function run(label, fn) {
  if (busy) return;
  busy = true;
  render();
  status(label + "…");
  try {
    const r = await fn();
    return r;
  } catch (e) {
    status(String(e), true);
  } finally {
    busy = false;
    render();
  }
}

// ---- Install queue: click as many as you like; up to MAX_PARALLEL run at once.
const MAX_PARALLEL = 4;
const jobs = new Map(); // key -> "queued" | "running"
const queue = [];
let running = 0;
let results = []; // finished since the queue was last empty

/** fn resolves to a success message. Keys stop the same thing being queued twice. */
function enqueue(key, label, fn) {
  if (jobs.has(key)) return;
  jobs.set(key, "queued");
  queue.push({ key, label, fn });
  pump();
  render();
}

function pump() {
  while (running < MAX_PARALLEL && queue.length) {
    const job = queue.shift();
    running++;
    jobs.set(job.key, "running");
    job
      .fn()
      .then((msg) => results.push({ ok: true, msg }))
      .catch((e) => results.push({ ok: false, msg: `${job.label}: ${e}` }))
      .finally(async () => {
        running--;
        jobs.delete(job.key);
        try { await refresh(); } catch (_) {}
        pump();
        progress();
      });
  }
  progress();
}

function progress() {
  const queued = queue.length;
  if (running || queued) {
    const done = results.length;
    status(`Installing ${running}${queued ? `, ${queued} queued` : ""}${done ? ` · ${done} done` : ""}…`);
    return;
  }
  if (!results.length) return;
  const failed = results.filter((r) => !r.ok);
  const ok = results.filter((r) => r.ok);
  const lines = ok.length === 1 && !failed.length ? [ok[0].msg] : ok.length ? [`Finished ${ok.length}: ` + ok.map((r) => r.msg).join(" · ")] : [];
  if (failed.length) lines.push(`${failed.length} failed:`, ...failed.map((r) => "  " + r.msg));
  status(lines.join("\n"), failed.length > 0);
  results = [];
}

/** Button for a queueable action: shows Queued…/Installing… while its job is pending. */
function jobButton(key, attrs, label, cls = "") {
  const st = jobs.get(key);
  if (st) return `<button class="small ${cls}" disabled>${st === "running" ? "Installing…" : "Queued…"}</button>`;
  return `<button class="small ${cls}" ${attrs}>${label}</button>`;
}

// Opens in the user's own browser via the backend (allow-listed sites only).
const extLink = (url, text) => `<a href="#" class="ext" data-open="${esc(url)}">${esc(text)}</a>`;

/** Where to find the CurseForge Project ID: the page itself if we know it, else a search. */
function findIdLinks(a) {
  if (a.curse_id) return extLink(`https://www.curseforge.com/projects/${encodeURIComponent(a.curse_id)}`, `Open ${a.name} on CurseForge ↗`);
  const q = encodeURIComponent(a.name);
  const web = encodeURIComponent(`${a.name} ${a.folders[0] ?? ""} site:curseforge.com/wow`);
  return `${extLink(`https://www.curseforge.com/wow/search?class=addons&search=${q}`, `Search CurseForge for “${a.name}” ↗`)}
    <span class="muted">·</span> ${extLink(`https://duckduckgo.com/?q=${web}`, "or search the web ↗")}`;
}

function linkRow(a) {
  const hint = a.curse_id
    ? `This zip names CurseForge project <code>${esc(a.curse_id)}</code>.`
    : `Open the addon's page and copy the <strong>Project ID</strong> from the "About Project" box.`;
  return `<tr class="link-row"><td colspan="5">
    <div class="link-find">${findIdLinks(a)}</div>
    <div class="row">
      <input id="in-link" spellcheck="false" placeholder="CurseForge project ID, GitHub URL, or WoWInterface URL" value="${esc(a.curse_id ?? "")}">
      <button class="small primary" data-link-go="${esc(a.id)}">Link &amp; update</button>
      <button class="small ghost" data-link-cancel>Cancel</button>
    </div>
    <div class="folders">${hint} Hearth then installs the newest build for your game from there and keeps it updated.</div>
  </td></tr>`;
}

document.addEventListener("click", (e) => {
  const a = e.target.closest("[data-open]");
  if (!a) return;
  e.preventDefault();
  const url = a.dataset.open;
  invoke("open_url", { url }).catch((err) => {
    // Fall back to showing the link so it can be copied into a browser by hand.
    status(`${err}\nCopy this link into your browser instead:\n${url}`, true);
    const s = window.getSelection(), r = document.createRange();
    const node = $("status").firstChild;
    if (node) { r.setStart(node, node.length - url.length); r.setEnd(node, node.length); s.removeAllRanges(); s.addRange(r); }
  });
});

function render() {
  const tb = $("tracked");
  tb.innerHTML = state.addons
    .map((a) => {
      const u = updates[a.id];
      let latest = '<span class="muted">–</span>';
      if (a.provider === "local") latest = '<span class="muted" title="No online source known. Download a newer zip to update.">manual</span>';
      else if (u?.error) latest = `<span class="bad" title="${esc(u.error)}">error</span>`;
      else if (u?.has_update) latest = `<span class="new">${esc(u.latest)}</span>`;
      else if (u) latest = '<span class="muted">up to date</span>';
      const src = sourceLabel(a.provider, a.source);
      return `<tr>
        <td><strong>${esc(a.name)}</strong><div class="folders">${a.folders.map(esc).join(", ")}</div></td>
        <td>${a.url ? extLink(a.url, src) : esc(src)}${a.asset_pattern ? `<div class="folders">pattern: <code>${esc(a.asset_pattern)}</code></div>` : ""}</td>
        <td>${esc(a.version)}</td>
        <td>${latest}</td>
        <td class="actions-cell">
          ${a.provider === "local"
            ? jobButton("ln:" + a.id, `data-link="${esc(a.id)}"`, "Link", "primary")
            : jobButton("up:" + a.id, `data-update="${esc(a.id)}"`, u?.has_update ? "Update" : "Reinstall", u?.has_update ? "primary" : "")}
          <button class="small danger" data-remove="${esc(a.id)}" ${jobs.has("up:" + a.id) || jobs.has("ln:" + a.id) ? "disabled" : ""}>Remove</button>
        </td></tr>${linkOpen === a.id ? linkRow(a) : ""}`;
    })
    .join("");
  // Downloads: show what can be installed/updated; just count the rest.
  const hidden = ["installed", "older", "other_game"];
  const actionable = dlRows.filter((r) => !hidden.includes(r.status));
  const otherGame = dlRows.filter((r) => r.status === "other_game").length;
  const label = { new: "Install", update: "Update", different: "Install" };
  $("dl-card").hidden = dlRows.length === 0;
  $("dl-count").textContent = actionable.length ? `(${actionable.length})` : "";
  $("downloads").innerHTML = actionable
    .map((r, i) => {
      const { title, version, interface: iface } = r.main;
      const ver =
        r.status === "update" ? `<span class="new">${esc(version)}</span> <span class="muted">(have ${esc(r.installed_version)})</span>`
        : r.status === "different" ? `${esc(version ?? "?")} <span class="muted">(have ${esc(r.installed_version)})</span>`
        : esc(version ?? "?");
      const warn = r.compatible === false
        ? `<div class="warn">Not marked for interface ${esc(state.game_interface)} (lists ${esc(iface ?? "none")}); may not load</div>` : "";
      return `<tr>
        <td><strong>${esc(title)}</strong><div class="folders">${r.folders.map(esc).join(", ")}</div>${warn}</td>
        <td class="folders">${esc(r.file_name)}</td>
        <td>${ver}</td>
        <td>${esc(sourceLabel(r.link_provider, r.link_source))}</td>
        <td class="actions-cell">${
          r.compatible === false && r.link_provider !== "local"
            ? jobButton("rm:" + r.link_provider + ":" + r.link_source,
                `data-remote="${i}" title="Install the newest build for your game from ${esc(sourceLabel(r.link_provider, r.link_source))} instead of this zip"`,
                "Get build for your game", "primary")
            : jobButton("dl:" + r.path, `data-local="${i}"`, label[r.status], r.status === "new" ? "" : "primary")
        }</td>
      </tr>`;
    })
    .join("");
  const done = dlRows.length - actionable.length - otherGame;
  const notes = [];
  if (done) notes.push(`${done} already installed`);
  if (otherGame) notes.push(`${otherGame} for another game version (you have a build that fits)`);
  $("dl-installed").textContent = !actionable.length && !otherGame
    ? "Everything in Downloads is already installed."
    : notes.length ? `${actionable.length ? "Also: " : ""}${notes.join(", ")}.` : "";
  $("dl-installed").hidden = !$("dl-installed").textContent;
  actionableRows = actionable;

  $("empty").hidden = state.addons.length > 0;
  $("count").textContent = state.addons.length ? `(${state.addons.length})` : "";

  for (const id of ["btn-check", "btn-save", "btn-rescan"]) $(id).disabled = busy;
  $("btn-update-all").disabled = busy || !Object.values(updates).some((u) => u.has_update && !jobs.has("up:" + u.id));
  $("btn-install").disabled = jobs.has("url:" + $("in-url").value.trim());
}

async function refresh() {
  state = await invoke("get_state");
  $("in-dir").value = state.addons_dir;
  $("in-token").value = state.github_token ?? "";
  $("in-dl").value = state.downloads_dir;
  $("in-iface").value = state.game_interface;
  dlRows = await invoke("scan_downloads");
  const rows = await invoke("scan");
  const untracked = rows.filter((r) => !r.tracked_by);
  $("untracked-card").hidden = untracked.length === 0;
  $("untracked").innerHTML = untracked
    .map((r) => {
      const ids = [
        r.curse_id && `Curse ${r.curse_id}`,
        r.wowi_id && `WoWI ${r.wowi_id}`,
        r.wago_id && `Wago ${r.wago_id}`,
      ].filter(Boolean);
      return `<tr><td>${esc(r.folder)}</td><td>${esc(r.title)}</td><td>${esc(r.version ?? "")}</td><td class="folders">${esc(ids.join(" · "))}</td></tr>`;
    })
    .join("");
  if (!state.addons_dir) {
    $("settings").hidden = false;
    status("Couldn't find your AddOns folder. Set it in Settings.", true);
  }
  render();
}

$("btn-settings").onclick = () => ($("settings").hidden = !$("settings").hidden);

$("btn-save").onclick = () =>
  run("Saving", async () => {
    state = await invoke("save_settings", {
      addonsDir: $("in-dir").value,
      downloadsDir: $("in-dl").value,
      gameInterface: $("in-iface").value,
      githubToken: $("in-token").value || null,
    });
    await refresh();
    status("Settings saved.");
  });

$("btn-install").onclick = () => {
  const url = $("in-url").value.trim();
  if (!url) return status("Paste a URL first.", true);
  const assetPattern = $("in-pattern").value || null;
  // Clear right away so the next URL can be pasted while this one installs.
  $("in-url").value = "";
  $("in-pattern").value = "";
  enqueue("url:" + url, url, async () => {
    const a = await invoke("install", { url, assetPattern });
    delete updates[a.id];
    return `${a.name} ${a.version}`;
  });
};
$("in-url").addEventListener("keydown", (e) => e.key === "Enter" && $("btn-install").click());

$("btn-check").onclick = () =>
  run("Checking for updates", async () => {
    const list = await invoke("check_updates");
    updates = Object.fromEntries(list.map((u) => [u.id, u]));
    const n = list.filter((u) => u.has_update).length;
    const errs = list.filter((u) => u.error);
    status(
      (n ? `${n} update${n > 1 ? "s" : ""} available.` : "Everything is up to date.") +
        (errs.length ? `\n${errs.length} failed: ${errs.map((e) => e.id + " – " + e.error).join("; ")}` : ""),
      errs.length > 0
    );
  });

function enqueueUpdate(id) {
  const name = state.addons.find((a) => a.id === id)?.name ?? id;
  enqueue("up:" + id, name, async () => {
    const a = await invoke("update_addon", { id });
    delete updates[id];
    return `${a.name} ${a.version}`;
  });
}

$("btn-update-all").onclick = () =>
  Object.values(updates).filter((u) => u.has_update).forEach((u) => enqueueUpdate(u.id));

$("btn-rescan").onclick = () => run("Scanning Downloads", async () => { await refresh(); status("Rescanned Downloads."); });

const sourceUrl = (provider, source) =>
  provider === "wowi" ? `https://www.wowinterface.com/downloads/info${source}` : source; // github: owner/repo, curse: id

$("downloads").onclick = (e) => {
  const ri = e.target.closest("[data-remote]")?.dataset.remote;
  if (ri !== undefined) {
    const r = actionableRows[Number(ri)];
    return enqueue("rm:" + r.link_provider + ":" + r.link_source, r.main.title, async () => {
      const a = await invoke("install", { url: sourceUrl(r.link_provider, r.link_source), assetPattern: null });
      delete updates[a.id];
      return `${a.name} ${a.version} (from ${sourceLabel(a.provider, a.source)})`;
    });
  }
  const i = e.target.closest("[data-local]")?.dataset.local;
  if (i === undefined) return;
  const r = actionableRows[Number(i)];
  enqueue("dl:" + r.path, r.main.title, async () => {
    const a = await invoke("install_local", { path: r.path });
    delete updates[a.id];
    return `${a.name} ${a.version}${a.provider === "local" ? " (manual)" : ` (updates from ${sourceLabel(a.provider, a.source)})`}`;
  });
};

$("tracked").onclick = (e) => {
  const up = e.target.closest("[data-update]")?.dataset.update;
  const rm = e.target.closest("[data-remove]")?.dataset.remove;
  const ln = e.target.closest("[data-link]")?.dataset.link;
  const go = e.target.closest("[data-link-go]")?.dataset.linkGo;
  if (e.target.closest("[data-link-cancel]")) {
    linkOpen = null;
    return render();
  }
  if (ln) {
    linkOpen = linkOpen === ln ? null : ln;
    render();
    $("in-link")?.focus();
    return;
  }
  if (go) {
    const source = $("in-link").value.trim();
    if (!source) return status("Paste a CurseForge project ID, GitHub URL or WoWInterface URL.", true);
    const name = state.addons.find((a) => a.id === go)?.name ?? go;
    linkOpen = null;
    return enqueue("ln:" + go, name, async () => {
      const a = await invoke("link_addon", { id: go, source });
      delete updates[go];
      return `${a.name} ${a.version} (now updates from ${sourceLabel(a.provider, a.source)})`;
    });
  }
  if (up) enqueueUpdate(up);
  if (rm) {
    const a = state.addons.find((x) => x.id === rm);
    if (!confirm(`Remove ${a?.name}? Its folders are moved to the backup folder, not deleted.`)) return;
    run("Removing " + rm, async () => {
      await invoke("uninstall", { id: rm });
      delete updates[rm];
      await refresh();
      status(`Removed ${a?.name}.`);
    });
  }
};

(async () => {
  try {
    $("backup-loc").textContent = "Backups: " + (await invoke("backup_location"));
    await refresh();
    if (state.addons_dir) status("Ready. AddOns: " + state.addons_dir);
  } catch (e) {
    status(String(e), true);
  }
})();
