<script setup lang="ts">
const me = useMe();
const api = useApi();
const router = useRouter();

const slug = ref("");
const name = ref("");
const error = ref("");
const busy = ref(false);

watch(name, (v) => {
  slug.value = v.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
});

onMounted(() => {
  if (me.value?.orgs.length) router.replace(`/orgs/${me.value.orgs[0].slug}`);
});

async function createOrg() {
  error.value = "";
  busy.value = true;
  try {
    await api.post("/api/v1/orgs", { slug: slug.value, name: name.value });
    me.value = await api.get<Me>("/api/v1/auth/me");
    router.push(`/orgs/${slug.value}`);
  } catch (e) {
    error.value = apiErrorMessage(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="!me?.orgs.length" class="mx-auto max-w-md">
    <UiCard title="Create your first organization" description="Organizations namespace your repositories and own teams.">
      <form class="flex flex-col gap-4" @submit.prevent="createOrg">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Name</label>
          <UiInput v-model="name" placeholder="Acme Inc" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Slug</label>
          <UiInput v-model="slug" placeholder="acme" required />
        </div>
        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
        <UiButton type="submit" :disabled="busy">Create organization</UiButton>
      </form>
    </UiCard>
  </div>
  <div v-else class="text-muted-foreground">Redirecting…</div>
</template>
