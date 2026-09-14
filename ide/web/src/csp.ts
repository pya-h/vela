// The page's Content-Security-Policy allows `<style>` elements only with the
// nonce the server put in the page. CodeMirror takes the nonce as an option;
// xterm.js has no such option and adds its own style elements. So every style
// element created by the page's scripts gets the nonce. Markup injected as
// HTML still doesn't (it isn't made with `createElement`), and only the page's
// own scripts run (`script-src 'self'`), so the policy still stops injected
// styles.

export function nonceStyleElements(nonce: string): void {
  if (!nonce) return;
  const create = Document.prototype.createElement;
  Document.prototype.createElement = function (this: Document, tagName: string, options?: ElementCreationOptions): HTMLElement {
    const element = create.call(this, tagName, options);
    if (element instanceof HTMLStyleElement) element.nonce = nonce;
    return element;
  } as typeof Document.prototype.createElement;
}
