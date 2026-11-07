/// <reference types="vite/client" />
// Test-only helpers (excluded from coverage and never imported by the app).

import indexHtml from "../../index.html?raw";

/** Replace the document body with index.html's, as the app sees it at startup. */
export function loadIndexHtml() {
  const doc = new DOMParser().parseFromString(indexHtml, "text/html");
  document.documentElement.removeAttribute("style");
  delete document.documentElement.dataset.platform;
  document.body.replaceWith(document.importNode(doc.body, true));
}

export const byId = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

/** Let pending promise callbacks (and zero-delay timers) run. */
export async function flush(times = 5) {
  for (let i = 0; i < times; i++) await new Promise((r) => setTimeout(r, 0));
}

/** A promise plus its resolve/reject, for controlling when a mocked call settles. */
export function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

type Handler = (args?: Record<string, unknown>) => unknown;

/**
 * An `invoke` implementation driven by a table of command handlers. A handler
 * may return a value or a promise, or throw to reject. Unknown commands reject.
 */
export function invokeRouter(handlers: Record<string, Handler>) {
  return async (cmd: string, args?: Record<string, unknown>) => {
    const h = handlers[cmd];
    if (!h) throw new Error(`unexpected invoke: ${cmd}`);
    return h(args);
  };
}
