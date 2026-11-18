import init, { WasmStudioSession } from "./pkg/aweyuki_wasm.js";

const I18N = {
  vi: {
    openFile: "Mở File ROM",
    configBtn: "Cấu hình",
    deviceSection: "THIẾT BỊ ĐÍCH (SIMULATOR)",
    simProfileLbl: "Profile giả lập thiết bị",
    optionsSection: "CHIA NHỎ & CẤU HÌNH FLASH",
    maxDlLbl: "Giới hạn chia chunk (max-download-size)",
    slotLbl: "Slot đích",
    superStrategyLbl: "Chiến lược super.img",
    superWhole: "Nguyên khối",
    superUnpack: "Bung fastbootd",
    optSplit: "Tự động cắt Sparse (.simg chunks)",
    optVbmeta: "Patch vbmeta (--disable-verity)",
    optResize: "Resize logical & xoá -cow",
    optSideload: "Ưu tiên ADB Sideload",
    optWipe: "Xoá dữ liệu (Wipe -w)",
    optReboot: "Tự khởi động lại sau khi xong",
    flashNow: "Bắt đầu Flash ngay",
    flashShort: "Flash ngay",
    dryRun: "Chạy thử & Kiểm tra (Dry-Run)",
    statParts: "Phân vùng",
    statSteps: "Bước Flash",
    pipeDone: "Hoàn tất & Reboot",
    tabParts: "Phân vùng & Kế hoạch",
    tabSplit: "Chia nhỏ Phân vùng",
    tabScript: "Xuất Script",
    tabConsole: "Console",
    partListTitle: "Phân vùng tự động phát hiện",
    partListHint: "Kéo-thả file .zip, payload.bin, super.img hoặc .img vào bất kỳ đâu để phân tích tức thì",
    colPart: "Phân vùng",
    colFormat: "Định dạng / Magic",
    colSize: "Dung lượng",
    colStage: "Giai đoạn",
    colChunks: "Chunks",
    colActions: "Thao tác",
    planTitle: "TRÌNH TỰ LỆNH FLASH TỰ ĐỘNG",
    splitSelectLbl: "Phân vùng kiểm tra chia nhỏ",
    kpiRaw: "Dung lượng gốc",
    kpiMaxDl: "Giới hạn mỗi chunk",
    kpiChunks: "Số gói .simg xuất ra",
    kpiEncoded: "Tổng sau nén Sparse",
    copyBtn: "Sao chép",
    copiedBtn: "Đã chép!",
    downloadBtn: "Tải file script",
    consoleReady: "Sẵn sàng chạy giả lập hoặc Dry-Run",
    kpiStepsDone: "Bước hoàn thành",
    kpiChunksSent: "Gói đã truyền",
    kpiBytesSent: "Tổng dung lượng",
    stepsSuffix: "bước",
  },
  en: {
    openFile: "Open ROM File",
    configBtn: "Config",
    deviceSection: "TARGET DEVICE (SIMULATOR)",
    simProfileLbl: "Simulator Device Profile",
    optionsSection: "PARTITION SPLIT & FLASH OPTIONS",
    maxDlLbl: "Chunk Split Limit (max-download-size)",
    slotLbl: "Target Slot",
    superStrategyLbl: "super.img Strategy",
    superWhole: "Whole Sparse",
    superUnpack: "Unpack fastbootd",
    optSplit: "Auto Sparse Chunk Split (.simg)",
    optVbmeta: "Patch vbmeta (--disable-verity)",
    optResize: "Resize logicals & reclaim -cow",
    optSideload: "Force ADB Sideload",
    optWipe: "Wipe Userdata (-w)",
    optReboot: "Reboot when finished",
    flashNow: "Start Flash Now",
    flashShort: "Flash",
    dryRun: "Dry-Run Verification",
    statParts: "Partitions",
    statSteps: "Flash Steps",
    pipeDone: "Finalize & Reboot",
    tabParts: "Partitions & Plan",
    tabSplit: "Chunk & Super Splitter",
    tabScript: "Export Script",
    tabConsole: "Console",
    partListTitle: "Auto-Detected Partitions",
    partListHint: "Drag & drop any .zip, payload.bin, super.img or .img file anywhere to inspect instantly",
    colPart: "Partition",
    colFormat: "Format / Magic",
    colSize: "Size",
    colStage: "Stage",
    colChunks: "Chunks",
    colActions: "Actions",
    planTitle: "GENERATED FLASH PIPELINE",
    splitSelectLbl: "Inspect partition split layout",
    kpiRaw: "Raw Size",
    kpiMaxDl: "Chunk Limit",
    kpiChunks: "Output .simg Parts",
    kpiEncoded: "Total Sparse Size",
    copyBtn: "Copy",
    copiedBtn: "Copied!",
    downloadBtn: "Download Script",
    consoleReady: "Ready for simulated live flash or dry-run",
    kpiStepsDone: "Steps Completed",
    kpiChunksSent: "Chunks Transferred",
    kpiBytesSent: "Bytes Transferred",
    stepsSuffix: "steps",
  },
};

