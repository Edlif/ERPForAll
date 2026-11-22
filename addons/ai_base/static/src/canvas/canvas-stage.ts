/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

import { icon } from "@erpforall/client";
import {
  renderBarChart,
  renderDonutChart,
  renderLineChart,
  normalizeSeries,
  type ChartPoint,
} from "./canvas-chart.js";
import { renderRichTable, type TableSpec } from "./canvas-table.js";
import { renderWorkflowFlow } from "./canvas-flow.js";
import { renderSimulation } from "./canvas-simulation.js";
import { renderPivotTable, type PivotSpec } from "./canvas-pivot.js";
import { slides } from "../presentation.js";

export type CanvasArtifact = {
  canvas_type: "chart" | "table" | "pivot" | "flow" | "dashboard" | "simulation" | "slides";
  title?: string;
  description?: string;
  data: unknown;
};

export type CanvasStageHandle = {
  node: HTMLElement;
  renderArtifact(artifact: unknown): void;
  detectAndRender(text: string): boolean;
  clear(): void;
  dispose(): void;
};

type ViewMode = "bar" | "line" | "donut" | "pivot" | "table";

function createDragHandle(): HTMLElement {
  const span = document.createElement("span");
  span.className = "canvas-drag-handle";
  span.title = "Tarik untuk mengatur urutan kartu";
  span.setAttribute("aria-label", "Pindahkan kartu");

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", "16");
  svg.setAttribute("height", "16");
  svg.setAttribute("fill", "currentColor");
  svg.setAttribute("aria-hidden", "true");

  for (const [cx, cy] of [
    [9, 6],
    [15, 6],
    [9, 12],
    [15, 12],
    [9, 18],
    [15, 18],
  ]) {
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(cx));
    circle.setAttribute("cy", String(cy));
    circle.setAttribute("r", "1.5");
    svg.append(circle);
  }

  span.append(svg);
  return span;
}

