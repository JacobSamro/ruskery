<script setup lang="ts">
const route = useRoute();
const api = useApi();
const slug = computed(() => route.params.slug as string);

const stats = ref<OrgStats | null>(null);
const role = ref<string>("");
const repos = ref<RepoSummary[]>([]);
const loading = ref(true);

const canManage = computed(() => role.value === "owner" || role.value === "admin");

async function load() {
  loading.value = true;
  try {
    const [org, list] = await Promise.all([
      api.get<{ role: string; stats: OrgStats }>(`/api/v1/orgs/${slug.value}`),
      api.get<{ repositories: RepoSummary[] }>(`/api/v1/orgs/${slug.value}/repos`),
    ]);
    role.value = org.role;
    stats.value = org.stats;
    repos.value = list.repositories;
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch(slug, load);

// ── create repository ──
const showCreate = ref(false);
const newName = ref("");
const creating = ref(false);
const createError = ref("");

function openCreate() {
  newName.value = "";
  createError.value = "";
  showCreate.value = true;
}
function closeCreate() {
  showCreate.value = false;
}

async function createRepo() {
  const name = newName.value.trim();
  if (!name || creating.value) return;
  creating.value = true;
  createError.value = "";
  try {
    await api.post(`/api/v1/orgs/${slug.value}/repos`, { name });
    closeCreate();
    await load();
  } catch (e) {
    createError.value = apiErrorMessage(e);
  } finally {
    creating.value = false;
  }
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Repositories</h1>
        <p class="text-sm text-muted-foreground">Container images in this organization.</p>
      </div>
      <div class="flex items-center gap-2">
        <template v-if="stats">
          <UiBadge>{{ stats.repos }} repos</UiBadge>
          <UiBadge>{{ stats.members }} members</UiBadge>
          <UiBadge>{{ stats.teams }} teams</UiBadge>
        </template>
        <UiButton v-if="canManage" size="sm" data-testid="new-repo" @click="openCreate">
          <UiIcon name="plus" :size="16" /> New repository
        </UiButton>
      </div>
    </div>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-muted-foreground">Loading…</div>
      <div v-else-if="!repos.length" class="flex flex-col items-center gap-3 py-12 text-center">
        <UiIcon name="package" :size="32" class="text-muted-foreground" />
        <div>
          <p class="font-medium">No repositories yet</p>
          <p class="mt-1 text-sm text-muted-foreground">
            Create one below, or push your first image:
            <code class="text-foreground">docker push &lt;host&gt;/{{ slug }}/app:latest</code>
          </p>
        </div>
        <UiButton v-if="canManage" size="sm" @click="openCreate">
          <UiIcon name="plus" :size="16" /> New repository
        </UiButton>
      </div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-muted-foreground">
          <tr class="border-b border-border">
            <th class="px-3 py-2 font-medium">Repository</th>
            <th class="px-3 py-2 font-medium">Tags</th>
            <th class="px-3 py-2 font-medium">Updated</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="r in repos"
            :key="r.name"
            class="border-b border-border last:border-0 hover:bg-muted/50"
          >
            <td class="px-3 py-3">
              <NuxtLink
                :to="`/orgs/${slug}/repos/${r.name}`"
                class="flex items-center gap-2 font-medium hover:text-primary"
              >
                <UiIcon name="boxes" :size="16" class="text-muted-foreground" />
                {{ r.name }}
              </NuxtLink>
            </td>
            <td class="px-3 py-3 text-muted-foreground">{{ r.tag_count }}</td>
            <td class="px-3 py-3 text-muted-foreground">{{ timeAgo(r.updated_at) }}</td>
          </tr>
        </tbody>
      </table>
    </UiCard>

    <UiModal :open="showCreate" title="New repository" @close="closeCreate">
      <form class="flex flex-col gap-4" @submit.prevent="createRepo">
        <div>
          <label class="mb-1.5 block text-sm font-medium">Name</label>
          <UiInput v-model="newName" placeholder="team/app" data-testid="new-repo-name" />
          <p class="mt-1.5 text-xs text-muted-foreground">
            Lowercase letters, digits, <code>. _ -</code> and <code>/</code>.
          </p>
        </div>
        <p v-if="createError" class="text-sm text-destructive">{{ createError }}</p>
        <div class="flex justify-end gap-2">
          <UiButton type="button" variant="ghost" @click="closeCreate">Cancel</UiButton>
          <UiButton type="submit" :disabled="creating || !newName.trim()" data-testid="new-repo-submit">
            {{ creating ? "Creating…" : "Create" }}
          </UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
