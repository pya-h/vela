// The session token: vela-studio opens the page as `/#token=…`. The token is
// kept in sessionStorage (so reloading the tab works) and removed from the
// address bar, so it doesn't end up in bookmarks or screenshots.

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

/** The token from the address or, failing that, from earlier in this tab. */
export function takeToken(place: Place, history: History, store: Store | null): string | null {
  const fromAddress = new URLSearchParams(place.hash.replace(/^#/, "")).get("token");
  if (fromAddress) {
    history.replaceState(null, "", place.pathname + place.search);
    try {
      store?.setItem(KEY, fromAddress);
    } catch {
      // Storage may be unavailable (private mode); the token still works until reload.
    }
    return fromAddress;
  }
  try {
    return store?.getItem(KEY) ?? null;
  } catch {
    return null;
  }
}
