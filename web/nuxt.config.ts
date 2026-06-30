// Nuxt 4 config — client-rendered SPA, statically generated for embedding in
// the ruskery binary. Tailwind v4 via the official PostCSS plugin.
export default defineNuxtConfig({
  compatibilityDate: "2025-01-01",
  ssr: false,
  devtools: { enabled: false },
  css: ["~/assets/css/main.css"],
  postcss: {
    plugins: {
      "@tailwindcss/postcss": {},
    },
  },
  app: {
    head: {
      title: "ruskery",
      meta: [
        { charset: "utf-8" },
        { name: "viewport", content: "width=device-width, initial-scale=1" },
        { name: "description", content: "A fast, private container registry." },
      ],
      htmlAttrs: { class: "dark" },
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
