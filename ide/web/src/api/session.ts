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

export async function obtainToken(
  place: Place,
  history: History,
  store: Store | null,
  fetch: Fetch,
  refused: (reason: string) => void = () => {},
): Promise<string | null> {
  const code = new URLSearchParams(place.hash.replace(/^#/, "")).get("launch");
  if (!code) return takeToken(place, history, store);
  // Strip launch code from the URL bar so it doesn't linger in bookmarks/history.
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
    // Reloading a launch tab hits an expired code; reuse any earlier tab token.
    const earlier = stored(store);
    const reason = field("error");
    if (earlier === null && typeof reason === "string") refused(reason);
    return earlier;
  } catch {
    // Fall back to a stored token if the server didn't respond.
  }
  return stored(store);
}

function keep(store: Store | null, token: string): void {
  try {
    store?.setItem(KEY, token);
  } catch {
    // Private browsing blocks sessionStorage; token still works until reload.
  }
}

function stored(store: Store | null): string | null {
  try {
    return store?.getItem(KEY) ?? null;
  } catch {
    return null;
  }
}

export function takeToken(place: Place, history: History, store: Store | null): string | null {
  const fromAddress = new URLSearchParams(place.hash.replace(/^#/, "")).get("token");
  if (fromAddress) {
    history.replaceState(null, "", place.pathname + place.search);
    keep(store, fromAddress);
    return fromAddress;
  }
  return stored(store);
}