let session = null;
let state = null;
let lang = "vi";
let activeTab = "partitions";
let selectedSplitIdx = 0;
let activeScriptFmt = "sh";
let lastLogs = [
  "[Aweyuki] Ready. Loaded built-in Payload OTA sample + 256 KB OTG simulator.",
];
let lastReport = null;

const $ = (id) => document.getElementById(id);

function t(key) {
  return (I18N[lang] && I18N[lang][key]) || key;
}

function showToast(ok, msg) {
  const el = $("toast-banner");
  el.textContent = (ok ? "✓ " : "✕ ") + msg;
  el.className = "toast " + (ok ? "ok" : "err");
}

function applyLang() {
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const k = el.getAttribute("data-i18n");
    el.textContent = t(k);
  });
  $("lang-vi").classList.toggle("active", lang === "vi");
  $("lang-en").classList.toggle("active", lang === "en");
  if (state) renderAll();
}

function stageTagClass(stage) {
  switch (stage) {
    case "bootloader": return "tag-bootloader";
    case "transition": return "tag-transition";
    case "fastbootd":  return "tag-fastbootd";
    case "sideload":   return "tag-sideload";
    default:           return "tag-finalize";
  }
}

function renderAll() {
  if (!state) return;
  const { device, options, package: pkg, plan } = state;

  // Device status
  const modeCls =
    device.mode === "FASTBOOTD"
      ? "mode-fastbootd"
      : device.mode === "SIDELOAD"
      ? "mode-sideload"
      : "mode-bootloader";
  const ledCls =
    device.mode === "FASTBOOTD"
      ? "led-violet"
      : device.mode === "SIDELOAD"
      ? "led-amber"
      : "led-green";

  $("dev-mode-pill").textContent = device.mode;
  $("dev-mode-pill").className = "mode-badge " + modeCls;
  $("mob-dev-pill").textContent = device.mode;
  $("mob-dev-pill").className = "mode-badge " + modeCls;
  $("dev-led").className = "led " + ledCls;
  $("dev-product").textContent = device.product;
  $("mob-dev-name").textContent = `${device.product} • ${device.max_download_human}`;
  $("dev-sub").textContent = `${device.serial} • Slot ${(device.current_slot || "n/a").toUpperCase()} • ${device.max_download_human}`;
  $("sim-profile-select").value = device.profile_id;

  // Controls sync
  $("max-dl-select").value = String(options.max_download_size);
  $("slot-select").value = options.slot;
  $("super-whole").classList.toggle("active", options.super_mode === "whole");
  $("super-unpack").classList.toggle("active", options.super_mode === "unpack");
  $("opt-auto-split").checked = options.auto_sparse_split;
  $("opt-vbmeta").checked = options.disable_verity;
  $("opt-resize").checked = options.auto_resize_logical;
  $("opt-sideload").checked = options.force_sideload;
  $("opt-wipe").checked = options.wipe_userdata;
  $("opt-reboot").checked = options.reboot_after;

  if (!pkg || !plan) return;

  // Hero & Pipeline
  $("pkg-name").textContent = pkg.package_name;
  $("pkg-kind-pill").textContent = pkg.kind_label;
  $("pkg-size-pill").textContent = pkg.total_human;
  $("plan-maxdl-pill").textContent = `MaxDL: ${plan.effective_max_dl_human}`;

  if (pkg.codename_hint) {
    $("pkg-board-pill").textContent = `Board: ${pkg.codename_hint}`;
    $("pkg-board-pill").classList.remove("hidden");
  } else {
    $("pkg-board-pill").classList.add("hidden");
  }

  if (pkg.stripped_locks.length > 0) {
    $("pkg-shield-pill").textContent = `Safety: blocked ${pkg.stripped_locks.length} oem lock`;
    $("pkg-shield-pill").classList.remove("hidden");
  } else {
    $("pkg-shield-pill").classList.add("hidden");
  }

  if (pkg.warnings.length > 0) {
    $("pkg-warnings").innerHTML = pkg.warnings.map((w) => `<div>⚠ ${w}</div>`).join("");
    $("pkg-warnings").classList.remove("hidden");
  } else {
    $("pkg-warnings").classList.add("hidden");
  }

  $("stat-parts").textContent = String(pkg.partitions.length);
  $("stat-steps").textContent = String(plan.steps.length);
  $("tab-badge-parts").textContent = String(pkg.partitions.length);
  $("plan-step-count").textContent = `${plan.steps.length} ${t("stepsSuffix")}`;

  const sc = plan.stage_counts;
  $("pipe-boot-cnt").textContent = `${sc.bootloader} ${t("stepsSuffix")}`;
  $("pipe-fb-cnt").textContent = `${sc.transition + sc.fastbootd} ${t("stepsSuffix")}`;
  $("pipe-side-cnt").textContent = `${sc.sideload} ${t("stepsSuffix")}`;
  $("pipe-fin-cnt").textContent = `${sc.finalize} ${t("stepsSuffix")}`;

  document.querySelectorAll(".pipe-step").forEach((el) => {
    const st = el.getAttribute("data-stage");
    const cnt =
      st === "bootloader"
        ? sc.bootloader
        : st === "fastbootd"
        ? sc.transition + sc.fastbootd
        : st === "sideload"
        ? sc.sideload
        : sc.finalize;
    el.classList.toggle("active", cnt > 0);
    el.classList.toggle("dim", cnt === 0);
  });

  // Partitions Table
  const tbody = $("partitions-tbody");
  tbody.innerHTML = "";
  for (const p of pkg.partitions) {
    const tr = document.createElement("tr");
    const stagePillCls =
      p.stage === "fastbootd"
        ? "pill-violet"
        : p.stage === "sideload"
        ? "pill-amber"
        : "pill-green";
    const chunkBadge =
      p.est_chunks > 1
        ? `<span class="pill pill-amber mono">${p.est_chunks} chunks</span>`
        : `<span class="pill pill-green mono">1</span>`;
    tr.innerHTML = `
      <td>
        <label class="toggle-row" style="justify-content:center">
          <input type="checkbox" data-part-toggle="${p.index}" ${p.selected ? "checked" : ""}/>
          <span class="switch"></span>
        </label>
      </td>
      <td>
        <div class="part-name">${p.name}</div>
        <div class="part-notes">${p.notes}</div>
      </td>
      <td><span class="pill pill-info mono">${p.format}</span></td>
      <td class="num">${p.size_human}</td>
      <td><span class="pill ${stagePillCls}">${p.stage}</span></td>
      <td>${chunkBadge}</td>
      <td class="right">
        <div class="row-actions">
          ${p.can_unpack ? `<button class="btn btn-secondary btn-sm" data-unpack="${p.index}">Unpack</button>` : ""}
          <button class="btn btn-secondary btn-sm" data-split-view="${p.index}">Split</button>
        </div>
      </td>
    `;
    tbody.appendChild(tr);
  }

  // Timeline
  const tl = $("plan-timeline");
  tl.innerHTML = plan.steps
    .map(
      (s) => `
      <div class="step-row">
        <span class="step-num">${String(s.num).padStart(2, "0")}</span>
        <span class="step-tag ${stageTagClass(s.stage)}">${s.stage}</span>
        <span>${s.summary}</span>
      </div>`
    )
    .join("");

  // Split Selector & Super card
  const splitSel = $("split-part-select");
  if (selectedSplitIdx >= pkg.partitions.length) {
    selectedSplitIdx = pkg.partitions.reduce(
      (best, p, i, arr) => (p.size_bytes > arr[best].size_bytes ? i : best),
      0
    );
  }
  splitSel.innerHTML = pkg.partitions
    .map((p) => `<option value="${p.index}">${p.name} (${p.size_human})</option>`)
    .join("");
  splitSel.value = String(selectedSplitIdx);
  renderSplitPreview();
  renderSuperCard(pkg.super_info);
  renderScript();
  renderConsole();
}

