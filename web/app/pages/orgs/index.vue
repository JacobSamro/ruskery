<script setup lang="ts">
// Super-admin view of every organization on the instance.
const me = useMe();
const api = useApi();
const router = useRouter();

interface OrgSummary {
  id: string;
  slug: string;
  name: string;
  created_at: string;
  member_count: number;
  repo_count: number;
}

const orgs = ref<OrgSummary[]>([]);
const loading = ref(true);

const showCreate = ref(false);
const name = ref("");
const slug = ref("");
const error = ref("");
const busy = ref(false);

watch(name, (v) => {
  slug.value = v
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
});

async function load() {
  loading.value = true;
  try {
    orgs.value = (await api.get<{ orgs: OrgSummary[] }>("/api/v1/admin/orgs")).orgs;
  } finally {
    loading.value = false;
  }
}

async function createOrg() {
  error.value = "";
  busy.value = true;
  try {
    await api.post("/api/v1/orgs", { slug: slug.value, name: name.value });
    name.value = "";
    slug.value = "";
    showCreate.value = false;
    me.value = await api.get<Me>("/api/v1/auth/me");
    await load();
  } catch (e) {
    error.value = apiErrorMessage(e);
  } finally {
    busy.value = false;
  }
}

onMounted(() => {
  if (!me.value?.user.is_admin) {
    router.replace("/");
    return;
  }
  load();
});
</script>

<template>
  <div>
    <div class="mb-6 flex items-center justify-between">
      <div>
        <h1 class="text-xl font-semibold tracking-tight">Organizations</h1>
        <p class="text-sm text-[var(--color-muted)]">
          Every organization on this instance.
        </p>
      </div>
      <UiButton @click="showCreate = !showCreate">
        <UiIcon name="plus" :size="14" /> New organization
      </UiButton>
    </div>

    <UiCard v-if="showCreate" title="Create organization" class="mb-6">
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
        <div class="flex gap-2">
          <UiButton type="submit" :disabled="busy">Create</UiButton>
          <UiButton type="button" variant="ghost" @click="showCreate = false">Cancel</UiButton>
        </div>
      </form>
    </UiCard>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-[var(--color-muted)]">
        Loading…
      </div>
      <div
        v-else-if="!orgs.length"
        class="py-8 text-center text-sm text-[var(--color-muted)]"
      >
        No organizations yet.
      </div>
      <ul v-else class="flex flex-col divide-y divide-[var(--color-border)]">
        <li
          v-for="o in orgs"
          :key="o.id"
          class="flex items-center justify-between py-3"
        >
          <div class="min-w-0">
            <NuxtLink
              :to="`/orgs/${o.slug}`"
              class="font-medium hover:underline"
            >
              {{ o.name }}
            </NuxtLink>
            <div class="font-mono text-xs text-[var(--color-muted)]">{{ o.slug }}</div>
          </div>
          <div class="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <UiBadge variant="outline">{{ o.repo_count }} repos</UiBadge>
            <UiBadge variant="outline">{{ o.member_count }} members</UiBadge>
            <NuxtLink :to="`/orgs/${o.slug}`">
              <UiButton variant="ghost" size="sm">Open</UiButton>
            </NuxtLink>
          </div>
        </li>
      </ul>
    </UiCard>
  </div>
</template>
