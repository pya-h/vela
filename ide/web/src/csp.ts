// xterm.js injects `<style>` elements without accepting a CSP nonce option.
export function nonceStyleElements(nonce: string): void {
  if (!nonce) return;
  const create = Document.prototype.createElement;
  Document.prototype.createElement = function (this: Document, tagName: string, options?: ElementCreationOptions): HTMLElement {
    const element = create.call(this, tagName, options);
    if (element instanceof HTMLStyleElement) element.nonce = nonce;
    return element;
  } as typeof Document.prototype.createElement;
}