function renderSplitPreview() {
  if (!session || !state?.package) return;
  try {
    const data = JSON.parse(session.get_split_preview(selectedSplitIdx));
    $("split-kpi-raw").textContent = data.raw_human;
    $("split-kpi-max").textContent = data.max_dl_human;
    $("split-kpi-count").textContent = data.error ? "—" : `${data.chunks.length}`;
    $("split-kpi-enc").textContent = data.error ? "—" : data.total_encoded_human;

    const bars = $("split-bars");
    if (data.error) {
      bars.innerHTML = `<div class="warnings-box">${data.error}</div>`;
      return;
    }
    bars.innerHTML = data.chunks
      .map((ch) => {
        const tot = Math.max(1, ch.total_blocks);
        const segsHtml = ch.segments
          .map((seg) => {
            const pct = ((seg.block_count / tot) * 100).toFixed(2);
            return `<div class="seg-block seg-${seg.kind}" style="width:${pct}%" title="${seg.kind}: blocks ${seg.start_block}..${seg.start_block + seg.block_count}"></div>`;
          })
          .join("");
        return `
          <div class="chunk-card">
            <div class="chunk-head">
              <span class="mono">Chunk ${ch.index + 1}/${ch.total_parts} • ${ch.encoded_human} (.simg)</span>
              <span class="mono" style="color:var(--text-muted)">blocks ${ch.start_block}..${ch.start_block + ch.active_blocks} / ${ch.total_blocks}</span>
            </div>
            <div class="chunk-track">${segsHtml}</div>
          </div>`;
      })
      .join("");
  } catch (e) {
    $("split-bars").innerHTML = `<div class="toast err">${String(e)}</div>`;
  }
}

