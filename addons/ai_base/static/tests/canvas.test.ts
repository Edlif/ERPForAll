/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, describe, expect, test, vi } from "vitest";
import { renderBarChart, renderDonutChart } from "../src/canvas/canvas-chart.js";
import { renderRichTable } from "../src/canvas/canvas-table.js";
import { renderWorkflowFlow } from "../src/canvas/canvas-flow.js";
import { renderSimulation } from "../src/canvas/canvas-simulation.js";
import { createCanvasStage } from "../src/canvas/canvas-stage.js";

afterEach(() => document.body.replaceChildren());

describe("Canvas Visual Components", () => {
  test("renderBarChart creates SVG bars with proper heights and labels", () => {
    const el = renderBarChart({
      title: "Top Customers",
      series: [
        { label: "Deco Addict", value: 100 },
        { label: "Azure Interior", value: 50 },
      ],
      unit: "USD",
    });
    document.body.append(el);

    const svg = el.querySelector("svg");
    expect(svg).not.toBeNull();
    const rects = el.querySelectorAll("rect.canvas-bar");
    expect(rects.length).toBe(2);

    const texts = Array.from(el.querySelectorAll("text"), t => t.textContent);
    expect(texts).toContain("Deco Addict");
    expect(texts).toContain("Azure Interior");
  });

  test("renderDonutChart calculates segments and legend totals", () => {
    const el = renderDonutChart({
      title: "Revenue by State",
      series: [
        { label: "Draft", value: 20 },
        { label: "Done", value: 80 },
      ],
      unit: "USD",
    });
    document.body.append(el);

    const segments = el.querySelectorAll("circle.canvas-donut-segment");
    expect(segments.length).toBe(2);

    const legendItems = el.querySelectorAll(".canvas-donut-legend li");
    expect(legendItems.length).toBe(2);
    expect(legendItems[0]?.textContent).toContain("Draft");
  });

  test("renderRichTable sorts columns and filters rows", () => {
    const el = renderRichTable({
      title: "Sales Orders",
      columns: ["Order", "Customer", "Amount"],
      rows: [
        ["SO001", "Customer B", 20000],
        ["SO002", "Customer A", 50000],
        ["SO003", "Customer C", 10000],
      ],
    });
    document.body.append(el);

    const rows = () => Array.from(el.querySelectorAll("tbody tr"), tr => Array.from(tr.querySelectorAll("td"), td => td.textContent));
    expect(rows().length).toBe(3);

    // Filter
    const search = el.querySelector<HTMLInputElement>(".canvas-table-search")!;
    search.value = "Customer A";
    search.dispatchEvent(new Event("input"));
    expect(rows().length).toBe(1);
    expect(rows()[0]?.[1]).toBe("Customer A");

    // Clear filter
    search.value = "";
    search.dispatchEvent(new Event("input"));
    expect(rows().length).toBe(3);

    // Sort by Amount ascending (click column 2)
    const amountTh = el.querySelectorAll("th")[2]!;
    amountTh.click();
    expect(rows()[0]?.[0]).toBe("SO003"); // 10000 is first
    expect(rows()[2]?.[0]).toBe("SO002"); // 50000 is last

    // Sort descending
    amountTh.click();
    expect(rows()[0]?.[0]).toBe("SO002"); // 50000 is first
  });

  test("renderWorkflowFlow displays status badges and active indicators", () => {
    const el = renderWorkflowFlow({
      title: "Lifecycle",
      steps: [
        { id: "draft", label: "Penawaran", state: "completed" },
        { id: "sale", label: "Pesanan Penjualan", state: "active" },
        { id: "done", label: "Faktur Selesai", state: "pending" },
      ],
    });
    document.body.append(el);

    const steps = el.querySelectorAll(".canvas-flow-step");
    expect(steps.length).toBe(3);
    expect(steps[0]?.classList.contains("step-completed")).toBe(true);
    expect(steps[1]?.classList.contains("step-active")).toBe(true);
    expect(steps[2]?.classList.contains("step-pending")).toBe(true);
  });

  test("renderSimulation updates projected revenue and margin on slider change", () => {
    const el = renderSimulation({
      title: "Simulasi Diskon",
      variables: [
        { key: "discount", label: "Diskon Promosi", min: 0, max: 30, initial: 10, unit: "%" },
      ],
      baseRevenue: 1000000,
      baseMarginPct: 30,
    });
    document.body.append(el);

    const readout = el.querySelector(".canvas-sim-readout")!;
    expect(readout.textContent).toBe("10%");

    const slider = el.querySelector<HTMLInputElement>("input[type=range]")!;
    slider.value = "20";
    slider.dispatchEvent(new Event("input"));

    expect(readout.textContent).toBe("20%");
    const kpis = el.querySelectorAll(".canvas-kpi-val");
    expect(kpis.length).toBe(3);
  });

  test("createCanvasStage renders artifact and detects fenced erp-view JSON", () => {
    const stage = createCanvasStage();
    document.body.append(stage.node);

    expect(stage.node.querySelector(".canvas-blank-state")).not.toBeNull();

    // Render structured artifact
    stage.renderArtifact({
      canvas_type: "chart",
      title: "Hasil Agregasi",
      data: {
        series: [{ label: "A", value: 10 }],
      },
    });

    expect(stage.node.querySelector(".canvas-blank-state")).toBeNull();
    expect(stage.node.querySelector(".canvas-bar-chart")).not.toBeNull();
    expect(stage.node.textContent).toContain("Hasil Agregasi");

    // Clear
    stage.clear();
    expect(stage.node.querySelector(".canvas-blank-state")).not.toBeNull();

    // Detect fenced code block
    const text = "Berikut adalah chart:\n```erp-view\n{\"type\":\"chart\",\"title\":\"Fenced Chart\",\"series\":[{\"label\":\"B\",\"value\":20}]}\n```";
    const detected = stage.detectAndRender(text);
    expect(detected).toBe(true);
    expect(stage.node.querySelector(".canvas-bar-chart")).not.toBeNull();
    expect(stage.node.textContent).toContain("Fenced Chart");

    stage.dispose();
  });

  test("createCanvasStage detectAndRender handles malformed JSON by displaying error card", () => {
    const stage = createCanvasStage();
    document.body.append(stage.node);

    const invalidText = "Berikut adalah chart:\n```erp-view\n{invalid_json:\n```";
    const detectedErr = stage.detectAndRender(invalidText);
    expect(detectedErr).toBe(false);
    expect(stage.node.textContent).toContain("Format Visualisasi Tidak Valid");

    stage.dispose();
  });
});
