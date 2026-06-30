<script setup lang="ts">
const api = useApi();
const me = useMe();
const router = useRouter();

const route = useRoute();
const login = ref("");
const password = ref("");
const error = ref(route.query.error === "oauth" ? "Google sign-in failed or was cancelled." : "");
const busy = ref(false);
const googleEnabled = ref(false);

onMounted(async () => {
  try {
    const p = await api.get<{ google: boolean }>("/api/v1/auth/providers");
    googleEnabled.value = p.google;
  } catch {
    googleEnabled.value = false;
  }
});

function googleSignIn() {
  window.location.href = "/api/v1/auth/google/login";
}

async function submit() {
  error.value = "";
  busy.value = true;
  try {
    await api.post("/api/v1/auth/login", { login: login.value, password: password.value });
    me.value = await api.get<Me>("/api/v1/auth/me");
    await router.push("/");
  } catch (e) {
    error.value = apiErrorMessage(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="w-full max-w-sm">
    <div class="mb-6 flex items-center justify-center gap-2">
      <div class="flex h-9 w-9 items-center justify-center rounded-md bg-[var(--color-primary)] text-[var(--color-primary-fg)]">
        <UiIcon name="grid" :size="20" />
      </div>
      <span class="text-2xl font-semibold tracking-tight">ruskery</span>
    </div>
    <UiCard title="Sign in" description="Access your container registry dashboard.">
      <form class="flex flex-col gap-4" @submit.prevent="submit">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Username or email</label>
          <UiInput v-model="login" placeholder="you@example.com" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Password</label>
          <UiInput v-model="password" type="password" placeholder="••••••••" required />
        </div>
        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
        <UiButton type="submit" :disabled="busy">{{ busy ? "Signing in…" : "Sign in" }}</UiButton>
      </form>

      <template v-if="googleEnabled">
        <div class="my-4 flex items-center gap-3 text-xs text-[var(--color-muted)]">
          <span class="h-px flex-1 bg-[var(--color-border)]" /> or <span class="h-px flex-1 bg-[var(--color-border)]" />
        </div>
        <UiButton variant="outline" class="w-full" @click="googleSignIn">
          <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
            <path fill="#4285F4" d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92a5.06 5.06 0 0 1-2.2 3.32v2.76h3.56c2.08-1.92 3.28-4.74 3.28-8.09z"/>
            <path fill="#34A853" d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.56-2.76c-.98.66-2.23 1.06-3.72 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84A11 11 0 0 0 12 23z"/>
            <path fill="#FBBC05" d="M5.84 14.1a6.6 6.6 0 0 1 0-4.2V7.06H2.18a11 11 0 0 0 0 9.88l3.66-2.84z"/>
            <path fill="#EA4335" d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.06l3.66 2.84C6.71 7.31 9.14 5.38 12 5.38z"/>
          </svg>
          Sign in with Google
        </UiButton>
      </template>
    </UiCard>
  </div>
</template>