function renderSuperCard(si) {
  const card = $("super-card");
  if (!si) {
    card.classList.add("hidden");
    return;
  }
  card.classList.remove("hidden");
  $("super-meta-pill").textContent = `${si.is_sparse ? "Sparse LP" : "Raw LP"} • ${si.raw_human}`;
  $("super-groups").innerHTML = si.groups
    .map((g) => {
      const pct = g.maximum_size > 0 ? Math.min(100, (g.used_bytes / g.maximum_size) * 100).toFixed(1) : "0";
      return `
        <div class="super-group-row">
          <div class="super-group-head">
            <span class="mono">${g.name}</span>
            <span class="mono" style="color:var(--text-muted)">${g.used_human} / ${g.maximum_human} (${pct}%)</span>
          </div>
          <div class="progress-track"><div class="progress-fill" style="width:${pct}%"></div></div>
        </div>`;
    })
    .join("");
  $("super-parts").innerHTML = si.partitions
    .map(
      (lp) => `
      <div class="super-part-chip mono">
        <span>${lp.name}</span>
        <span style="color:var(--cyan)">${lp.size_human}</span>
      </div>`
    )
    .join("");
}

function renderScript() {
  if (!session) return;
  const fname =
    activeScriptFmt === "bat"
      ? "flash_rom.bat"
      : activeScriptFmt === "termux"
      ? "flash_termux_otg.sh"
      : "flash_rom.sh";
  $("script-filename").textContent = fname;
  $("script-preview").textContent = session.get_script(activeScriptFmt);
}

