/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

export type TableSpec = {
  title?: string;
  columns: string[];
  rows: (string | number | boolean | null)[][];
};

export function renderRichTable(spec: TableSpec): HTMLElement {
  const container = document.createElement("div");
  container.className = "canvas-table-container";

  const { columns, rows: rawRows } = spec;
  if (!columns.length) {
    container.textContent = "Tidak ada kolom yang didefinisikan.";
    return container;
  }

  let sortColIndex = -1;
  let sortAsc = true;
  let filterText = "";
  let displayRows = [...rawRows];

  // Toolbar (Filter & CSV Export)
  const toolbar = document.createElement("div");
  toolbar.className = "canvas-table-toolbar";

  const searchBox = document.createElement("input");
  searchBox.type = "search";
  searchBox.placeholder = "Cari di tabel…";
  searchBox.className = "canvas-table-search";

  const exportBtn = document.createElement("button");
  exportBtn.type = "button";
  exportBtn.className = "erp-button erp-button-secondary canvas-table-export";
  exportBtn.textContent = "Ekspor CSV";

  const countBadge = document.createElement("span");
  countBadge.className = "canvas-table-count";
  countBadge.textContent = `${displayRows.length} baris`;

  toolbar.append(searchBox, exportBtn, countBadge);

  // Table element
  const scrollWrapper = document.createElement("div");
  scrollWrapper.className = "canvas-table-scroll";

  const table = document.createElement("table");
  table.className = "canvas-data-table";

  const thead = table.createTHead();
  const headerRow = thead.insertRow();

  columns.forEach((col, idx) => {
    const th = document.createElement("th");
    th.scope = "col";
    th.className = "canvas-th-sortable";

    const labelSpan = document.createElement("span");
    labelSpan.textContent = col;

    const sortIndicator = document.createElement("span");
    sortIndicator.className = "canvas-sort-indicator";

    th.append(labelSpan, sortIndicator);

    th.addEventListener("click", () => {
      if (sortColIndex === idx) {
        sortAsc = !sortAsc;
      } else {
        sortColIndex = idx;
        sortAsc = true;
      }
      renderRows();
    });

    headerRow.append(th);
  });

  const tbody = table.createTBody();

  function updateDisplayRows() {
    let rows = [...rawRows];
    if (filterText.trim()) {
      const q = filterText.toLowerCase();
      rows = rows.filter(r =>
        r.some(cell => cell !== null && String(cell).toLowerCase().includes(q))
      );
    }
    if (sortColIndex >= 0) {
      rows.sort((a, b) => {
        const valA = a[sortColIndex];
        const valB = b[sortColIndex];
        if (valA === valB) return 0;
        if (valA === null || valA === undefined) return 1;
        if (valB === null || valB === undefined) return -1;
        if (typeof valA === "number" && typeof valB === "number") {
          return sortAsc ? valA - valB : valB - valA;
        }
        const strA = String(valA);
        const strB = String(valB);
        return sortAsc ? strA.localeCompare(strB) : strB.localeCompare(strA);
      });
    }
    displayRows = rows;
    countBadge.textContent = `${displayRows.length} baris`;
  }

  function renderRows() {
    updateDisplayRows();

    // Update header sort indicators
    headerRow.querySelectorAll("th").forEach((th, idx) => {
      const indicator = th.querySelector(".canvas-sort-indicator");
      if (indicator) {
        if (idx === sortColIndex) {
          indicator.textContent = sortAsc ? " [asc]" : " [desc]";
          th.classList.add("active-sort");
        } else {
          indicator.textContent = "";
          th.classList.remove("active-sort");
        }
      }
    });

    tbody.replaceChildren();
    if (!displayRows.length) {
      const emptyTr = tbody.insertRow();
      const emptyTd = emptyTr.insertCell();
      emptyTd.colSpan = columns.length;
      emptyTd.className = "canvas-td-empty";
      emptyTd.textContent = "Tidak ada baris yang sesuai pencarian.";
      return;
    }

    displayRows.forEach(row => {
      const tr = tbody.insertRow();
      row.forEach(val => {
        const td = tr.insertCell();
        if (val === null || val === undefined) {
          td.textContent = "—";
          td.classList.add("cell-null");
        } else if (typeof val === "number") {
          td.textContent = new Intl.NumberFormat("id-ID").format(val);
          td.classList.add("cell-numeric");
        } else {
          td.textContent = String(val);
        }
      });
    });
  }

  searchBox.addEventListener("input", () => {
    filterText = searchBox.value;
    renderRows();
  });

  exportBtn.addEventListener("click", () => {
    const csvLines = [
      columns.map(c => `"${c.replace(/"/g, '""')}"`).join(","),
      ...displayRows.map(r =>
        r.map(cell => `"${String(cell ?? "").replace(/"/g, '""')}"`).join(",")
      ),
    ];
    const blob = new Blob([csvLines.join("\n")], { type: "text/csv;charset=utf-8;" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${spec.title || "export"}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  });

  scrollWrapper.append(table);
  container.append(toolbar, scrollWrapper);
  renderRows();
  return container;
}
