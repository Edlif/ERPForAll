/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

export type ChartPoint = {
  label: string;
  value: number;
  color?: string;
};

export type ChartSpec = {
  type?: "bar" | "donut" | "line";
  title?: string;
  series: unknown;
  unit?: string;
};

function formatNum(val: number, unit?: string): string {
  const formatted = new Intl.NumberFormat("id-ID", { maximumFractionDigits: 2 }).format(val);
  return unit ? `${formatted} ${unit}` : formatted;
}

const COLORS = [
  "var(--accent)",
  "var(--success)",
  "var(--warning)",
  "var(--danger)",
  "var(--group-text)",
  "var(--selected-text)",
  "var(--focus)",
];

export function normalizeSeries(input: unknown): ChartPoint[] {
  if (Array.isArray(input)) {
    return input.map((item, idx) => {
      if (typeof item === "object" && item !== null) {
        const obj = item as Record<string, unknown>;
        const label = String(obj["label"] ?? obj["name"] ?? obj["key"] ?? obj["category"] ?? `Item ${idx + 1}`);
        const val = Number(obj["value"] ?? obj["val"] ?? obj["amount"] ?? obj["total"] ?? obj["count"] ?? 0);
        const color = typeof obj["color"] === "string" ? obj["color"] : undefined;
        return { label, value: Number.isFinite(val) ? val : 0, color };
      }
      return { label: `Item ${idx + 1}`, value: Number(item) || 0 };
    });
  }
  if (typeof input === "object" && input !== null) {
    return Object.entries(input as Record<string, unknown>).map(([k, v]) => ({
      label: k,
      value: Number.isFinite(Number(v)) ? Number(v) : 0,
    }));
  }
  return [];
}

/**
 * Render an interactive SVG bar chart with animated transitions and tooltips.
 */
export function renderBarChart(spec: ChartSpec): HTMLElement {
  const container = document.createElement("div");
  container.className = "canvas-chart canvas-bar-chart";

  const series = normalizeSeries(spec.series);
  const unit = spec.unit;
  if (!series.length) {
    container.textContent = "Tidak ada data untuk ditampilkan.";
    return container;
  }

  const maxValue =
    Math.max(...series.map(s => (Number.isFinite(s.value) ? Math.abs(s.value) : 0))) || 1;
  const height = 240;
  const barWidth = Math.max(24, Math.min(60, Math.floor(480 / series.length) - 12));
  const svgWidth = Math.max(500, series.length * (barWidth + 16) + 60);

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", `0 0 ${svgWidth} ${height + 60}`);
  svg.setAttribute("width", "100%");
  svg.setAttribute("height", "100%");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", spec.title || "Grafik Batang");

  // Grid line at 0
  const baseLine = document.createElementNS("http://www.w3.org/2000/svg", "line");
  baseLine.setAttribute("x1", "40");
  baseLine.setAttribute("y1", String(height));
  baseLine.setAttribute("x2", String(svgWidth - 20));
  baseLine.setAttribute("y2", String(height));
  baseLine.setAttribute("stroke", "var(--line)");
  baseLine.setAttribute("stroke-width", "1");
  svg.append(baseLine);

  series.forEach((point, i) => {
    const x = 50 + i * (barWidth + 16);
    const val = Number.isFinite(point.value) ? Math.abs(point.value) : 0;
    const barHeight = Math.max(4, Math.round((val / maxValue) * (height - 30)));
    const y = height - barHeight;
    const color = point.color || COLORS[i % COLORS.length] || "var(--accent)";

    // Bar rectangle
    const rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
    rect.setAttribute("x", String(x));
    rect.setAttribute("y", String(y));
    rect.setAttribute("width", String(barWidth));
    rect.setAttribute("height", String(barHeight));
    rect.setAttribute("rx", "4");
    rect.setAttribute("fill", color);
    rect.classList.add("canvas-bar");

    // Value label on top
    const textVal = document.createElementNS("http://www.w3.org/2000/svg", "text");
    textVal.setAttribute("x", String(x + barWidth / 2));
    textVal.setAttribute("y", String(y - 6));
    textVal.setAttribute("text-anchor", "middle");
    textVal.setAttribute("font-size", "11");
    textVal.setAttribute("font-weight", "600");
    textVal.setAttribute("fill", "var(--text)");
    textVal.textContent = formatNum(point.value, unit);

    // Label below
    const textLabel = document.createElementNS("http://www.w3.org/2000/svg", "text");
    textLabel.setAttribute("x", String(x + barWidth / 2));
    textLabel.setAttribute("y", String(height + 20));
    textLabel.setAttribute("text-anchor", "middle");
    textLabel.setAttribute("font-size", "11");
    textLabel.setAttribute("fill", "var(--muted)");
    const shortLabel = point.label.length > 16 ? point.label.slice(0, 15) + "…" : point.label;
    textLabel.textContent = shortLabel;

    const titleEl = document.createElementNS("http://www.w3.org/2000/svg", "title");
    titleEl.textContent = `${point.label}: ${formatNum(point.value, unit)}`;
    rect.append(titleEl);

    svg.append(rect, textVal, textLabel);
  });

  container.append(svg);
  return container;
}