function renderConsole() {
  const body = $("console-log-lines");
  body.innerHTML = lastLogs
    .map((line) => {
      const cls =
        line.includes("ERROR") || line.includes("Failed")
          ? "log-err"
          : line.includes("OKAY") || line.includes("Finished") || line.includes("patched")
          ? "log-ok"
          : line.includes("Split") || line.includes("Chunk")
          ? "log-split"
          : line.startsWith("===")
          ? "log-head"
          : "";
      return `<div class="log-line ${cls}">${escapeHtml(line)}</div>`;
    })
    .join("");
  body.scrollTop = body.scrollHeight;

  if (lastLogs.length > 1) {
    $("tab-badge-logs").textContent = String(lastLogs.length);
    $("tab-badge-logs").classList.remove("hidden");
  }
  if (lastReport) {
    $("console-kpis").classList.remove("hidden");
    $("con-kpi-steps").textContent = String(lastReport.steps_completed);
    $("con-kpi-chunks").textContent = String(lastReport.chunks_sent);
    $("con-kpi-bytes").textContent = lastReport.total_human;
    $("console-progress-bar").style.width = "100%";
    $("console-pct").textContent = "100%";
    $("console-status-text").textContent = lastReport.dry_run
      ? "Dry-Run Verification Completed"
      : "Flash Completed Successfully";
  }
}

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function switchTab(tabName) {
  activeTab = tabName;
  document.querySelectorAll(".tab-btn").forEach((b) => {
    b.classList.toggle("active", b.getAttribute("data-tab") === tabName);
  });
  document.querySelectorAll(".tab-pane").forEach((p) => {
    p.classList.toggle("active", p.id === `tab-${tabName}`);
  });
}

function pushOptionsPatch(patch) {
  if (!session) return;
  state = JSON.parse(session.set_options_json(JSON.stringify(patch)));
  renderAll();
}

