// jsdom lacks a few browser APIs the app uses; give them minimal behaviour.

if (!HTMLDialogElement.prototype.showModal) {
  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    this.open = true;
  };
}
if (!HTMLDialogElement.prototype.close) {
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
    if (!this.open) return;
    this.open = false;
    this.dispatchEvent(new Event("close"));
  };
}
for (const name of ["setPointerCapture", "releasePointerCapture"] as const) {
  if (!Element.prototype[name]) Element.prototype[name] = function () {};
}
// CodeMirror measures text ranges for the cursor and selection.
if (!Range.prototype.getClientRects) {
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
}