/**
 * Render an interactive SVG Trend Line Chart.
 */
export function renderLineChart(spec: ChartSpec): HTMLElement {
  const container = document.createElement("div");
  container.className = "canvas-chart canvas-line-chart";

  const series = normalizeSeries(spec.series);
  const unit = spec.unit;
  if (!series.length) {
    container.textContent = "Tidak ada data untuk ditampilkan.";
    return container;
  }

  const maxValue =
    Math.max(...series.map(s => (Number.isFinite(s.value) ? Math.abs(s.value) : 0))) || 1;
  const height = 240;
  const stepX = Math.max(64, Math.floor(480 / Math.max(1, series.length - 1)));
  const svgWidth = Math.max(500, (series.length - 1) * stepX + 120);

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", `0 0 ${svgWidth} ${height + 60}`);
  svg.setAttribute("width", "100%");
  svg.setAttribute("height", "100%");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", spec.title || "Grafik Tren");

  // Grid line at 0
  const baseLine = document.createElementNS("http://www.w3.org/2000/svg", "line");
  baseLine.setAttribute("x1", "40");
  baseLine.setAttribute("y1", String(height));
  baseLine.setAttribute("x2", String(svgWidth - 20));
  baseLine.setAttribute("y2", String(height));
  baseLine.setAttribute("stroke", "var(--line)");
  baseLine.setAttribute("stroke-width", "1");
  svg.append(baseLine);

  const points = series.map((point, i) => {
    const x = 60 + i * stepX;
    const val = Number.isFinite(point.value) ? Math.abs(point.value) : 0;
    const pointHeight = Math.max(4, Math.round((val / maxValue) * (height - 40)));
    const y = height - pointHeight;
    return { x, y, point };
  });

  if (points.length > 1) {
    const pathD = points.map((p, i) => `${i === 0 ? "M" : "L"} ${p.x} ${p.y}`).join(" ");
    const linePath = document.createElementNS("http://www.w3.org/2000/svg", "path");
    linePath.setAttribute("d", pathD);
    linePath.setAttribute("fill", "none");
    linePath.setAttribute("stroke", "var(--accent)");
    linePath.setAttribute("stroke-width", "3");
    linePath.setAttribute("stroke-linecap", "round");
    linePath.setAttribute("stroke-linejoin", "round");
    linePath.classList.add("canvas-line-path");
    svg.append(linePath);
  }

  points.forEach(({ x, y, point }, i) => {
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(x));
    circle.setAttribute("cy", String(y));
    circle.setAttribute("r", "5");
    circle.setAttribute("fill", point.color || COLORS[i % COLORS.length] || "var(--accent)");
    circle.setAttribute("stroke", "var(--surface)");
    circle.setAttribute("stroke-width", "2");
    circle.classList.add("canvas-line-point");

    const textVal = document.createElementNS("http://www.w3.org/2000/svg", "text");
    textVal.setAttribute("x", String(x));
    textVal.setAttribute("y", String(y - 10));
    textVal.setAttribute("text-anchor", "middle");
    textVal.setAttribute("font-size", "11");
    textVal.setAttribute("font-weight", "600");
    textVal.setAttribute("fill", "var(--text)");
    textVal.textContent = formatNum(point.value, unit);

    const textLabel = document.createElementNS("http://www.w3.org/2000/svg", "text");
    textLabel.setAttribute("x", String(x));
    textLabel.setAttribute("y", String(height + 20));
    textLabel.setAttribute("text-anchor", "middle");
    textLabel.setAttribute("font-size", "11");
    textLabel.setAttribute("fill", "var(--muted)");
    const shortLabel = point.label.length > 16 ? point.label.slice(0, 15) + "…" : point.label;
    textLabel.textContent = shortLabel;

    const titleEl = document.createElementNS("http://www.w3.org/2000/svg", "title");
    titleEl.textContent = `${point.label}: ${formatNum(point.value, unit)}`;
    circle.append(titleEl);

    svg.append(circle, textVal, textLabel);
  });

  container.append(svg);
  return container;
}

