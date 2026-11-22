/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

import { renderBarChart, renderDonutChart, type ChartSpec } from "./canvas-chart.js";
import { renderRichTable, type TableSpec } from "./canvas-table.js";
import { renderWorkflowFlow, type FlowSpec } from "./canvas-flow.js";
import { renderSimulation, type SimulationSpec } from "./canvas-simulation.js";
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

export function createCanvasStage(onPromptSelect?: (prompt: string) => void): CanvasStageHandle {
  const container = document.createElement("section");
  container.className = "ai-workspace-canvas-stage";
  container.setAttribute("aria-label", "AI Dynamic Canvas");

  // Top header bar of the canvas
  const header = document.createElement("div");
  header.className = "canvas-stage-header";

  const titleGroup = document.createElement("div");
  titleGroup.className = "canvas-stage-title-group";

  const stageTitle = document.createElement("h2");
  stageTitle.className = "canvas-stage-title";
  stageTitle.textContent = "AI Workspace Canvas";

  const stageSubtitle = document.createElement("p");
  stageSubtitle.className = "canvas-stage-subtitle";
  stageSubtitle.textContent = "Visualisasi dinamis, grafik interaktif, dan simulasi proses bisnis";

  titleGroup.append(stageTitle, stageSubtitle);

  const actions = document.createElement("div");
  actions.className = "canvas-stage-actions";

  const liveBadge = document.createElement("div");
  liveBadge.className = "canvas-live-badge";
  const liveDot = document.createElement("span");
  liveDot.className = "canvas-live-dot";
  const liveText = document.createElement("span");
  liveText.textContent = "Live Data Connected";
  liveBadge.append(liveDot, liveText);

  const clearBtn = document.createElement("button");
  clearBtn.type = "button";
  clearBtn.className = "erp-button erp-button-secondary canvas-btn-clear";
  clearBtn.textContent = "Bersihkan";
  clearBtn.hidden = true;

  actions.append(liveBadge, clearBtn);
  header.append(titleGroup, actions);

  // Main canvas viewport
  const viewport = document.createElement("div");
  viewport.className = "canvas-stage-viewport";

  // Blank placeholder
  const blankState = document.createElement("div");
  blankState.className = "canvas-blank-state";

  const heroHeader = document.createElement("div");
  heroHeader.className = "canvas-hero-header";

  const eyebrow = document.createElement("span");
  eyebrow.className = "canvas-hero-eyebrow";
  eyebrow.textContent = "AI Analytics Studio";

  const blankHeading = document.createElement("h3");
  blankHeading.className = "canvas-hero-heading";
  blankHeading.textContent = "Visualisasikan Data Bisnis Secara Cepat & Interaktif";

  const blankDesc = document.createElement("p");
  blankDesc.className = "canvas-hero-desc";
  blankDesc.textContent =
    "Pilih analisis instan di bawah ini atau ajukan pertanyaan melalui Chat Assistant untuk merender grafik, matriks pivot, atau simulasi langsung di kanvas ini.";

  heroHeader.append(eyebrow, blankHeading, blankDesc);

  const bentoGrid = document.createElement("div");
  bentoGrid.className = "canvas-bento-grid";

  const bentoItems = [
    {
      category: "Peringkat Penjualan",
      title: "5 Customer Terloyal",
      desc: "Grafik batang omzet pelanggan dengan transaksi tertinggi dan volume order.",
      meta: "sale.order · Bar Chart",
      prompt: "Siapa 5 customer terloyal tahun ini? Tampilkan grafiknya di canvas.",
      theme: "accent",
    },
    {
      category: "BI Pivot Matrix",
      title: "Pivot Omzet & Status",
      desc: "Matriks pivot multidimensi omzet per pelanggan dan status pesanan dengan ekspor CSV.",
      meta: "Hierarki Multidimensi · Ekspor CSV",
      prompt: "Tampilkan pivot table omzet penjualan per customer dan status pesanan.",
      theme: "emerald",
    },
    {
      category: "Tren Keuangan",
      title: "Tren Omzet Bulanan",
      desc: "Analisis pergerakan total nilai penjualan per bulan sepanjang tahun 2026.",
      meta: "Performa Bulanan · Trend Chart",
      prompt: "Tampilkan omzet penjualan bulanan tahun ini dalam bentuk chart.",
      theme: "purple",
    },
    {
      category: "Aging Piutang",
      title: "Ringkasan Piutang Jatuh Tempo",
      desc: "Daftar ringkasan faktur pelanggan yang mendekati atau telah melewati jatuh tempo.",
      meta: "account.move · Sisa Tagihan",
      prompt: "Tampilkan tabel ringkasan faktur piutang yang jatuh tempo.",
      theme: "amber",
    },
    {
      category: "Workflow Proses",
      title: "Alur Siklus Pesanan",
      desc: "Diagram visual alur dari penawaran harga hingga faktur terbayar lunas.",
      meta: "Sales Lifecycle · Flowchart",
      prompt: "Gambarkan alur proses dari penawaran harga hingga faktur terbayar.",
      theme: "blue",
    },
  ];

  bentoItems.forEach(item => {
    const card = document.createElement("button");
    card.type = "button";
    card.className = `canvas-bento-card canvas-suggestion-chip card-theme-${item.theme}`;

    const topRow = document.createElement("div");
    topRow.className = "canvas-bento-top";

    const catSpan = document.createElement("span");
    catSpan.className = "canvas-bento-category";
    catSpan.textContent = item.category;

    topRow.append(catSpan);

    const titleEl = document.createElement("h4");
    titleEl.className = "canvas-bento-title";
    titleEl.textContent = item.title;

    const descEl = document.createElement("p");
    descEl.className = "canvas-bento-desc";
    descEl.textContent = item.desc;

    const metaEl = document.createElement("span");
    metaEl.className = "canvas-bento-meta";
    metaEl.textContent = item.meta;

    card.append(topRow, titleEl, descEl, metaEl);
    card.addEventListener("click", () => {
      if (onPromptSelect) onPromptSelect(item.prompt);
    });
    bentoGrid.append(card);
  });

  blankState.append(heroHeader, bentoGrid);
  viewport.append(blankState);

  container.append(header, viewport);

  function clear() {
    viewport.replaceChildren(blankState);
    stageTitle.textContent = "AI Workspace Canvas";
    stageSubtitle.textContent = "Visualisasi dinamis, grafik interaktif, dan simulasi proses bisnis";
    clearBtn.hidden = true;
  }

  clearBtn.addEventListener("click", clear);

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

    viewport.replaceChildren();

    const card = document.createElement("div");
    card.className = "canvas-rendered-card";

    let renderedContent: HTMLElement | undefined;

    try {
      switch (canvasType) {
        case "chart": {
          const series = (data["series"] || data["data"] || (Array.isArray(data) ? data : [])) as any[];
          const chartType = String(data["type"] || "bar");
          const chartSpec: ChartSpec = {
            type: chartType as any,
            title,
            series,
            unit: typeof data["unit"] === "string" ? data["unit"] : undefined,
          };
          if (chartType === "donut" || chartType === "pie") {
            renderedContent = renderDonutChart(chartSpec);
          } else {
            renderedContent = renderBarChart(chartSpec);
          }
          break;
        }
        case "table": {
          const columns = (data["columns"] || data["headers"]) as string[] | undefined;
          const rows = (data["rows"] || data["records"] || (Array.isArray(data) ? data : [])) as any;
          const tableSpec: TableSpec = {
            title,
            columns: Array.isArray(columns) ? columns : undefined,
            rows: Array.isArray(rows) ? rows : undefined,
            records: Array.isArray(data["records"]) ? (data["records"] as any[]) : undefined,
          };
          renderedContent = renderRichTable(tableSpec);
          break;
        }
        case "pivot": {
          const dims = (data["dimensions"] || data["dimension"] || data["groupby"] || data["group_by"]) as string[] | undefined;
          const measures = data["measures"] as any[] | undefined;
          const records = (data["records"] || data["groups"] || (Array.isArray(data) ? data : undefined)) as Record<string, unknown>[] | undefined;
          const columns = (data["columns"] as string[]) || [];
          const rows = (data["rows"] as any[][]) || [];
          const pivotSpec: PivotSpec = {
            title,
            dimensions: Array.isArray(dims) ? dims : undefined,
            measures: Array.isArray(measures) ? measures : undefined,
            records: Array.isArray(records) ? records : undefined,
            columns: Array.isArray(columns) ? columns : undefined,
            rows: Array.isArray(rows) ? rows : undefined,
          };
          renderedContent = renderPivotTable(pivotSpec);
          break;
        }
        case "slides": {
          renderedContent = slides(data);
          break;
        }
        case "flow": {
          const steps = (data["steps"] as any[]) || [];
          const flowSpec: FlowSpec = { title, steps };
          renderedContent = renderWorkflowFlow(flowSpec);
          break;
        }
        case "simulation": {
          const variables = (data["variables"] as any[]) || [];
          const baseRev = Number(data["baseRevenue"]);
          const baseMargin = Number(data["baseMarginPct"]);
          const simSpec: SimulationSpec = {
            title,
            description,
            variables,
            baseRevenue: Number.isFinite(baseRev) ? baseRev : 0,
            baseMarginPct: Number.isFinite(baseMargin) ? baseMargin : 0,
          };
          renderedContent = renderSimulation(simSpec);
          break;
        }
        case "dashboard": {
          // Multi-card dashboard (KPI stats + Chart + Table)
          const dashboard = document.createElement("div");
          dashboard.className = "canvas-dashboard-grid";

          if (Array.isArray(data["kpis"])) {
            const kpiRow = document.createElement("div");
            kpiRow.className = "canvas-sim-kpi-row";
            (data["kpis"] as any[]).forEach(k => {
              if (!k || typeof k !== "object") return;
              const kpiCard = document.createElement("div");
              kpiCard.className = "canvas-kpi-card";
              const kpiLbl = document.createElement("span");
              kpiLbl.textContent = String(k["label"] || "");
              const kpiVal = document.createElement("strong");
              kpiVal.className = "canvas-kpi-val";
              kpiVal.textContent = String(k["value"] || "");
              kpiCard.append(kpiLbl, kpiVal);
              kpiRow.append(kpiCard);
            });
            dashboard.append(kpiRow);
          }

          if (data["chart"] && typeof data["chart"] === "object") {
            const chartData = data["chart"] as any;
            const chartEl = renderBarChart({
              title: chartData["title"] || "Grafik Dashboard",
              series: chartData["series"] || [],
              unit: chartData["unit"],
            });
            dashboard.append(chartEl);
          }

          if (data["table"] && typeof data["table"] === "object") {
            const tableData = data["table"] as any;
            const tableEl = renderRichTable({
              title: tableData["title"],
              columns: tableData["columns"],
              rows: tableData["rows"],
              records: tableData["records"],
            });
            dashboard.append(tableEl);
          }

          renderedContent = dashboard;
          break;
        }
        default: {
          // Fallback: check if series or columns exist
          if (Array.isArray(data["series"])) {
            renderedContent = renderBarChart({ title, series: data["series"] as any });
          } else if (Array.isArray(data["columns"]) || Array.isArray(data["rows"]) || Array.isArray(data["records"])) {
            renderedContent = renderRichTable({
              title,
              columns: data["columns"] as any,
              rows: data["rows"] as any,
              records: data["records"] as any,
            });
          } else {
            const pre = document.createElement("pre");
            pre.textContent = JSON.stringify(data, null, 2);
            renderedContent = pre;
          }
        }
      }

      if (renderedContent) {
        card.append(renderedContent);
        viewport.append(card);
      }
    } catch (renderError) {
      console.error("Canvas artifact rendering error:", renderError);
      const errNotice = document.createElement("div");
      errNotice.className = "canvas-render-error";
      const errTitle = document.createElement("strong");
      errTitle.textContent = "Gagal memvisualisasikan data pada canvas";
      const errDesc = document.createElement("p");
      errDesc.textContent = renderError instanceof Error ? renderError.message : "Format data tidak sesuai.";
      errNotice.append(errTitle, errDesc);
      card.append(errNotice);
      viewport.append(card);
    }
  }

  function detectAndRender(text: string): boolean {
    // Regex for fenced code block ```erp-view or ```json
    const match = text.match(/```(?:erp-view|json)\s*\n([\s\S]*?)\n```/);
    if (!match || !match[1]) return false;
    try {
      const parsed = JSON.parse(match[1]);
      renderArtifact(parsed);
      return true;
    } catch (err) {
      console.warn("AI Canvas artifact JSON parse failed:", err);
      viewport.replaceChildren();
      const errCard = document.createElement("div");
      errCard.className = "canvas-empty-state";
      const icon = document.createElement("div");
      icon.className = "canvas-blank-icon";
      icon.setAttribute("aria-hidden", "true");
      const title = document.createElement("div");
      title.className = "canvas-empty-title";
      title.textContent = "Format Visualisasi Tidak Valid";
      const desc = document.createElement("div");
      desc.className = "canvas-empty-desc";
      desc.textContent = "Gagal memproses artifact visual: Format data JSON tidak terbaca.";
      errCard.append(icon, title, desc);
      viewport.append(errCard);
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
