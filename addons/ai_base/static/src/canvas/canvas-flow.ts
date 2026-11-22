/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

export type FlowStep = {
  id: string;
  label: string;
  state: "completed" | "active" | "pending" | "cancelled";
  description?: string;
  metadata?: string;
};

export type FlowSpec = {
  title?: string;
  steps: FlowStep[];
};

export function renderWorkflowFlow(spec: FlowSpec): HTMLElement {
  const container = document.createElement("div");
  container.className = "canvas-flow-container";

  const { steps } = spec;
  if (!steps.length) {
    container.textContent = "Tidak ada alur proses yang didefinisikan.";
    return container;
  }

  const timeline = document.createElement("div");
  timeline.className = "canvas-flow-timeline";

  steps.forEach((step, idx) => {
    const stepEl = document.createElement("div");
    stepEl.className = `canvas-flow-step step-${step.state}`;

    // Node icon / badge
    const nodeBadge = document.createElement("div");
    nodeBadge.className = `canvas-flow-badge badge-${step.state}`;
    nodeBadge.textContent = String(idx + 1);

    // Step content
    const content = document.createElement("div");
    content.className = "canvas-flow-content";

    const label = document.createElement("h4");
    label.className = "canvas-flow-label";
    label.textContent = step.label;

    const statusBadge = document.createElement("span");
    statusBadge.className = `canvas-status-tag tag-${step.state}`;
    statusBadge.textContent =
      step.state === "completed"
        ? "Selesai"
        : step.state === "active"
        ? "Aktif / Berjalan"
        : step.state === "cancelled"
        ? "Dibatalkan"
        : "Menunggu";

    content.append(label, statusBadge);

    if (step.description) {
      const desc = document.createElement("p");
      desc.className = "canvas-flow-desc";
      desc.textContent = step.description;
      content.append(desc);
    }

    if (step.metadata) {
      const meta = document.createElement("span");
      meta.className = "canvas-flow-meta";
      meta.textContent = step.metadata;
      content.append(meta);
    }

    stepEl.append(nodeBadge, content);
    timeline.append(stepEl);

    // Connector line between steps
    if (idx < steps.length - 1) {
      const connector = document.createElement("div");
      connector.className = `canvas-flow-connector connector-${
        step.state === "completed" ? "completed" : "pending"
      }`;
      timeline.append(connector);
    }
  });

  container.append(timeline);
  return container;
}
