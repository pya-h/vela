import { defineConfig } from "vite";

// `npm run dev` serves the page with hot reload and forwards the API to a
// running `vela-studio --port $VELA_STUDIO_PORT`. The proxy presents the
// server's own origin, since the server refuses others. Open the dev page
// with the token that vela-studio printed: http://localhost:5173/#token=…
const studioPort = process.env["VELA_STUDIO_PORT"] ?? "4000";
const studio = `http://127.0.0.1:${studioPort}`;

export default defineConfig({
  build: {
    target: "es2022",
    // Fonts and the WebAssembly module stay separate files: the page's
    // Content-Security-Policy doesn't allow data: URLs for them.
    assetsInlineLimit: 0,
    sourcemap: true,
  },
  server: {
    proxy: {
      "/api": {
        target: studio,
        changeOrigin: true,
        ws: true,
        configure: (proxy) => {
          proxy.on("proxyReq", (request) => request.setHeader("origin", studio));
          proxy.on("proxyReqWs", (request) => request.setHeader("origin", studio));
        },
      },
    },
  },
});
