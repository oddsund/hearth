const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let state = { addons: [], addons_dir: "" };
let updates = {}; // id -> UpdateInfo
let dlRows = []; // zips found in Downloads
let actionableRows = [];
let busy = false;

const sourceLabel = (provider, source) =>
  provider === "github" ? source : provider === "wowi" ? `WoWInterface #${source}` : "manual (zip only)";

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function status(msg, isError = false) {
  const el = $("status");
  el.textContent = msg;
  el.classList.toggle("error", isError);
}

async function run(label, fn) {
  if (busy) return;
  busy = true;
  document.querySelectorAll("button").forEach((b) => (b.disabled = true));
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
        <td><span title="${esc(a.url)}">${esc(src)}</span>${a.asset_pattern ? `<div class="folders">pattern: <code>${esc(a.asset_pattern)}</code></div>` : ""}</td>
        <td>${esc(a.version)}</td>
        <td>${latest}</td>
        <td class="actions-cell">
          ${a.provider === "local" ? "" : `<button class="small" data-update="${esc(a.id)}">${u?.has_update ? "Update" : "Reinstall"}</button>`}
          <button class="small danger" data-remove="${esc(a.id)}">Remove</button>
        </td></tr>`;
    })
    .join("");
  // Downloads: show what can be installed/updated; just count the rest.
  const actionable = dlRows.filter((r) => r.status !== "installed" && r.status !== "older");
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
        <td class="actions-cell"><button class="small ${r.status === "new" ? "" : "primary"}" data-local="${i}">${label[r.status]}</button></td>
      </tr>`;
    })
    .join("");
  const done = dlRows.length - actionable.length;
  $("dl-installed").textContent = !actionable.length
    ? "Everything in Downloads is already installed."
    : done ? `${done} more already installed.` : "";
  $("dl-installed").hidden = !$("dl-installed").textContent;
  actionableRows = actionable;

  $("empty").hidden = state.addons.length > 0;
  $("count").textContent = state.addons.length ? `(${state.addons.length})` : "";

  document.querySelectorAll("button").forEach((b) => (b.disabled = busy));
  $("btn-update-all").disabled = busy || !Object.values(updates).some((u) => u.has_update);
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
  run("Installing " + url, async () => {
    const a = await invoke("install", { url, assetPattern: $("in-pattern").value || null });
    delete updates[a.id];
    $("in-url").value = "";
    $("in-pattern").value = "";
    await refresh();
    status(`Installed ${a.name} ${a.version} → ${a.folders.join(", ")}`);
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

$("btn-update-all").onclick = () =>
  run("Updating", async () => {
    const ids = Object.values(updates).filter((u) => u.has_update).map((u) => u.id);
    const failed = [];
    for (const id of ids) {
      status(`Updating ${id}…`);
      try {
        await invoke("update_addon", { id });
        delete updates[id];
      } catch (e) {
        failed.push(`${id}: ${e}`);
      }
    }
    await refresh();
    status(failed.length ? "Some updates failed:\n" + failed.join("\n") : `Updated ${ids.length} addon(s).`, failed.length > 0);
  });

$("btn-rescan").onclick = () => run("Scanning Downloads", async () => { await refresh(); status("Rescanned Downloads."); });

$("downloads").onclick = (e) => {
  const i = e.target.closest("[data-local]")?.dataset.local;
  if (i === undefined) return;
  const r = actionableRows[Number(i)];
  run("Installing " + r.file_name, async () => {
    const a = await invoke("install_local", { path: r.path });
    delete updates[a.id];
    await refresh();
    const how = a.provider === "local" ? "update it by downloading a newer zip" : `updates will come from ${sourceLabel(a.provider, a.source)}`;
    status(`Installed ${a.name} ${a.version}. ${how[0].toUpperCase() + how.slice(1)}.`);
  });
};

$("tracked").onclick = (e) => {
  const up = e.target.closest("[data-update]")?.dataset.update;
  const rm = e.target.closest("[data-remove]")?.dataset.remove;
  if (up)
    run("Updating " + up, async () => {
      const a = await invoke("update_addon", { id: up });
      delete updates[up];
      await refresh();
      status(`${a.name} is now ${a.version}`);
    });
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
