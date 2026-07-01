// Nuxt 4 config — client-rendered SPA, statically generated for embedding in
// the ruskery binary. Tailwind v4 via the official PostCSS plugin.
export default defineNuxtConfig({
  compatibilityDate: "2025-01-01",
  ssr: false,
  devtools: { enabled: false },
  modules: ["@nuxtjs/color-mode"],
  css: ["~/assets/css/main.css"],
  postcss: {
    plugins: {
      "@tailwindcss/postcss": {},
    },
  },
  // shadcn-vue theming: toggle the `dark`/`light` class on <html> (no suffix),
  // default to the OS preference, fall back to dark. The module injects a
  // no-flash init script into index.html; the server's per-HTML CSP hashes it
  // at serve time, so it stays inside the strict script-src.
  colorMode: {
    classSuffix: "",
    preference: "system",
    fallback: "dark",
    storageKey: "ruskery-theme",
  },
  app: {
    head: {
      title: "ruskery",
      meta: [
        { charset: "utf-8" },
        { name: "viewport", content: "width=device-width, initial-scale=1" },
        { name: "description", content: "A fast, private container registry." },
      ],
    },
  },
  // Dev proxy so `nuxt dev` talks to a running ruskery server.
  nitro: {
    devProxy: {
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: true },
      "/v2": { target: "http://127.0.0.1:8080", changeOrigin: true },
    },
  },
});
