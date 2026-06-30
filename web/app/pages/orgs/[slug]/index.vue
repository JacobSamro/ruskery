<script setup lang="ts">
const route = useRoute();
const api = useApi();
const slug = computed(() => route.params.slug as string);

const stats = ref<OrgStats | null>(null);
const repos = ref<RepoSummary[]>([]);
const loading = ref(true);

async function load() {
  loading.value = true;
  try {
    const [org, list] = await Promise.all([
      api.get<{ stats: OrgStats }>(`/api/v1/orgs/${slug.value}`),
      api.get<{ repositories: RepoSummary[] }>(`/api/v1/orgs/${slug.value}/repos`),
    ]);
    stats.value = org.stats;
    repos.value = list.repositories;
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch(slug, load);
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Repositories</h1>
        <p class="text-sm text-[var(--color-muted)]">Container images in this organization.</p>
      </div>
      <div v-if="stats" class="flex gap-2">
        <UiBadge>{{ stats.repos }} repos</UiBadge>
        <UiBadge>{{ stats.members }} members</UiBadge>
        <UiBadge>{{ stats.teams }} teams</UiBadge>
      </div>
    </div>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-[var(--color-muted)]">Loading…</div>
      <div v-else-if="!repos.length" class="flex flex-col items-center gap-3 py-12 text-center">
        <UiIcon name="package" :size="32" class="text-[var(--color-muted)]" />
        <div>
          <p class="font-medium">No repositories yet</p>
          <p class="mt-1 text-sm text-[var(--color-muted)]">
            Push your first image:
            <code class="text-[var(--color-fg)]">docker push &lt;host&gt;/{{ slug }}/app:latest</code>
          </p>
        </div>
      </div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-[var(--color-muted)]">
          <tr class="border-b border-[var(--color-border)]">
            <th class="px-3 py-2 font-medium">Repository</th>
            <th class="px-3 py-2 font-medium">Tags</th>
            <th class="px-3 py-2 font-medium">Updated</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="r in repos"
            :key="r.name"
            class="border-b border-[var(--color-border)] last:border-0 hover:bg-[var(--color-bg)]"
          >
            <td class="px-3 py-3">
              <NuxtLink
                :to="`/orgs/${slug}/repos/${r.name}`"
                class="flex items-center gap-2 font-medium hover:text-[var(--color-primary)]"
              >
                <UiIcon name="boxes" :size="16" class="text-[var(--color-muted)]" />
                {{ r.name }}
              </NuxtLink>
            </td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ r.tag_count }}</td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ timeAgo(r.updated_at) }}</td>
          </tr>
        </tbody>
      </table>
    </UiCard>
  </div>
</template>