function triggerDownload(filename, dataBytesOrStr) {
  const blob =
    typeof dataBytesOrStr === "string"
      ? new Blob([dataBytesOrStr], { type: "text/plain;charset=utf-8" })
      : new Blob([dataBytesOrStr], { type: "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function runFlashAction(dryRun) {
  if (!session) return;
  try {
    const res = JSON.parse(session.run_flash(dryRun));
    lastLogs = res.logs;
    lastReport = res.report;
    state = res.state_after;
    showToast(
      true,
      `${dryRun ? "Dry-Run OK" : "Flash OK"}: ${res.report.chunks_sent} chunks (${res.report.total_human})`
    );
    switchTab("console");
    renderAll();
  } catch (e) {
    lastLogs = [`[ERROR] ${String(e)}`];
    showToast(false, String(e));
    switchTab("console");
    renderConsole();
  }
}

async function handleFile(file) {
  if (!file || !session) return;
  try {
    const buf = await file.arrayBuffer();
    state = JSON.parse(session.load_rom_bytes(file.name, new Uint8Array(buf)));
    selectedSplitIdx = state.package.partitions.reduce(
      (best, p, i, arr) => (p.size_bytes > arr[best].size_bytes ? i : best),
      0
    );
    lastLogs.push(`[Detect] Loaded ${file.name} -> ${state.package.kind_label}`);
    showToast(true, `Loaded ${file.name}`);
    renderAll();
  } catch (e) {
    showToast(false, `Failed to inspect ${file.name}: ${String(e)}`);
  }
}

async function bootstrap() {
  await init();
  session = new WasmStudioSession();
  state = JSON.parse(session.get_state());
  selectedSplitIdx = state.package.partitions.reduce(
    (best, p, i, arr) => (p.size_bytes > arr[best].size_bytes ? i : best),
    0
  );
  applyLang();
  showToast(true, `Loaded ${state.package.package_name}`);

  // Event bindings
  $("lang-vi").addEventListener("click", () => { lang = "vi"; applyLang(); });
  $("lang-en").addEventListener("click", () => { lang = "en"; applyLang(); });
  $("btn-toggle-drawer").addEventListener("click", () => {
    $("inspector-panel").classList.toggle("drawer-open");
  });

  $("btn-open-file").addEventListener("click", () => $("rom-file-input").click());
  $("rom-file-input").addEventListener("change", (ev) => {
    const f = ev.target.files?.[0];
    if (f) handleFile(f);
    ev.target.value = "";
  });

  // Drag & Drop
  window.addEventListener("dragover", (e) => {
    e.preventDefault();
    $("drop-overlay").classList.remove("hidden");
  });
  window.addEventListener("dragleave", (e) => {
    if (e.relatedTarget === null) $("drop-overlay").classList.add("hidden");
  });
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    $("drop-overlay").classList.add("hidden");
    const f = e.dataTransfer?.files?.[0];
    if (f) handleFile(f);
  });

  $("sample-select").addEventListener("change", (e) => {
    state = JSON.parse(session.load_sample(e.target.value));
    selectedSplitIdx = state.package.partitions.reduce(
      (best, p, i, arr) => (p.size_bytes > arr[best].size_bytes ? i : best),
      0
    );
    showToast(true, `Loaded ${state.package.package_name}`);
    renderAll();
  });

  $("sim-profile-select").addEventListener("change", (e) => {
    state = JSON.parse(session.set_sim_profile(e.target.value));
    renderAll();
  });

  $("max-dl-select").addEventListener("change", (e) =>
    pushOptionsPatch({ max_download_size: Number(e.target.value) })
  );
  $("slot-select").addEventListener("change", (e) =>
    pushOptionsPatch({ slot: e.target.value })
  );
  $("super-whole").addEventListener("click", () => pushOptionsPatch({ super_mode: "whole" }));
  $("super-unpack").addEventListener("click", () => pushOptionsPatch({ super_mode: "unpack" }));
  $("opt-auto-split").addEventListener("change", (e) =>
    pushOptionsPatch({ auto_sparse_split: e.target.checked })
  );
  $("opt-vbmeta").addEventListener("change", (e) =>
    pushOptionsPatch({ disable_verity: e.target.checked })
  );
  $("opt-resize").addEventListener("change", (e) =>
    pushOptionsPatch({ auto_resize_logical: e.target.checked })
  );
  $("opt-sideload").addEventListener("change", (e) =>
    pushOptionsPatch({ force_sideload: e.target.checked })
  );
  $("opt-wipe").addEventListener("change", (e) =>
    pushOptionsPatch({ wipe_userdata: e.target.checked })
  );
  $("opt-reboot").addEventListener("change", (e) =>
    pushOptionsPatch({ reboot_after: e.target.checked })
  );

  $("btn-flash").addEventListener("click", () => runFlashAction(false));
  $("btn-dryrun").addEventListener("click", () => runFlashAction(true));
  $("mob-btn-flash").addEventListener("click", () => runFlashAction(false));
  $("mob-btn-dryrun").addEventListener("click", () => runFlashAction(true));

  document.querySelectorAll(".tab-btn").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.getAttribute("data-tab")));
  });

  $("partitions-tbody").addEventListener("change", (e) => {
    const idx = e.target.getAttribute("data-part-toggle");
    if (idx !== null) {
      state = JSON.parse(session.set_partition_selected(Number(idx), e.target.checked));
      renderAll();
    }
  });

  $("partitions-tbody").addEventListener("click", (e) => {
    const unpackIdx = e.target.getAttribute("data-unpack");
    if (unpackIdx !== null) {
      const idx = Number(unpackIdx);
      const part = state.package.partitions[idx];
      const bytes = session.extract_partition(idx);
      triggerDownload(`${part.name}.img`, bytes);
      showToast(true, `Extracted ${part.name}.img (${part.size_human})`);
      return;
    }
    const splitIdx = e.target.getAttribute("data-split-view");
    if (splitIdx !== null) {
      selectedSplitIdx = Number(splitIdx);
      switchTab("splitter");
      renderAll();
    }
  });

  $("split-part-select").addEventListener("change", (e) => {
    selectedSplitIdx = Number(e.target.value);
    renderSplitPreview();
  });

  document.querySelectorAll("[data-script]").forEach((btn) => {
    btn.addEventListener("click", () => {
      activeScriptFmt = btn.getAttribute("data-script");
      document
        .querySelectorAll("[data-script]")
        .forEach((b) => b.classList.toggle("active", b === btn));
      renderScript();
    });
  });

  $("btn-copy-script").addEventListener("click", async () => {
    const txt = $("script-preview").textContent;
    await navigator.clipboard?.writeText(txt);
    $("btn-copy-script").textContent = t("copiedBtn");
    setTimeout(() => ($("btn-copy-script").textContent = t("copyBtn")), 1400);
  });

  $("btn-download-script").addEventListener("click", () => {
    triggerDownload($("script-filename").textContent, $("script-preview").textContent);
  });
}

bootstrap().catch((err) => {
  console.error(err);
  showToast(false, String(err));
});
