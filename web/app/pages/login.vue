<script setup lang="ts">
const api = useApi();
const me = useMe();
const router = useRouter();

const login = ref("");
const password = ref("");
const error = ref("");
const busy = ref(false);

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
    </UiCard>
  </div>
</template>
