/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
/** Clear protected addon tab state when the authenticated shell ends or the sign-in screen opens. */
export function clearProtectedTabState(): void {
  window.dispatchEvent(new Event("erp:session-ended"));
  try {
    const storage = window.sessionStorage;
    for (let i = storage.length - 1; i >= 0; i--) {
      const key = storage.key(i);
      if (key?.startsWith("erp.session.")) storage.removeItem(key);
    }
  } catch (error) {
    // Storage can be completely disabled by the browser; there are no accessible entries to erase.
    if (!(error instanceof DOMException && error.name === "SecurityError")) throw error;
  }
}
