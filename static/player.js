/* Fets Anime player: HTML5 direct play -> HLS (hls.js) -> automatic mpv fallback.
   All logic is local; no external requests are made. */
(function () {
  "use strict";
  const video = document.getElementById("player");
  if (!video) return;
  const episodeId = Number(video.dataset.episode);
  const mode = video.dataset.mode || "auto"; // auto | html5 | mpv
  const errorBox = document.getElementById("player-error");
  let hls = null;
  let fellBack = false;
  let loadTimer = null;

  function toast(message) {
    let el = document.getElementById("toast");
    if (!el) {
      el = document.createElement("div");
      el.id = "toast";
      document.body.appendChild(el);
    }
    el.textContent = message;
    el.classList.add("show");
    setTimeout(() => el.classList.remove("show"), 2600);
  }

  async function saveProgress(watched) {
    try {
      await fetch("/api/progress/" + episodeId, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          position_sec: video.currentTime || 0,
          watched: Boolean(watched),
        }),
      });
    } catch (_) { /* offline loopback should not happen; ignore */ }
  }

  async function fallbackToMpv(reason) {
    if (fellBack) return;
    fellBack = true;
    if (loadTimer) clearTimeout(loadTimer);
    toast("Beralih ke mpv…");
    try {
      const r = await fetch("/api/mpv/play", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          episode_id: episodeId,
          position_sec: Math.max(0, video.currentTime || 0),
          reason: String(reason).slice(0, 120),
        }),
      });
      if (!r.ok) {
        const detail = await r.text();
        toast(detail || "mpv tidak tersedia");
        if (errorBox) errorBox.textContent = detail;
        fellBack = false; // allow retry from the manual button
      } else {
        if (errorBox) errorBox.textContent = "Sedang diputar di mpv. Progres tersimpan otomatis.";
        window.__fetsMpvActive = true;
      }
    } catch (e) {
      toast("Gagal menghubungi mpv: " + e);
      fellBack = false;
    }
  }
  window.fetsPlayWithMpv = () => fallbackToMpv("manual");

  if (mode === "mpv") {
    fallbackToMpv("default-player");
    return;
  }

  function attachErrorHandlers() {
    video.addEventListener("error", () => {
      if (mode === "html5") {
        if (errorBox) errorBox.textContent = "Format ini tidak didukung browser.";
        return;
      }
      fallbackToMpv("html5-error");
    });
    // Stall watchdog: if playback never starts while loading, switch to mpv.
    video.addEventListener("loadstart", () => {
      if (mode !== "auto") return;
      if (loadTimer) clearTimeout(loadTimer);
      loadTimer = setTimeout(() => {
        if (!fellBack && video.readyState < 2 && !window.__fetsMpvActive) {
          fallbackToMpv("stall");
        }
      }, 8000);
    });
    ["loadeddata", "canplay"].forEach((ev) =>
      video.addEventListener(ev, () => {
        if (loadTimer) clearTimeout(loadTimer);
      })
    );
  }

  function startHls(src) {
    if (video.canPlayType("application/vnd.apple.mpegurl")) {
      video.src = src;
      return;
    }
    if (window.Hls && Hls.isSupported()) {
      hls = new Hls({ maxBufferLength: 30 });
      hls.loadSource(src);
      hls.attachMedia(video);
      hls.on(Hls.Events.ERROR, (_e, data) => {
        if (data.fatal) {
          if (mode === "html5") {
            if (errorBox) errorBox.textContent = "HLS gagal diputar.";
          } else {
            fallbackToMpv("hls-error");
          }
        }
      });
    } else {
      if (mode === "html5") {
        if (errorBox) errorBox.textContent = "Browser ini tidak mendukung HLS.";
      } else {
        fallbackToMpv("hls-unsupported");
      }
    }
  }

  attachErrorHandlers();

  // Resume position + periodic saving.
  video.addEventListener("loadedmetadata", async () => {
    try {
      const r = await fetch("/api/progress/" + episodeId);
      if (r.ok) {
        const p = await r.json();
        if (p.position_sec > 0 && video.duration > 0 && p.position_sec < video.duration * 0.9) {
          video.currentTime = p.position_sec;
        }
      }
    } catch (_) {}
  }, { once: true });
  setInterval(() => {
    if (!video.paused && !video.ended && !window.__fetsMpvActive) saveProgress(false);
  }, 10000);
  video.addEventListener("pause", () => saveProgress(false));
  video.addEventListener("ended", () => saveProgress(true));

  // Keyboard shortcuts: space, arrows, F fullscreen, N next episode.
  document.addEventListener("keydown", (e) => {
    if (e.target.matches("input, select, textarea")) return;
    switch (e.key) {
      case " ":
        e.preventDefault();
        video.paused ? video.play() : video.pause();
        break;
      case "ArrowRight": video.currentTime += 5; break;
      case "ArrowLeft": video.currentTime -= 5; break;
      case "ArrowUp": video.volume = Math.min(1, video.volume + 0.05); break;
      case "ArrowDown": video.volume = Math.max(0, video.volume - 0.05); break;
      case "f": case "F":
        document.fullscreenElement ? document.exitFullscreen() : video.requestFullscreen();
        break;
      case "n": case "N": {
        const next = document.getElementById("next");
        if (next) location.href = next.getAttribute("href");
        break;
      }
    }
  });

  // Subtitle selector drives native text tracks.
  const subSelect = document.getElementById("subtitle-select");
  if (subSelect) {
    subSelect.addEventListener("change", () => {
      const value = subSelect.value;
      for (const track of video.textTracks) track.mode = "disabled";
      if (value !== "") {
        const index = Number(value);
        if (video.textTracks[index]) video.textTracks[index].mode = "showing";
      }
    });
  }

  const src = video.dataset.src;
  if (src.endsWith(".m3u8")) startHls(src);
  else video.src = src;
})();
