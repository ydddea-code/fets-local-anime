/* Native Windows folder/file pickers, drag-and-drop, and live preferences. */
(function () {
  "use strict";
  const byId = (id) => document.getElementById(id);
  const status = byId("settings-status");
  const scanStatus = byId("scan-status");
  const scanProgress = byId("scan-progress");
  const setStatus = (message, error = false) => {
    if (status) {
      status.textContent = message;
      status.classList.toggle("error", error);
    }
  };

  async function saveSettings() {
    const data = {};
    if (byId("player")) data.player = byId("player").value;
    if (byId("mpv-path")) data.mpv_path = byId("mpv-path").value;
    if (byId("ffmpeg-path")) data.ffmpeg_path = byId("ffmpeg-path").value;
    if (byId("ffprobe-path")) data.ffprobe_path = byId("ffprobe-path").value;
    if (byId("theme")) data.theme = byId("theme").value;
    if (byId("watch")) data.watch = byId("watch").checked;
    if (byId("anilist")) data.anilist_enabled = byId("anilist").checked;
    const response = await fetch("/api/settings", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(data),
    });
    if (!response.ok) throw new Error(await response.text());
    if (data.theme) document.documentElement.dataset.theme = data.theme === "system"
      ? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light") : data.theme;
    setStatus("Pengaturan tersimpan otomatis.");
  }

  async function loadSettings() {
    const response = await fetch("/api/settings");
    if (!response.ok) throw new Error(await response.text());
    const data = await response.json();
    for (const [id, key] of [["player", "player"], ["mpv-path", "mpv_path"], ["ffmpeg-path", "ffmpeg_path"], ["ffprobe-path", "ffprobe_path"], ["theme", "theme"]]) {
      if (byId(id)) byId(id).value = data[key];
    }
    if (byId("watch")) byId("watch").checked = data.watch;
    if (byId("anilist")) byId("anilist").checked = data.anilist_enabled;
    if (data.theme) document.documentElement.dataset.theme = data.theme === "system"
      ? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light") : data.theme;
    renderFolders(data.library_paths || []);
  }

  function renderFolders(paths) {
    const list = byId("library-folders");
    if (!list) return;
    list.replaceChildren();
    for (const path of paths) {
      const li = document.createElement("li");
      const label = document.createElement("span");
      label.textContent = path;
      label.title = path;
      const remove = document.createElement("button");
      remove.className = "icon-button";
      remove.type = "button";
      remove.textContent = "Hapus";
      remove.setAttribute("aria-label", "Hapus folder " + path);
      remove.addEventListener("click", async () => {
        const response = await fetch("/api/library/remove-folder", {
          method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ path }),
        });
        if (!response.ok) return setStatus(await response.text(), true);
        await loadSettings();
        setStatus("Folder library dihapus.");
      });
      li.append(label, remove);
      list.append(li);
    }
    if (!paths.length) {
      const empty = document.createElement("li");
      empty.className = "muted";
      empty.textContent = "Belum ada folder. Pilih Tambah Folder untuk memulai.";
      list.append(empty);
    }
  }

  async function addFolder(path) {
    if (!path) return;
    if (scanStatus) scanStatus.textContent = "Memindai folder…";
    const response = await fetch("/api/library/add-folder", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ path }),
    });
    if (!response.ok) {
      const message = await response.text();
      if (scanStatus) scanStatus.textContent = message;
      setStatus(message, true);
      return;
    }
    const result = await response.json();
    await loadSettings();
    if (scanStatus) scanStatus.textContent = "Folder ditambahkan. Memulai pemindaian…";
    if (result.job_id) await refreshScanStatus();
  }

  async function choose(options) {
    const dialog = window.__TAURI__ && window.__TAURI__.dialog;
    if (!dialog) {
      setStatus("Dialog native tersedia saat Fets Anime dibuka sebagai aplikasi Windows.", true);
      return null;
    }
    return dialog.open(options);
  }

  const addButton = byId("add-folder");
  if (addButton) addButton.addEventListener("click", async () => {
    try {
      const selected = await choose({ directory: true, multiple: false, title: "Pilih folder anime" });
      if (selected) await addFolder(selected);
    } catch (error) { setStatus(String(error), true); }
  });

  const scanButton = byId("scan-library");
  if (scanButton) scanButton.addEventListener("click", async () => {
    if (scanStatus) scanStatus.textContent = "Memindai ulang library…";
    const response = await fetch("/api/library/scan", { method: "POST" });
    if (!response.ok) return setStatus(await response.text(), true);
    if (!response.ok) return setStatus(await response.text(), true);
    await refreshScanStatus();
  });

  async function refreshScanStatus() {
    const response = await fetch("/api/library/status");
    if (!response.ok) return;
    const state = await response.json();
    renderScan(state);
  }

  function renderScan(state) {
    if (!scanStatus) return;
    const running = state.running === true || state.state === "running";
    if (running) {
      const total = Number(state.total || 0);
      const done = Number(state.done || 0);
      scanStatus.textContent = total
        ? `Memindai ${done} dari ${total} file… ${state.current || ""}`
        : "Menghitung file di folder library…";
      if (scanProgress) {
        scanProgress.hidden = false;
        scanProgress.value = total ? Math.min(100, done / total * 100) : 0;
      }
    } else {
      scanStatus.textContent = state.message || state.current || state.last_result || "Tidak ada pemindaian aktif.";
      if (scanProgress) scanProgress.hidden = true;
    }
  }

  if (scanStatus && window.EventSource) {
    const events = new EventSource("/api/library/events");
    events.onmessage = (message) => {
      try { renderScan(JSON.parse(message.data)); } catch (_) { /* ignore malformed local event */ }
    };
    refreshScanStatus().catch(() => {});
  }

  document.querySelectorAll("[data-pick]").forEach((button) => button.addEventListener("click", async () => {
    const key = button.dataset.pick;
    const filters = key === "mpv_path" ? [{ name: "mpv.exe", extensions: ["exe"] }] : [{ name: "Program", extensions: ["exe"] }];
    const file = await choose({ multiple: false, title: "Pilih program", filters });
    const input = byId(key.replaceAll("_", "-"));
    if (file && input) {
      input.value = file;
      await saveSettings();
    }
  }));

  ["player", "mpv-path", "ffmpeg-path", "ffprobe-path", "theme", "watch", "anilist"].forEach((id) => {
    const input = byId(id);
    if (input) input.addEventListener("change", () => saveSettings().catch((error) => setStatus(String(error), true)));
  });

  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (tauriEvent && typeof tauriEvent.listen === "function") {
    tauriEvent.listen("tauri://drag-drop", (event) => {
      const paths = event.payload && event.payload.paths || [];
      paths.forEach((path) => {
        const normalized = path.replace(/[\\/]+$/, "");
        if (/\.(mkv|mp4|webm|avi|m4v)$/i.test(normalized)) {
          const parent = normalized.replace(/[\\/][^\\/]+$/, "");
          if (parent) addFolder(parent).catch((error) => setStatus(String(error), true));
        } else {
          addFolder(normalized).catch((error) => setStatus(String(error), true));
        }
      });
    });
  }

  if (byId("player")) loadSettings().catch((error) => setStatus(String(error), true));
})();