/**
 * Render an interactive SVG Donut / Pie Chart.
 */
export function renderDonutChart(spec: ChartSpec): HTMLElement {
  const container = document.createElement("div");
  container.className = "canvas-chart canvas-donut-chart";

  const series = normalizeSeries(spec.series);
  const unit = spec.unit;
  if (!series.length) {
    container.textContent = "Tidak ada data untuk ditampilkan.";
    return container;
  }

  const total = series.reduce((acc, p) => acc + Math.max(0, p.value), 0) || 1;
  const size = 260;
  const radius = 90;
  const circumference = 2 * Math.PI * radius;
  const center = size / 2;

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", `0 0 ${size} ${size}`);
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.classList.add("canvas-donut-svg");

  let accumulated = 0;
  series.forEach((point, i) => {
    const val = Math.max(0, point.value);
    const fraction = val / total;
    const strokeDash = fraction * circumference;
    const offset = -accumulated * circumference;
    const color = point.color || COLORS[i % COLORS.length] || "var(--accent)";

    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(center));
    circle.setAttribute("cy", String(center));
    circle.setAttribute("r", String(radius));
    circle.setAttribute("fill", "none");
    circle.setAttribute("stroke", color);
    circle.setAttribute("stroke-width", "28");
    circle.setAttribute("stroke-dasharray", `${strokeDash} ${circumference}`);
    circle.setAttribute("stroke-dashoffset", String(offset));
    circle.classList.add("canvas-donut-segment");

    const titleEl = document.createElementNS("http://www.w3.org/2000/svg", "title");
    titleEl.textContent = `${point.label}: ${formatNum(val, unit)} (${(fraction * 100).toFixed(1)}%)`;
    circle.append(titleEl);

    svg.append(circle);
    accumulated += fraction;
  });

  // Center summary text
  const centerText = document.createElementNS("http://www.w3.org/2000/svg", "text");
  centerText.setAttribute("x", String(center));
  centerText.setAttribute("y", String(center - 5));
  centerText.setAttribute("text-anchor", "middle");
  centerText.setAttribute("font-size", "12");
  centerText.setAttribute("fill", "var(--muted)");
  centerText.textContent = "Total";

  const totalText = document.createElementNS("http://www.w3.org/2000/svg", "text");
  totalText.setAttribute("x", String(center));
  totalText.setAttribute("y", String(center + 16));
  totalText.setAttribute("text-anchor", "middle");
  totalText.setAttribute("font-size", "14");
  totalText.setAttribute("font-weight", "bold");
  totalText.setAttribute("fill", "var(--text)");
  totalText.textContent = formatNum(total, unit);

  svg.append(centerText, totalText);

  // Legend list
  const legend = document.createElement("ul");
  legend.className = "canvas-donut-legend";
  series.forEach((p, i) => {
    const li = document.createElement("li");
    const dot = document.createElement("span");
    dot.className = "canvas-legend-dot";
    dot.style.background = p.color || COLORS[i % COLORS.length] || "var(--accent)";

    const labelSpan = document.createElement("span");
    labelSpan.className = "canvas-legend-label";
    labelSpan.textContent = p.label;

    const valSpan = document.createElement("strong");
    valSpan.textContent = formatNum(p.value, unit);

    li.append(dot, labelSpan, valSpan);
    legend.append(li);
  });

  container.append(svg, legend);
  return container;
}
