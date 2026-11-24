/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import type { BreadcrumbItem } from "./navigation.js";
import { parseRoute, routeUrl, type Route } from "./route.js";

/** The tab's navigation store: one session-storage key (`erp.session.*`, so sign-out, expiry and the sign-in page erase
 *  it with the rest of the tab's protected state). It holds, per history entry, the breadcrumb trail as it was drawn
 *  (titles and addresses the user saw). What it reads back is untrusted: checked entry by entry, never evaluated, and
 *  every entry is loaded again through the normal, server-checked navigation when the user goes to it. */
export const NAVIGATION_KEY = "erp.session.navigation";
/** Most history entries whose trail is kept; the oldest drop first. */
const MAX_ENTRIES = 64;
/** Largest stored text, in UTF-16 units; a larger value is not read. */
const MAX_STORED = 512 * 1024;
const MAX_TITLE = 512;

interface Stored { readonly id: string; readonly title: string; readonly url: string; readonly form: boolean }

/** A trail entry read back from storage, or `undefined` when it is not one this client writes. */
function crumb(raw: unknown): BreadcrumbItem | undefined {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return undefined;
  const { id, title, url, form } = raw as Record<string, unknown>;
  if (typeof id !== "string" || !/^[A-Za-z0-9:_-]{1,256}$/.test(id)) return undefined;
  if (typeof title !== "string" || !title.trim() || title.length > MAX_TITLE || /[\u0000-\u001f\u007f]/u.test(title)) return undefined;
  if (typeof form !== "boolean" || typeof url !== "string" || !url.startsWith("/") || url.startsWith("//")) return undefined;
  try {
    const route = parseRoute(new URL(url, window.location.origin));
    if (route.action === undefined) return undefined;
    return { id, title, route, url: routeUrl(route), isFormView: form };
  } catch { return undefined; }
}

export class NavigationStore {
  private trails = new Map<number, readonly BreadcrumbItem[]>();
  private failed = false;

  /** `report` hears once when the browser refuses to keep the store (disabled storage, full quota). */
  constructor(private readonly uid: number, private readonly cap: number, private readonly report: (error: unknown) => void,
    private readonly storage: () => Storage = () => window.sessionStorage) {
    let text: string | null = null;
    try { text = this.storage().getItem(NAVIGATION_KEY); } catch (error) { this.refused(error); }
    if (text === null) return;
    let data: unknown;
    try { data = text.length <= MAX_STORED ? JSON.parse(text) : undefined; } catch { data = undefined; }
    const value = typeof data === "object" && data !== null && !Array.isArray(data) ? data as Record<string, unknown> : {};
    // Another user's trail (another sign-in in this tab's session) is never shown.
    if (value["v"] !== 1 || value["uid"] !== uid || !Array.isArray(value["trails"])) { this.write(); return; }
    for (const pair of value["trails"].slice(-MAX_ENTRIES)) {
      if (!Array.isArray(pair) || pair.length !== 2 || !Number.isSafeInteger(pair[0]) || pair[0] < 0 || !Array.isArray(pair[1])) continue;
      const items = (pair[1] as unknown[]).slice(-cap).map(crumb).filter((item): item is BreadcrumbItem => item !== undefined);
      if (items.length) this.trails.set(pair[0] as number, items);
    }
  }

  /** The trail kept for history entry `index`, when its last entry is the page `route` opens (same action, same related
   *  list): a trail kept for another page is not shown with this one. */
  trail(index: number, route: Route): readonly BreadcrumbItem[] | undefined {
    const items = this.trails.get(index), top = items?.at(-1)?.route;
    return top && top.action === route.action && top.method_action === route.method_action ? items : undefined;
  }

  /** Keeps `items` (the trail on screen) for history entry `index`. */
  saveTrail(index: number, items: readonly BreadcrumbItem[]): void {
    this.trails.delete(index);
    this.trails.set(index, items.slice(-this.cap).map(item => ({ id: item.id, title: item.title, route: { ...item.route }, url: item.url, isFormView: item.isFormView === true })));
    while (this.trails.size > MAX_ENTRIES) this.trails.delete(this.trails.keys().next().value!);
    this.write();
  }

  private write(): void {
    const trails = [...this.trails].map(([index, items]) => [index, items.map((item): Stored => ({ id: item.id, title: item.title, url: item.url, form: item.isFormView === true }))]);
    let text = JSON.stringify({ v: 1, uid: this.uid, trails });
    // The oldest entries give way until the store fits its bound.
    while (text.length > MAX_STORED && trails.length > 1) { trails.shift(); this.trails.delete(this.trails.keys().next().value!); text = JSON.stringify({ v: 1, uid: this.uid, trails }); }
    try { this.storage().setItem(NAVIGATION_KEY, text); } catch (error) { this.refused(error); }
  }

  private refused(error: unknown): void {
    if (this.failed) return;
    this.failed = true;
    this.report(error);
  }
}
