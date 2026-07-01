<script setup lang="ts">
const api = useApi();
const me = useMe();
const router = useRouter();

const form = reactive({
  username: "",
  email: "",
  password: "",
  org_name: "",
  org_slug: "",
});
const error = ref("");
const busy = ref(false);

// Auto-suggest a slug from the org name.
watch(
  () => form.org_name,
  (v) => {
    form.org_slug = v
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40);
  },
);

async function submit() {
  error.value = "";
  busy.value = true;
  try {
    await api.post("/api/v1/setup", { ...form });
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
  <div class="w-full max-w-md">
    <div class="mb-6 flex items-center justify-center gap-2">
      <div class="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
        <UiIcon name="grid" :size="20" />
      </div>
      <span class="text-2xl font-semibold tracking-tight">ruskery</span>
    </div>
    <UiCard title="Welcome — let's set up" description="Create the first admin account and your organization.">
      <form class="flex flex-col gap-4" @submit.prevent="submit">
        <div class="grid grid-cols-2 gap-3">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Username</label>
            <UiInput v-model="form.username" placeholder="admin" required data-testid="setup-username" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Email</label>
            <UiInput v-model="form.email" type="email" placeholder="admin@example.com" required data-testid="setup-email" />
          </div>
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Password</label>
          <UiInput v-model="form.password" type="password" placeholder="at least 8 characters" required data-testid="setup-password" />
        </div>
        <div class="grid grid-cols-2 gap-3">
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Organization</label>
            <UiInput v-model="form.org_name" placeholder="Acme Inc" required data-testid="setup-org-name" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Slug</label>
            <UiInput v-model="form.org_slug" placeholder="acme" required data-testid="setup-org-slug" />
          </div>
        </div>
        <p class="text-xs text-muted-foreground">
          Images will live under <code class="text-foreground">{{ form.org_slug || "acme" }}/&lt;repo&gt;</code>.
        </p>
        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
        <UiButton type="submit" :disabled="busy">{{ busy ? "Creating…" : "Create & continue" }}</UiButton>
      </form>
    </UiCard>
  </div>
</template>
