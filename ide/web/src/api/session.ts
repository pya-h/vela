// The session token. vela-studio prints the page's address as `/#token=…`, and
// opens the browser with `/#launch=…`, a one-time code the page exchanges for
// the token (a browser's command line is visible to other users of the
// machine, and a used code is worthless). The token is kept in sessionStorage
// (so reloading the tab works) and removed from the address bar, so it doesn't
// end up in bookmarks or screenshots.

const KEY = "vela-studio:token";

interface Place {
  hash: string;
  pathname: string;
  search: string;
}

interface History {
  replaceState(data: unknown, unused: string, url: string): void;
}

interface Store {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

/**
 * The token, from a launch code in the address, the token in the address, or
 * earlier in this tab, in that order. When the server refuses the launch code
 * and no earlier token is kept, `refused` gets its reason.
 */
export async function obtainToken(
  place: Place,
  history: History,
  store: Store | null,
  fetch: Fetch,
  refused: (reason: string) => void = () => {},
): Promise<string | null> {
  const code = new URLSearchParams(place.hash.replace(/^#/, "")).get("launch");
  if (!code) return takeToken(place, history, store);
  history.replaceState(null, "", place.pathname + place.search);
  try {
    const response = await fetch("/api/launch", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code }),
    });
    const body: unknown = await response.json().catch(() => null);
    const field = (name: string): unknown => (typeof body === "object" && body !== null && name in body ? (body as Record<string, unknown>)[name] : null);
    const token = response.ok ? field("token") : null;
    if (typeof token === "string") {
      keep(store, token);
      return token;
    }
    // A used or expired code (a reloaded launch tab): an earlier token may still work.
    const earlier = stored(store);
    const reason = field("error");
    if (earlier === null && typeof reason === "string") refused(reason);
    return earlier;
  } catch {
    // The server isn't answering; fall back to a token from earlier.
  }
  return stored(store);
}

function keep(store: Store | null, token: string): void {
  try {
    store?.setItem(KEY, token);
  } catch {
    // Storage may be unavailable (private mode); the token still works until reload.
  }
}

function stored(store: Store | null): string | null {
  try {
    return store?.getItem(KEY) ?? null;
  } catch {
    return null;
  }
}

/** The token from the address or, failing that, from earlier in this tab. */
export function takeToken(place: Place, history: History, store: Store | null): string | null {
  const fromAddress = new URLSearchParams(place.hash.replace(/^#/, "")).get("token");
  if (fromAddress) {
    history.replaceState(null, "", place.pathname + place.search);
    keep(store, fromAddress);
    return fromAddress;
  }
  return stored(store);
}
