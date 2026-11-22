/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

import { renderBarChart, renderDonutChart, type ChartSpec } from "./canvas-chart.js";
import { renderRichTable, type TableSpec } from "./canvas-table.js";
import { renderWorkflowFlow, type FlowSpec } from "./canvas-flow.js";
import { renderSimulation, type SimulationSpec } from "./canvas-simulation.js";

export type CanvasArtifact = {
  canvas_type: "chart" | "table" | "flow" | "dashboard" | "simulation";
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

  const clearBtn = document.createElement("button");
  clearBtn.type = "button";
  clearBtn.className = "erp-button erp-button-secondary canvas-btn-clear";
  clearBtn.textContent = "Bersihkan";
  clearBtn.hidden = true;

  actions.append(clearBtn);
  header.append(titleGroup, actions);

  // Main canvas viewport
  const viewport = document.createElement("div");
  viewport.className = "canvas-stage-viewport";

  // Blank placeholder
  const blankState = document.createElement("div");
  blankState.className = "canvas-blank-state";

  const blankIcon = document.createElement("div");
  blankIcon.className = "canvas-blank-icon";

  const blankHeading = document.createElement("h3");
  blankHeading.textContent = "Blank Canvas Siap Memvisualisasikan Data";

  const blankDesc = document.createElement("p");
  blankDesc.textContent =
    "Tanyakan analisis penjualan, peringkat customer terloyal, status alur pesanan, atau minta simulasi skenario. AI akan merender visualisasi interaktif langsung di sini.";

  const suggestions = document.createElement("div");
  suggestions.className = "canvas-suggestions";

  const chips = [
    { label: "5 Customer Terloyal", prompt: "Siapa 5 customer terloyal tahun ini? Tampilkan grafiknya di canvas." },
    { label: "Tren Omzet Bulanan", prompt: "Tampilkan omzet penjualan bulanan tahun ini dalam bentuk chart." },
    { label: "Ringkasan Piutang Jatuh Tempo", prompt: "Tampilkan tabel ringkasan faktur piutang yang jatuh tempo." },
    { label: "Alur Siklus Pesanan", prompt: "Gambarkan alur proses dari penawaran harga hingga faktur terbayar." },
  ];

  chips.forEach(chip => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "canvas-suggestion-chip";
    btn.textContent = chip.label;
    btn.addEventListener("click", () => {
      if (onPromptSelect) onPromptSelect(chip.prompt);
    });
    suggestions.append(btn);
  });

  blankState.append(blankIcon, blankHeading, blankDesc, suggestions);
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

    switch (canvasType) {
      case "chart": {
        const series = (data["series"] as any[]) || [];
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
        const columns = (data["columns"] as string[]) || [];
        const rows = (data["rows"] as any[][]) || [];
        const tableSpec: TableSpec = { title, columns, rows };
        renderedContent = renderRichTable(tableSpec);
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

        if (data["chart"]) {
          const chartData = data["chart"] as any;
          const chartEl = renderBarChart({
            title: chartData["title"] || "Grafik Dashboard",
            series: chartData["series"] || [],
            unit: chartData["unit"],
          });
          dashboard.append(chartEl);
        }

        if (data["table"]) {
          const tableData = data["table"] as any;
          const tableEl = renderRichTable({
            title: tableData["title"],
            columns: tableData["columns"] || [],
            rows: tableData["rows"] || [],
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
        } else if (Array.isArray(data["columns"]) && Array.isArray(data["rows"])) {
          renderedContent = renderRichTable({ title, columns: data["columns"] as any, rows: data["rows"] as any });
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
