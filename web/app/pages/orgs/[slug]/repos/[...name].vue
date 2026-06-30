<script setup lang="ts">
const route = useRoute();
const api = useApi();
const me = useMe();

const slug = computed(() => route.params.slug as string);
const name = computed(() => {
  const n = route.params.name;
  return Array.isArray(n) ? n.join("/") : (n as string);
});

const repo = ref<{ tags: TagDetail[]; pull_prefix: string } | null>(null);
const loading = ref(true);
const copied = ref("");

const canAdmin = computed(() => {
  const role = me.value?.orgs.find((o) => o.slug === slug.value)?.role;
  return role === "owner" || role === "admin";
});

async function load() {
  loading.value = true;
  try {
    repo.value = await api.get(`/api/v1/orgs/${slug.value}/repos/${name.value}`);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

function copy(tag: string) {
  const cmd = `${repo.value?.pull_prefix}:${tag}`;
  navigator.clipboard?.writeText(cmd);
  copied.value = tag;
  setTimeout(() => (copied.value = ""), 1500);
}

const router = useRouter();
async function remove() {
  if (!confirm(`Delete repository "${name.value}" and all its tags?`)) return;
  await api.del(`/api/v1/orgs/${slug.value}/repos/${name.value}`);
  router.push(`/orgs/${slug.value}`);
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-center justify-between">
      <div>
        <NuxtLink :to="`/orgs/${slug}`" class="text-sm text-[var(--color-muted)] hover:text-[var(--color-fg)]">
          ← Repositories
        </NuxtLink>
        <h1 class="mt-1 flex items-center gap-2 text-2xl font-semibold tracking-tight">
          <UiIcon name="boxes" :size="22" /> {{ name }}
        </h1>
      </div>
      <UiButton v-if="canAdmin" variant="destructive" size="sm" @click="remove">
        <UiIcon name="trash" :size="14" /> Delete
      </UiButton>
    </div>

    <UiCard v-if="repo" title="Pull this image">
      <div class="flex items-center justify-between rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 font-mono text-sm">
        <span>docker pull {{ repo.pull_prefix }}:&lt;tag&gt;</span>
      </div>
    </UiCard>

    <UiCard class="mt-6" title="Tags">
      <div v-if="loading" class="py-8 text-center text-sm text-[var(--color-muted)]">Loading…</div>
      <div v-else-if="!repo?.tags.length" class="py-8 text-center text-sm text-[var(--color-muted)]">No tags.</div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-[var(--color-muted)]">
          <tr class="border-b border-[var(--color-border)]">
            <th class="px-3 py-2 font-medium">Tag</th>
            <th class="px-3 py-2 font-medium">Digest</th>
            <th class="px-3 py-2 font-medium">Size</th>
            <th class="px-3 py-2 font-medium">Updated</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="t in repo.tags" :key="t.tag" class="border-b border-[var(--color-border)] last:border-0">
            <td class="px-3 py-3"><UiBadge variant="primary">{{ t.tag }}</UiBadge></td>
            <td class="px-3 py-3 font-mono text-xs text-[var(--color-muted)]">{{ shortDigest(t.digest) }}</td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ formatBytes(t.size) }}</td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ timeAgo(t.updated_at) }}</td>
            <td class="px-3 py-3 text-right">
              <UiButton variant="ghost" size="sm" @click="copy(t.tag)">
                <UiIcon :name="copied === t.tag ? 'check' : 'copy'" :size="14" />
                {{ copied === t.tag ? "Copied" : "Pull cmd" }}
              </UiButton>
            </td>
          </tr>
        </tbody>
      </table>
    </UiCard>
  </div>
</template>
