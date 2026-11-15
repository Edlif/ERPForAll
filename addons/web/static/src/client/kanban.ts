/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The kanban view's cards: the card layout read from the
// description, the ungrouped card grid and its keyboard. The application owns every read; this file only draws records.
import { ClientError } from "./transport.js";
import type { Field, Node, View } from "./metadata.js";

/** Where each card field goes: the first field of the card (or of `<main>`) is the title, `<aside>` is the leading tile. */
export interface KanbanCardLayout { title: Field | undefined; lines: Field[]; aside: Field | undefined; footer: Field[] }

/** The kanban root of a described view (`describeViews` already checked its shape). */
export function kanbanRoot(view: View): Extract<Node, { kind: "kanban" }> {
  const root = view.nodes[0];
  if (view.type !== "kanban" || root?.kind !== "kanban") throw new ClientError("server", "Invalid kanban view description.");
  return root;
}

export function kanbanLayout(view: View): KanbanCardLayout {
  const card = kanbanRoot(view).children.find(child => child.kind === "kanban_card");
  if (!card || card.kind !== "kanban_card") throw new ClientError("server", "A kanban view has no card.");
  const shown = (nodes: readonly Node[]): Field[] => nodes.flatMap(node => node.kind === "field" && !node.field.invisible ? [node.field] : []);
  const region = (name: "main" | "aside" | "footer"): Field[] => {
    const section = card.children.find(child => child.kind === "kanban_section" && child.region === name);
    return section && "children" in section ? shown(section.children) : [];
  };
  // Fields directly in the card read as `<main>` lines (reference card_arch_parser.js: the card is free markup).
  const main = [...shown(card.children), ...region("main")];
  const [title, ...lines] = main;
  return { title, lines, aside: region("aside")[0], footer: region("footer") };
}

/** Up to two initials of a value, for the tile that stands where an image will go (is Proposed). */
export function initials(value: unknown): string {
  const text = typeof value === "string" ? value : Array.isArray(value) && typeof value[1] === "string" ? value[1] : "";
  const words = text.trim().split(/\s+/u).filter(Boolean);
  const letters = (words.length > 1 ? [words[0]!, words[words.length - 1]!] : words.slice(0, 1)).map(word => [...word][0]!.toLocaleUpperCase());
  return letters.join("");
}

export interface KanbanRendered { node: HTMLElement; dispose: () => void }
export interface KanbanCardsOptions {
  layout: KanbanCardLayout;
  records: readonly Record<string, unknown>[];
  /** Draws one field of one record read-only (the application's field widgets). */
  renderField: (field: Field, record: Record<string, unknown>) => KanbanRendered;
  /** Opens a record; absent when the arch says `can_open="0"` or the action has no form. */
  open?: (id: number) => void;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text?: string): HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

/** The card grid: one focusable article per record, roving focus by the arrow keys, Enter opens. */
export function renderKanbanCards(options: KanbanCardsOptions): KanbanRendered {
  const { layout, records, renderField, open } = options;
  const stops: (() => void)[] = [];
  const grid = el("div", "kanban-grid");
  grid.setAttribute("role", "list");
  grid.setAttribute("aria-label", "Records");
  const field = (target: HTMLElement, item: Field, record: Record<string, unknown>, className: string): void => {
    const shown = renderField(item, record);
    stops.push(shown.dispose);
    const slot = el("div", className);
    slot.dataset["field"] = item.name;
    slot.append(shown.node);
    target.append(slot);
  };
  const cards: HTMLElement[] = records.map((record, index) => {
    const id = record["id"];
    if (typeof id !== "number") throw new ClientError("server", "A kanban record has no id.");
    const card = el("article", `kanban-card${open ? " kanban-card-open" : ""}`);
    card.setAttribute("role", "listitem");
    card.dataset["id"] = String(id);
    card.tabIndex = index === 0 ? 0 : -1;
    if (layout.aside) {
      const tile = el("div", "kanban-card-tile", initials(record[layout.aside.name]));
      tile.setAttribute("aria-hidden", "true");
      card.append(tile);
    }
    const body = el("div", "kanban-card-body");
    if (layout.title) field(body, layout.title, record, "kanban-card-title");
    for (const line of layout.lines) field(body, line, record, "kanban-card-line");
    if (layout.footer.length) {
      const footer = el("div", "kanban-card-footer");
      for (const item of layout.footer) field(footer, item, record, "kanban-card-meta");
      body.append(footer);
    }
    card.append(body);
    const title = layout.title ? body.querySelector(".kanban-card-title")?.textContent?.trim() : "";
    card.setAttribute("aria-label", title || `Record ${id}`);
    if (open) card.addEventListener("click", event => {
      if ((event.target as HTMLElement).closest("a, button, input, select, textarea")) return;
      open(id);
    });
    return card;
  });
  grid.append(...cards);
  // Rows come from the layout, so Up and Down follow what the user sees at any width.
  const move = (from: number, key: string): number => {
    if (key === "ArrowLeft") return from - 1;
    if (key === "ArrowRight") return from + 1;
    if (key === "Home") return 0;
    if (key === "End") return cards.length - 1;
    const top = cards[from]!.offsetTop, left = cards[from]!.offsetLeft;
    const rows = key === "ArrowDown" ? cards.slice(from + 1).filter(card => card.offsetTop > top) : cards.slice(0, from).filter(card => card.offsetTop < top).reverse();
    const row = rows.filter(card => card.offsetTop === rows[0]?.offsetTop);
    const target = row.reduce<HTMLElement | undefined>((best, card) => !best || Math.abs(card.offsetLeft - left) < Math.abs(best.offsetLeft - left) ? card : best, undefined);
    return target ? cards.indexOf(target) : from;
  };
  const onKey = (event: KeyboardEvent): void => {
    const card = (event.target as HTMLElement).closest<HTMLElement>(".kanban-card");
    if (!card || event.target !== card || event.altKey || event.ctrlKey || event.metaKey) return;
    const from = cards.indexOf(card);
    if (event.key === "Enter" && open) { event.preventDefault(); open(Number(card.dataset["id"])); return; }
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    focusCard(cards[Math.max(0, Math.min(cards.length - 1, move(from, event.key)))]!);
  };
  const focusCard = (card: HTMLElement): void => {
    for (const other of cards) other.tabIndex = other === card ? 0 : -1;
    card.focus();
  };
  grid.addEventListener("keydown", onKey);
  return { node: grid, dispose: () => { grid.removeEventListener("keydown", onKey); for (const stop of stops.splice(0)) stop(); } };
}

/** Moves the roving focus to the card of `id` (the one a user opened and came back from). */
export function focusKanbanCard(grid: HTMLElement, id: number): boolean {
  const cards = [...grid.querySelectorAll<HTMLElement>(".kanban-card")];
  const card = cards.find(item => item.dataset["id"] === String(id));
  if (!card) return false;
  for (const other of cards) other.tabIndex = other === card ? 0 : -1;
  card.focus({ preventScroll: true });
  return true;
}