export function createCanvasStage(onPromptSelect?: (prompt: string) => void): CanvasStageHandle {
  const container = document.createElement("section");
  container.className = "ai-workspace-canvas-stage";
  container.setAttribute("aria-label", "AI Canvas");

  const header = document.createElement("div");
  header.className = "canvas-stage-header";

  const titleGroup = document.createElement("div");
  titleGroup.className = "canvas-stage-title-group";

  const stageTitle = document.createElement("h2");
  stageTitle.className = "canvas-stage-title";
  stageTitle.textContent = "Canvas";

  const stageSubtitle = document.createElement("p");
  stageSubtitle.className = "canvas-stage-subtitle";
  stageSubtitle.textContent = "Analisis dan visualisasi dari data perusahaan";

  titleGroup.append(stageTitle, stageSubtitle);

  const actions = document.createElement("div");
  actions.className = "canvas-stage-actions";

  const status = document.createElement("span");
  status.className = "canvas-stage-status";
  status.textContent = "Terhubung ke data perusahaan";

  const clearBtn = document.createElement("button");
  clearBtn.type = "button";
  clearBtn.className = "erp-button erp-button-secondary canvas-btn-clear";
  clearBtn.textContent = "Bersihkan";
  clearBtn.hidden = true;

  actions.append(status, clearBtn);
  header.append(titleGroup, actions);

  // Viewport
  const viewport = document.createElement("div");
  viewport.className = "canvas-stage-viewport";

  // Blank placeholder
  const blankState = document.createElement("div");
  blankState.className = "canvas-blank-state";

  const blankTitle = document.createElement("p");
  blankTitle.className = "canvas-blank-title";
  blankTitle.textContent = "Tanyakan analisis bisnis, hasilnya muncul di sini.";

  const blankDesc = document.createElement("p");
  blankDesc.className = "canvas-blank-desc";
  blankDesc.textContent =
    "Pilih analisis siap pakai di bawah ini atau ajukan pertanyaan lewat AI Assistant untuk grafik, tabel dan pivot dari data perusahaan.";

  const suggestionList = document.createElement("div");
  suggestionList.className = "canvas-suggestion-list";

  const suggestions = [
    { label: "5 customer terloyal", prompt: "Siapa 5 customer terloyal tahun ini? Tampilkan grafiknya di canvas." },
    { label: "Pivot omzet & status", prompt: "Tampilkan pivot table omzet penjualan per customer dan status pesanan." },
    { label: "Tren omzet bulanan", prompt: "Tampilkan omzet penjualan bulanan tahun ini dalam bentuk chart." },
    { label: "Piutang jatuh tempo", prompt: "Tampilkan tabel ringkasan faktur piutang yang jatuh tempo." },
    { label: "Alur siklus pesanan", prompt: "Gambarkan alur proses dari penawaran harga hingga faktur terbayar." },
  ];

  suggestions.forEach(item => {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "canvas-suggestion-chip";
    chip.textContent = item.label;
    chip.addEventListener("click", () => {
      if (onPromptSelect) onPromptSelect(item.prompt);
    });
    suggestionList.append(chip);
  });

  blankState.append(blankTitle, blankDesc, suggestionList);

  // Cards container for generated dashboard widgets
  const cardsGrid = document.createElement("div");
  cardsGrid.className = "canvas-cards-grid";

  viewport.append(blankState);
  container.append(header, viewport);

  let draggedCard: HTMLElement | null = null;

  function setupCardDragAndDrop(card: HTMLElement) {
    card.setAttribute("draggable", "true");

    card.addEventListener("dragstart", (e: DragEvent) => {
      draggedCard = card;
      card.classList.add("is-dragging");
      if (e.dataTransfer) {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", card.dataset["cardId"] || "");
      }
    });

    card.addEventListener("dragend", () => {
      card.classList.remove("is-dragging");
      draggedCard = null;
      cardsGrid.querySelectorAll(".canvas-card-item").forEach(c => {
        c.classList.remove("drag-over-above", "drag-over-below");
      });
    });

    card.addEventListener("dragover", (e: DragEvent) => {
      e.preventDefault();
      if (!draggedCard || draggedCard === card) return;
      if (e.dataTransfer) {
        e.dataTransfer.dropEffect = "move";
      }
      const rect = card.getBoundingClientRect();
      const midY = rect.top + rect.height / 2;
      if (e.clientY < midY) {
        card.classList.add("drag-over-above");
        card.classList.remove("drag-over-below");
      } else {
        card.classList.add("drag-over-below");
        card.classList.remove("drag-over-above");
      }
    });

    card.addEventListener("dragleave", () => {
      card.classList.remove("drag-over-above", "drag-over-below");
    });

    card.addEventListener("drop", (e: DragEvent) => {
      e.preventDefault();
      card.classList.remove("drag-over-above", "drag-over-below");
      if (!draggedCard || draggedCard === card) return;

      const rect = card.getBoundingClientRect();
      const midY = rect.top + rect.height / 2;
      if (e.clientY < midY) {
        cardsGrid.insertBefore(draggedCard, card);
      } else {
        cardsGrid.insertBefore(draggedCard, card.nextSibling);
      }
    });
  }

  function clear() {
    cardsGrid.replaceChildren();
    viewport.replaceChildren(blankState);
    stageTitle.textContent = "Canvas";
    stageSubtitle.textContent = "Analisis dan visualisasi dari data perusahaan";
    clearBtn.hidden = true;
  }

  clearBtn.addEventListener("click", clear);

  function renderViewModeContent(
    mode: ViewMode,
    title: string,
    series: ChartPoint[],
    unit: string | undefined,
    tableSpec: TableSpec | undefined,
    pivotSpec: PivotSpec | undefined
  ): HTMLElement {
    switch (mode) {
      case "bar":
        return renderBarChart({ title, series, unit });
      case "line":
        return renderLineChart({ title, series, unit });
      case "donut":
        return renderDonutChart({ title, series, unit });
      case "pivot":
        if (pivotSpec) {
          return renderPivotTable(pivotSpec);
        }
        return renderPivotTable({
          title,
          dimensions: ["Kategori"],
          measures: [{ key: "value", label: unit ? `Nilai (${unit})` : "Nilai", agg: "sum" }],
          records: series.map(s => ({ Kategori: s.label, value: s.value })),
        });
      case "table":
        if (tableSpec) {
          return renderRichTable(tableSpec);
        }
        return renderRichTable({
          title,
          columns: ["Kategori", unit ? `Nilai (${unit})` : "Nilai"],
          rows: series.map(s => [s.label, s.value]),
        });
    }
  }

  function buildCard(
    canvasType: string,
    title: string,
    description: string,
    data: Record<string, unknown>
  ): HTMLElement {
    const card = document.createElement("article");
    card.className = "canvas-card-item canvas-rendered-card";
    card.dataset["cardId"] = `card-${Math.random().toString(36).substring(2, 9)}`;

    // Prepare normalized data
    let initialMode: ViewMode = "bar";
    if (canvasType === "pivot") initialMode = "pivot";
    else if (canvasType === "table") initialMode = "table";
    else if (canvasType === "chart") {
      const ct = String(data["type"] || "bar");
      if (ct === "donut" || ct === "pie") initialMode = "donut";
      else if (ct === "line") initialMode = "line";
      else initialMode = "bar";
    }

    const unit = typeof data["unit"] === "string" ? data["unit"] : undefined;
    let series: ChartPoint[] = [];
    if (Array.isArray(data["series"]) || Array.isArray(data["data"])) {
      series = normalizeSeries(data["series"] || data["data"]);
    } else if (Array.isArray(data)) {
      series = normalizeSeries(data);
    }

    let tableSpec: TableSpec | undefined;
    if (Array.isArray(data["columns"]) || Array.isArray(data["rows"]) || Array.isArray(data["records"])) {
      tableSpec = {
        title,
        columns: Array.isArray(data["columns"]) ? (data["columns"] as any[]) : undefined,
        rows: Array.isArray(data["rows"]) ? (data["rows"] as any[]) : undefined,
        records: Array.isArray(data["records"]) ? (data["records"] as any[]) : undefined,
      };
      if (!series.length && Array.isArray(data["rows"]) && data["rows"].length) {
        series = normalizeSeries(data["rows"]);
      } else if (!series.length && Array.isArray(data["records"]) && data["records"].length) {
        series = normalizeSeries(data["records"]);
      }
    }

    let pivotSpec: PivotSpec | undefined;
    if (canvasType === "pivot" || Array.isArray(data["dimensions"]) || Array.isArray(data["records"])) {
      const dims = (data["dimensions"] || data["dimension"] || data["groupby"] || data["group_by"]) as
        | string[]
        | undefined;
      const measures = data["measures"] as any[] | undefined;
      const records = (data["records"] || data["groups"] || (Array.isArray(data) ? data : undefined)) as
        | Record<string, unknown>[]
        | undefined;
      pivotSpec = {
        title,
        dimensions: Array.isArray(dims) ? dims : undefined,
        measures: Array.isArray(measures) ? measures : undefined,
        records: Array.isArray(records) ? records : undefined,
        columns: Array.isArray(data["columns"]) ? (data["columns"] as string[]) : undefined,
        rows: Array.isArray(data["rows"]) ? (data["rows"] as any[][]) : undefined,
      };
      if (!series.length && Array.isArray(records) && records.length) {
        series = normalizeSeries(records);
      }
    }

    // Header
    const cardHeader = document.createElement("div");
    cardHeader.className = "canvas-card-header";

    const titleArea = document.createElement("div");
    titleArea.className = "canvas-card-title-area";

    const dragHandle = createDragHandle();
    const cardHeading = document.createElement("h3");
    cardHeading.className = "canvas-card-title";
    cardHeading.textContent = title;

    titleArea.append(dragHandle, cardHeading);

    // View Switcher (Bar, Line, Donut, Pivot, Table)
    const switcher = document.createElement("nav");
    switcher.className = "canvas-view-switcher";
    switcher.setAttribute("role", "tablist");
    switcher.setAttribute("aria-label", "Tampilan Visual");

    const modes: { id: ViewMode; label: string; iconName: "chart-column" | "chart-line" | "chart-pie" | "layout-grid" | "list" }[] = [
      { id: "bar", label: "Batang", iconName: "chart-column" },
      { id: "line", label: "Tren", iconName: "chart-line" },
      { id: "donut", label: "Donut", iconName: "chart-pie" },
      { id: "pivot", label: "Pivot", iconName: "layout-grid" },
      { id: "table", label: "Tabel", iconName: "list" },
    ];

    let currentMode = initialMode;
    const body = document.createElement("div");
    body.className = "canvas-card-body";

    function updateView(mode: ViewMode) {
      currentMode = mode;
      switcher.querySelectorAll<HTMLButtonElement>(".canvas-btn-switch").forEach(btn => {
        const selected = btn.dataset["mode"] === mode;
        btn.classList.toggle("active", selected);
        btn.setAttribute("aria-selected", String(selected));
        btn.tabIndex = selected ? 0 : -1;
      });
      body.replaceChildren(renderViewModeContent(mode, title, series, unit, tableSpec, pivotSpec));
    }

    const switchButtons: HTMLButtonElement[] = [];
    modes.forEach(m => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `canvas-btn-switch ${m.id === currentMode ? "active" : ""}`;
      btn.dataset["mode"] = m.id;
      btn.setAttribute("role", "tab");
      btn.setAttribute("aria-selected", String(m.id === currentMode));
      btn.tabIndex = m.id === currentMode ? 0 : -1;
      btn.append(icon(m.iconName), document.createTextNode(` ${m.label}`));
      btn.addEventListener("click", () => updateView(m.id));
      switchButtons.push(btn);
      switcher.append(btn);
    });
    // Roving tabindex with arrow-key navigation, like a tab list.
    switcher.addEventListener("keydown", event => {
      if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const visible = switchButtons.filter(btn => !btn.hidden);
      const index = visible.findIndex(btn => btn.dataset["mode"] === currentMode);
      let next = index;
      if (event.key === "ArrowLeft") next = (index - 1 + visible.length) % visible.length;
      else if (event.key === "ArrowRight") next = (index + 1) % visible.length;
      else if (event.key === "Home") next = 0;
      else if (event.key === "End") next = visible.length - 1;
      const target = visible[next];
      if (target) {
        updateView(target.dataset["mode"] as ViewMode);
        target.focus();
      }
    });

    // Actions (close button)
    const cardActions = document.createElement("div");
    cardActions.className = "canvas-card-actions";

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "canvas-card-btn-close";
    closeBtn.title = "Tutup kartu visualisasi ini";
    closeBtn.setAttribute("aria-label", "Tutup kartu");
    closeBtn.append(icon("x"));
    closeBtn.addEventListener("click", () => {
      card.remove();
      if (!cardsGrid.children.length) {
        clear();
      }
    });

    cardActions.append(closeBtn);
    cardHeader.append(titleArea, switcher, cardActions);

    // Initial Body Render
    if (canvasType === "slides") {
      const slideEl = slides(data);
      if (slideEl) body.append(slideEl);
      switcher.hidden = true;
    } else if (canvasType === "flow") {
      const steps = (data["steps"] as any[]) || [];
      body.append(renderWorkflowFlow({ title, steps }));
      switcher.hidden = true;
    } else if (canvasType === "simulation") {
      const variables = (data["variables"] as any[]) || [];
      const baseRev = Number(data["baseRevenue"]);
      const baseMargin = Number(data["baseMarginPct"]);
      body.append(
        renderSimulation({
          title,
          description,
          variables,
          baseRevenue: Number.isFinite(baseRev) ? baseRev : 0,
          baseMarginPct: Number.isFinite(baseMargin) ? baseMargin : 0,
        })
      );
      switcher.hidden = true;
    } else {
      body.append(renderViewModeContent(currentMode, title, series, unit, tableSpec, pivotSpec));
    }

    card.append(cardHeader, body);

    // Contextual follow-up questions
    const lowerTitle = title.toLowerCase();
    let followups = [
      "Tampilkan rincian tabel lengkap",
      "Bandingkan dengan performa periode lalu",
      "Ekspor laporan ini ke format CSV",
    ];

    if (lowerTitle.includes("customer") || lowerTitle.includes("loyal")) {
      followups = [
        "Bandingkan 5 customer ini dengan tahun lalu",
        "Apa 5 produk terlaris dari pelanggan teratas?",
        "Tampilkan faktur yang belum lunas dari customer ini",
      ];
    } else if (lowerTitle.includes("piutang") || lowerTitle.includes("faktur")) {
      followups = [
        "Siapa customer dengan piutang tertua?",
        "Tampilkan faktur jatuh tempo dalam 7 hari ke depan",
        "Kirim pesan follow-up penagihan ke pelanggan",
      ];
    } else if (lowerTitle.includes("omzet") || lowerTitle.includes("tren")) {
      followups = [
        "Breakdown omzet per kategori produk",
        "Bandingkan performa Q1 vs Q2",
        "Tampilkan rasio pertumbuhan bulanan (MoM)",
      ];
    }

    const followupsSection = document.createElement("div");
    followupsSection.className = "canvas-card-followups";

    const followupsTitle = document.createElement("span");
    followupsTitle.className = "canvas-followups-label";
    followupsTitle.textContent = "Pertanyaan lanjutan";

    const followupsChips = document.createElement("div");
    followupsChips.className = "canvas-followups-chips";

    followups.forEach(promptText => {
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "canvas-followup-chip";
      chip.textContent = promptText;
      chip.addEventListener("click", () => {
        if (onPromptSelect) onPromptSelect(promptText);
      });
      followupsChips.append(chip);
    });

    followupsSection.append(followupsTitle, followupsChips);
    card.append(followupsSection);

    setupCardDragAndDrop(card);
    return card;
  }

  function renderArtifact(raw: unknown): void {
    if (typeof raw !== "object" || raw === null) return;
    const obj = raw as Record<string, unknown>;
    const canvasType = String(obj["canvas_type"] || obj["type"] || "chart");
    const title = typeof obj["title"] === "string" ? obj["title"] : "Visualisasi";
    const description = typeof obj["description"] === "string" ? obj["description"] : "";
    const data = (obj["data"] ?? obj) as Record<string, unknown>;

    stageTitle.textContent = title;
    stageSubtitle.textContent = description || `Tipe visual: ${canvasType}`;
    clearBtn.hidden = false;

    if (viewport.contains(blankState)) {
      viewport.replaceChildren(cardsGrid);
    }

    try {
      const card = buildCard(canvasType, title, description, data);
      cardsGrid.prepend(card);
    } catch (renderError) {
      console.error("Canvas artifact rendering error:", renderError);
      const errCard = document.createElement("div");
      errCard.className = "canvas-render-error canvas-rendered-card";
      const errTitle = document.createElement("strong");
      errTitle.textContent = "Gagal memvisualisasikan data pada canvas";
      const errDesc = document.createElement("p");
      errDesc.textContent = renderError instanceof Error ? renderError.message : "Format data tidak sesuai.";
      errCard.append(errTitle, errDesc);
      cardsGrid.prepend(errCard);
    }
  }

  function detectAndRender(text: string): boolean {
    const match = text.match(/```(?:erp-view|json)\s*\n([\s\S]*?)\n```/);
    if (!match || !match[1]) return false;
    try {
      const parsed = JSON.parse(match[1]);
      renderArtifact(parsed);
      return true;
    } catch (err) {
      console.warn("AI Canvas artifact JSON parse failed:", err);
      if (viewport.contains(blankState)) {
        viewport.replaceChildren(cardsGrid);
      }
      const errCard = document.createElement("div");
      errCard.className = "canvas-render-error canvas-rendered-card";
      const errTitle = document.createElement("strong");
      errTitle.textContent = "Format Visualisasi Tidak Valid";
      const errDesc = document.createElement("p");
      errDesc.textContent = "Gagal memproses artifact visual: Format data JSON tidak terbaca.";
      errCard.append(errTitle, errDesc);
      cardsGrid.prepend(errCard);
      clearBtn.hidden = false;
      return false;
    }
  }

  return {
    node: container,
    renderArtifact,
    detectAndRender,
    clear,
    dispose() {
      container.remove();
    },
  };
}
