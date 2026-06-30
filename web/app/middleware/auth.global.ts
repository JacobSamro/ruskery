// Client-side auth gate. Loads the current user once, then redirects between
// the app, the login page, and the first-run setup wizard as appropriate.
export default defineNuxtRouteMiddleware(async (to) => {
  const me = useMe();
  const api = useApi();

  if (!me.value) {
    try {
      me.value = await api.get<Me>("/api/v1/auth/me");
    } catch {
      me.value = null;
    }
  }

  const authed = !!me.value;
  const publicPage = to.path === "/login" || to.path === "/setup";

  if (!authed && !publicPage) {
    const status = await api
      .get<{ needs_setup: boolean }>("/api/v1/setup/status")
      .catch(() => ({ needs_setup: false }));
    return navigateTo(status.needs_setup ? "/setup" : "/login");
  }

  if (authed && publicPage) {
    return navigateTo("/");
  }
});
